//! Linux: AT-SPI semantic actions, plus EWMH capture and `XTest` input for X11 windows.
//!
//! Deliberate limit: `SendEvent` reaches background windows, but marks events as synthetic and
//! many toolkits ignore them. Address input to the window that way if those toolkits accept it.
//!
//! Native Wayland targets are semantic-only. Their toolkit may not know the global origin.
//! Deliberate limit: portal capture/input need a user-selected sharing session, which cannot yet
//! be bound to our exact window token. Add that binding before enabling them for these targets.
mod accessibility;
use std::sync::{Arc, Mutex, PoisonError};

use x11rb::{
    connection::Connection as _,
    protocol::{
        xproto::{
            Atom, AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ClientMessageEvent,
            ConnectionExt as _, EventMask, ImageFormat, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
            Keycode, MOTION_NOTIFY_EVENT, MapState, Window,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

use crate::{
    ComputerElement, ComputerError, ComputerStatus, ComputerTarget, DisplayBounds, Key, Modifier,
    MouseButton, PermissionStatus,
    desktop::{Desktop, Image, InputEvent, InputRoute, NativeWindow, pixel_count},
};

// About 40 pixels of scrolling is one wheel click.
const PIXELS_PER_WHEEL_CLICK: i32 = 40;
const KEYSYM_RETURN: u32 = 0xff0d;
const KEYSYM_TAB: u32 = 0xff09;
const KEYSYM_SHIFT: u32 = 0xffe1;
const KEYSYM_CONTROL: u32 = 0xffe3;
const KEYSYM_ALT: u32 = 0xffe9;

pub fn native() -> Arc<dyn Desktop> {
    Arc::new(LinuxDesktop {
        display: x11rb::connect(None).ok().and_then(|(connection, screen)| {
            let root = connection.setup().roots.get(screen)?.root;
            Some(Mutex::new(Display { connection, root }))
        }),
    })
}

struct LinuxDesktop {
    display: Option<Mutex<Display>>,
}

struct Display {
    connection: RustConnection,
    root: Window,
}

impl LinuxDesktop {
    fn with<T>(&self, work: impl FnOnce(&Display) -> Option<T>) -> Option<T> {
        let display = self.display.as_ref()?;
        let display = display.lock().unwrap_or_else(PoisonError::into_inner);
        work(&display)
    }
}

impl Desktop for LinuxDesktop {
    fn status(&self) -> ComputerStatus {
        // X11 has no capture/input permission. AT-SPI can also work without an X display.
        let status = if self.display.is_some() {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Unsupported
        };
        ComputerStatus {
            accessibility: if accessibility::available() {
                PermissionStatus::Granted
            } else {
                status
            },
            screen_recording: status,
            secure_input: false,
        }
    }

    fn windows(&self) -> Vec<NativeWindow> {
        let windows: Vec<NativeWindow> = self
            .with(|display| {
                let stacking = display.cardinals(display.root, "_NET_CLIENT_LIST_STACKING")?;
                // The stacking list runs bottom to top.
                Some(
                    stacking
                        .into_iter()
                        .rev()
                        .filter_map(|window| display.native_window(window))
                        .collect(),
                )
            })
            .unwrap_or_default();
        // AT-SPI does not provide an XID. Keep the two identities separate instead of guessing
        // that windows with the same PID, title or geometry are the same native window.
        windows
            .into_iter()
            .chain(accessibility::windows())
            .collect()
    }

    fn route(&self) -> InputRoute {
        InputRoute::Shared
    }

    fn frontmost(&self) -> Option<i32> {
        self.with(|display| display.active().map(|(owner, _)| owner))
    }

    /// Keys go to the active window, so a process in the background receives none.
    fn focus(&self, process_id: i32) -> Option<DisplayBounds> {
        self.with(|display| {
            let (owner, window) = display.active()?;
            (owner == process_id)
                .then(|| display.bounds(window))
                .flatten()
        })
    }

    fn secure_input(&self) -> bool {
        false
    }

    fn capture(
        &self,
        window: &NativeWindow,
        width: u32,
        height: u32,
        _cursor: bool,
    ) -> Result<Image, ComputerError> {
        require_x11(window)?;
        self.with(|display| {
            let (native_width, native_height) = (
                u16::try_from(pixel_count(window.bounds.width)).ok()?,
                u16::try_from(pixel_count(window.bounds.height)).ok()?,
            );
            let image = display
                .connection
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    window.id,
                    0,
                    0,
                    native_width,
                    native_height,
                    !0,
                )
                .ok()?
                .reply()
                .ok()?;
            // 24- and 32-bit visuals store BGRX pixels in 32 bits.
            let bytes = usize::from(native_width)
                .checked_mul(usize::from(native_height))?
                .checked_mul(4)?;
            let rgba = image
                .data
                .get(..bytes)?
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|[blue, green, red, _]| [*red, *green, *blue, u8::MAX])
                .collect();
            Some(
                Image {
                    width: u32::from(native_width),
                    height: u32::from(native_height),
                    rgba,
                }
                .scaled(width, height),
            )
        })
        .ok_or_else(|| ComputerError::Platform("window capture failed".into()))
    }

    fn post(&self, window: &NativeWindow, event: &InputEvent) -> Result<(), ComputerError> {
        require_x11(window)?;
        self.with(|display| display.post(event).ok())
            .ok_or_else(|| ComputerError::Platform("X11 rejected the input".into()))
    }

    fn raise(&self, window: &NativeWindow) -> Result<(), ComputerError> {
        require_x11(window)?;
        self.with(|display| {
            let active = display.atom("_NET_ACTIVE_WINDOW")?;
            // Source 2 marks a pager-style request, which window managers honor.
            let event = ClientMessageEvent::new(32, window.id, active, [2, 0, 0, 0, 0]);
            display
                .connection
                .send_event(
                    false,
                    display.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    event,
                )
                .ok()?;
            display.connection.flush().ok()
        })
        .ok_or(ComputerError::TargetNotFocused)
    }

    fn icon(&self, bundle_id: &str, side: u32) -> Option<Image> {
        self.with(|display| {
            let stacking = display.cardinals(display.root, "_NET_CLIENT_LIST_STACKING")?;
            let window = stacking
                .into_iter()
                .find(|window| display.class(*window).as_deref() == Some(bundle_id))?;
            icon(&display.cardinals(window, "_NET_WM_ICON")?, side)
        })
    }

    fn elements(
        &self,
        window: &NativeWindow,
        limit: usize,
    ) -> Result<(Vec<ComputerElement>, bool), ComputerError> {
        accessibility::elements(window, limit)
    }

    fn element(
        &self,
        window: &NativeWindow,
        path: &[u16],
    ) -> Result<Option<ComputerElement>, ComputerError> {
        accessibility::element(window, path)
    }

    fn press(&self, window: &NativeWindow, path: &[u16]) -> Result<bool, ComputerError> {
        accessibility::press(window, path)
    }

    fn set_value(
        &self,
        window: &NativeWindow,
        path: &[u16],
        value: &str,
    ) -> Result<(), ComputerError> {
        accessibility::set_value(window, path, value)
    }
}

fn require_x11(window: &NativeWindow) -> Result<(), ComputerError> {
    if window
        .app
        .as_ref()
        .is_some_and(|app| app.native_id.is_some())
    {
        Err(ComputerError::Unsupported)
    } else {
        Ok(())
    }
}

impl Display {
    fn atom(&self, name: &str) -> Option<Atom> {
        Some(
            self.connection
                .intern_atom(false, name.as_bytes())
                .ok()?
                .reply()
                .ok()?
                .atom,
        )
    }

    fn property(&self, window: Window, name: &str) -> Option<Vec<u8>> {
        let atom = self.atom(name)?;
        let reply = self
            .connection
            .get_property(false, window, atom, AtomEnum::ANY, 0, u32::MAX / 4)
            .ok()?
            .reply()
            .ok()?;
        (reply.format != 0).then_some(reply.value)
    }

    fn cardinals(&self, window: Window, name: &str) -> Option<Vec<u32>> {
        let atom = self.atom(name)?;
        let reply = self
            .connection
            .get_property(false, window, atom, AtomEnum::ANY, 0, u32::MAX / 4)
            .ok()?
            .reply()
            .ok()?;
        Some(reply.value32()?.collect())
    }

    /// The `WM_CLASS` class name identifies the app, as a bundle id does on macOS.
    fn class(&self, window: Window) -> Option<String> {
        let value = self.property(window, "WM_CLASS")?;
        let mut parts = value
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty());
        let instance = parts.next();
        let class = parts.next().or(instance)?;
        Some(String::from_utf8_lossy(class).into_owned())
    }

    /// The active window and its process.
    fn active(&self) -> Option<(i32, Window)> {
        let window = *self.cardinals(self.root, "_NET_ACTIVE_WINDOW")?.first()?;
        let owner = i32::try_from(*self.cardinals(window, "_NET_WM_PID")?.first()?).ok()?;
        Some((owner, window))
    }

    fn bounds(&self, window: Window) -> Option<DisplayBounds> {
        let geometry = self.connection.get_geometry(window).ok()?.reply().ok()?;
        let origin = self
            .connection
            .translate_coordinates(window, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        (geometry.width > 0 && geometry.height > 0).then_some(DisplayBounds {
            x: f64::from(origin.dst_x),
            y: f64::from(origin.dst_y),
            width: f64::from(geometry.width),
            height: f64::from(geometry.height),
        })
    }

    fn native_window(&self, window: Window) -> Option<NativeWindow> {
        let attributes = self
            .connection
            .get_window_attributes(window)
            .ok()?
            .reply()
            .ok()?;
        if attributes.map_state != MapState::VIEWABLE {
            return None;
        }
        let bounds = self.bounds(window)?;
        let app = self.application(window, &bounds);
        Some(NativeWindow {
            id: window,
            bounds,
            visible: true,
            app,
        })
    }

    fn application(&self, window: Window, bounds: &DisplayBounds) -> Option<ComputerTarget> {
        let process_id = i32::try_from(*self.cardinals(window, "_NET_WM_PID")?.first()?).ok()?;
        let bundle_id = self.class(window)?;
        let title = self
            .property(window, "_NET_WM_NAME")
            .or_else(|| self.property(window, "WM_NAME"))
            .map(|title| String::from_utf8_lossy(&title).chars().take(512).collect());
        Some(ComputerTarget {
            window_id: window,
            native_id: None,
            process_id,
            application_name: Some(bundle_id.chars().take(256).collect()),
            bundle_id,
            launch_time: start_time(process_id)?,
            bounds: bounds.clone(),
            title,
        })
    }

    fn post(&self, event: &InputEvent) -> Result<(), ComputerError> {
        match event {
            InputEvent::Move { x, y } => self.pointer(*x, *y)?,
            InputEvent::Click { x, y, button } => {
                self.pointer(*x, *y)?;
                let button = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                };
                self.button(button, 1)?;
            }
            InputEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                self.pointer(*x, *y)?;
                // Buttons 4 and 5 scroll up and down; 6 and 7 scroll left and right.
                let clicks = |delta: i32| {
                    delta
                        .unsigned_abs()
                        .checked_div(PIXELS_PER_WHEEL_CLICK.unsigned_abs())
                        .unwrap_or(1)
                        .max(1)
                };
                if *delta_y != 0 {
                    self.button(if *delta_y > 0 { 4 } else { 5 }, clicks(*delta_y))?;
                }
                if *delta_x != 0 {
                    self.button(if *delta_x > 0 { 6 } else { 7 }, clicks(*delta_x))?;
                }
            }
            InputEvent::Text(text) => {
                for character in text.chars() {
                    self.type_keysym(keysym(character), &[])?;
                }
            }
            InputEvent::Key { key, modifiers } => {
                let held: Vec<u32> = modifiers
                    .iter()
                    .map(|modifier| match modifier {
                        // Mac-style shortcuts name Command; X11 apps use Control for them.
                        Modifier::Command | Modifier::Control => KEYSYM_CONTROL,
                        Modifier::Option => KEYSYM_ALT,
                        Modifier::Shift => KEYSYM_SHIFT,
                    })
                    .collect();
                self.type_keysym(key_keysym(*key), &held)?;
            }
        }
        self.connection.sync().map_err(|_| rejected())
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), ComputerError> {
        self.connection
            .xtest_fake_input(kind, detail, 0, self.root, x, y, 0)
            .map(drop)
            .map_err(|_| ComputerError::Platform("XTest is unavailable".into()))
    }

    fn pointer(&self, x: f64, y: f64) -> Result<(), ComputerError> {
        self.fake(MOTION_NOTIFY_EVENT, 0, coordinate(x), coordinate(y))
    }

    fn button(&self, button: u8, clicks: u32) -> Result<(), ComputerError> {
        for _ in 0..clicks {
            self.fake(BUTTON_PRESS_EVENT, button, 0, 0)?;
            self.fake(BUTTON_RELEASE_EVENT, button, 0, 0)?;
        }
        Ok(())
    }

    /// Press a keysym with modifiers held. A keysym missing from the keyboard map is bound to a
    /// spare keycode for the press, then unbound.
    fn type_keysym(&self, keysym: u32, held: &[u32]) -> Result<(), ComputerError> {
        let setup = self.connection.setup();
        let (first, last) = (setup.min_keycode, setup.max_keycode);
        let map = self
            .connection
            .get_keyboard_mapping(first, last.saturating_sub(first).saturating_add(1))
            .map_err(|_| keyboard_unavailable())?
            .reply()
            .map_err(|_| keyboard_unavailable())?;
        let per = usize::from(map.keysyms_per_keycode);
        let lookup = |wanted: u32| -> Option<(Keycode, bool)> {
            map.keysyms
                .chunks(per)
                .enumerate()
                .find_map(|(index, syms)| {
                    let code = first.checked_add(u8::try_from(index).ok()?)?;
                    if syms.first() == Some(&wanted) {
                        Some((code, false))
                    } else if syms.get(1) == Some(&wanted) {
                        Some((code, true))
                    } else {
                        None
                    }
                })
        };
        let mut modifiers: Vec<Keycode> = held
            .iter()
            .filter_map(|keysym| lookup(*keysym).map(|(code, _)| code))
            .collect();
        let (code, spare) = if let Some((code, shifted)) = lookup(keysym) {
            if shifted && let Some((shift, _)) = lookup(KEYSYM_SHIFT) {
                modifiers.push(shift);
            }
            (code, None)
        } else {
            let spare = map
                .keysyms
                .chunks(per)
                .rposition(|syms| syms.iter().all(|sym| *sym == 0))
                .and_then(|index| first.checked_add(u8::try_from(index).ok()?))
                .ok_or_else(|| ComputerError::Platform("no spare X11 keycode".into()))?;
            self.connection
                .change_keyboard_mapping(1, spare, map.keysyms_per_keycode, &vec![keysym; per])
                .map_err(|_| keyboard_unavailable())?;
            self.connection.sync().map_err(|_| keyboard_unavailable())?;
            (spare, Some(spare))
        };
        for modifier in &modifiers {
            self.fake(KEY_PRESS_EVENT, *modifier, 0, 0)?;
        }
        self.fake(KEY_PRESS_EVENT, code, 0, 0)?;
        self.fake(KEY_RELEASE_EVENT, code, 0, 0)?;
        for modifier in modifiers.iter().rev() {
            self.fake(KEY_RELEASE_EVENT, *modifier, 0, 0)?;
        }
        if let Some(spare) = spare {
            self.connection.sync().map_err(|_| keyboard_unavailable())?;
            self.connection
                .change_keyboard_mapping(1, spare, map.keysyms_per_keycode, &vec![0; per])
                .map_err(|_| keyboard_unavailable())?;
        }
        Ok(())
    }
}

fn rejected() -> ComputerError {
    ComputerError::Platform("X11 rejected the input".into())
}

fn keyboard_unavailable() -> ComputerError {
    ComputerError::Platform("X11 keyboard map is unavailable".into())
}

/// Process start time in clock ticks since boot; it identifies this process incarnation.
fn start_time(process_id: i32) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{process_id}/stat")).ok()?;
    // Fields after the parenthesized command name start at field 3; start time is field 22.
    let fields = stat.rsplit_once(')')?.1;
    fields.split_whitespace().nth(19)?.parse::<f64>().ok()
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "Float-to-integer casts saturate; points are clamped to the X11 coordinate range."
)]
const fn coordinate(value: f64) -> i16 {
    value.round().clamp(-32768.0, 32767.0) as i16
}

/// Pick the smallest `_NET_WM_ICON` image at least `side` wide, or the largest one.
fn icon(data: &[u32], side: u32) -> Option<Image> {
    let mut images = Vec::new();
    let mut rest = data;
    while let [width, height, pixels @ ..] = rest {
        let count = usize::try_from(*width)
            .ok()?
            .checked_mul(usize::try_from(*height).ok()?)?;
        let (Some(image), Some(next)) = (pixels.get(..count), pixels.get(count..)) else {
            break;
        };
        if count == 0 {
            break;
        }
        images.push((*width, *height, image));
        rest = next;
    }
    let (width, height, pixels) = images
        .iter()
        .filter(|(width, _, _)| *width >= side)
        .min_by_key(|(width, _, _)| *width)
        .or_else(|| images.iter().max_by_key(|(width, _, _)| *width))
        .copied()?;
    // Pixels are ARGB in native-endian cardinals.
    let rgba = pixels
        .iter()
        .flat_map(|pixel| {
            let [alpha, red, green, blue] = pixel.to_be_bytes();
            [red, green, blue, alpha]
        })
        .collect();
    Some(
        Image {
            width,
            height,
            rgba,
        }
        .scaled(side, side),
    )
}

/// Latin-1 characters are their own keysyms; other characters use Unicode keysyms.
fn keysym(character: char) -> u32 {
    match character {
        '\n' => KEYSYM_RETURN,
        '\t' => KEYSYM_TAB,
        ' '..='~' | '\u{a0}'..='\u{ff}' => u32::from(character),
        other => 0x0100_0000 | u32::from(other),
    }
}

const fn key_keysym(key: Key) -> u32 {
    match key {
        Key::Return => KEYSYM_RETURN,
        Key::Tab => KEYSYM_TAB,
        Key::Escape => 0xff1b,
        Key::Space => 0x20,
        Key::Backspace => 0xff08,
        Key::Delete => 0xffff,
        Key::Left => 0xff51,
        Key::Up => 0xff52,
        Key::Right => 0xff53,
        Key::Down => 0xff54,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::A => 0x61,
        Key::B => 0x62,
        Key::C => 0x63,
        Key::D => 0x64,
        Key::E => 0x65,
        Key::F => 0x66,
        Key::G => 0x67,
        Key::H => 0x68,
        Key::I => 0x69,
        Key::J => 0x6a,
        Key::K => 0x6b,
        Key::L => 0x6c,
        Key::M => 0x6d,
        Key::N => 0x6e,
        Key::O => 0x6f,
        Key::P => 0x70,
        Key::Q => 0x71,
        Key::R => 0x72,
        Key::S => 0x73,
        Key::T => 0x74,
        Key::U => 0x75,
        Key::V => 0x76,
        Key::W => 0x77,
        Key::X => 0x78,
        Key::Y => 0x79,
        Key::Z => 0x7a,
        Key::Digit0 => 0x30,
        Key::Digit1 => 0x31,
        Key::Digit2 => 0x32,
        Key::Digit3 => 0x33,
        Key::Digit4 => 0x34,
        Key::Digit5 => 0x35,
        Key::Digit6 => 0x36,
        Key::Digit7 => 0x37,
        Key::Digit8 => 0x38,
        Key::Digit9 => 0x39,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
        Key::F13 => 0xffca,
        Key::F14 => 0xffcb,
        Key::F15 => 0xffcc,
        Key::F16 => 0xffcd,
        Key::F17 => 0xffce,
        Key::F18 => 0xffcf,
        Key::F19 => 0xffd0,
        Key::F20 => 0xffd1,
        Key::Minus => 0x2d,
        Key::Equal => 0x3d,
        Key::LeftBracket => 0x5b,
        Key::RightBracket => 0x5d,
        Key::Backslash => 0x5c,
        Key::Semicolon => 0x3b,
        Key::Quote => 0x27,
        Key::Comma => 0x2c,
        Key::Period => 0x2e,
        Key::Slash => 0x2f,
        Key::Backtick => 0x60,
    }
}
