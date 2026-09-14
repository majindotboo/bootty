//! Host-neutral input facts captured from GPUI.
//!
//! GPUI 0.2 exposes logical [`gpui_kit::Keystroke`] values, not platform scan codes. It also
//! collapses the left and right instances of every modifier. This adapter therefore cannot
//! identify a physical key or a modifier side; consumers must leave those values unknown rather
//! than infer them from the logical key.

use std::{
    ops::Range,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use gpui_kit::{
    App, Bounds, ExternalPaths, InputHandler, KeyDownEvent, KeyUpEvent, ModifiersChangedEvent,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection, Pixels, Point as GpuiPoint,
    ScrollDelta, ScrollWheelEvent, UTF16Selection, Window,
};

/// A logical key after the active keyboard layout has been applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    Escape,
    Tab,
    Backspace,
    Enter,
    Space,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Copy,
    Cut,
    Paste,
    Colon,
    Comma,
    Backslash,
    Slash,
    Pipe,
    QuestionMark,
    ExclamationMark,
    OpenBracket,
    CloseBracket,
    OpenCurlyBracket,
    CloseCurlyBracket,
    Backtick,
    Minus,
    Period,
    Plus,
    Equals,
    Semicolon,
    Quote,
    Digit(u8),
    Letter(char),
    Function(u8),
    BrowserBack,
}

/// Modifier state without platform-specific aliases such as `command`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Keyboard modifiers can be pressed independently."
)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
    pub function: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelUnit {
    Pixels,
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollPhase {
    Started,
    Moved,
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Key {
        key: Key,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
    },
    ModifiersChanged(Modifiers),
    PointerMoved(Point),
    PointerGone,
    PointerButton {
        position: Point,
        button: PointerButton,
        pressed: bool,
        /// Number of consecutive clicks reported by the platform for this press sequence.
        click_count: usize,
        modifiers: Modifiers,
    },
    MouseWheel {
        unit: WheelUnit,
        delta: Point,
        phase: ScrollPhase,
        modifiers: Modifiers,
    },
    WindowFocused(bool),
    ImePreedit {
        text: String,
        selected_range_utf16: Option<Range<usize>>,
    },
    ImeCommit(String),
}

/// One drain of all input accepted since the previous frame.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameInputSnapshot {
    pub events: Vec<InputEvent>,
    pub dropped_file_paths: Vec<PathBuf>,
    pub modifiers: Modifiers,
    pub hover_position: Option<Point>,
    pub pressed_mouse_button: Option<PointerButton>,
    pub window_focused: bool,
}

#[derive(Default)]
struct SharedInput {
    events: Vec<InputEvent>,
    marked_text: String,
    ime_caret_bounds: Option<Bounds<Pixels>>,
}

/// Accumulates GPUI callbacks until the application drains a frame snapshot.
pub struct InputAccumulator {
    shared: Arc<Mutex<SharedInput>>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    modifiers: Modifiers,
    hover_position: Option<Point>,
    pressed_mouse_button: Option<PointerButton>,
    dropped_file_paths: Vec<PathBuf>,
    window_focused: bool,
}

impl Default for InputAccumulator {
    fn default() -> Self {
        Self {
            shared: Arc::default(),
            wake: None,
            modifiers: Modifiers::default(),
            hover_position: None,
            pressed_mouse_button: None,
            dropped_file_paths: Vec::new(),
            window_focused: true,
        }
    }
}

impl InputAccumulator {
    /// Install the host wake used when GPUI delivers text without a physical key event.
    pub fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = Some(wake);
    }

    #[must_use]
    pub fn ime_handler(&self) -> ImeHandler {
        ImeHandler {
            shared: Arc::clone(&self.shared),
            wake: self.wake.clone(),
        }
    }

    /// Update the screen-space caret rectangle used to place the IME candidate window.
    pub fn set_ime_caret_bounds(&mut self, bounds: Bounds<Pixels>) {
        self.with_shared(|shared| shared.ime_caret_bounds = Some(bounds));
    }

    pub fn key_down(&mut self, event: &KeyDownEvent) {
        self.modifiers = modifiers(event.keystroke.modifiers);
        if let Some(key) = key(&event.keystroke.key) {
            self.push(InputEvent::Key {
                key,
                pressed: true,
                repeat: event.is_held,
                modifiers: self.modifiers,
            });
        }
        // GPUI dispatches `key_char` to the installed `ImeHandler` after key handling. Text must
        // enter this queue only there, or every printable key is delivered twice.
    }

    pub fn key_up(&mut self, event: &KeyUpEvent) {
        self.modifiers = modifiers(event.keystroke.modifiers);
        if let Some(key) = key(&event.keystroke.key) {
            self.push(InputEvent::Key {
                key,
                pressed: false,
                repeat: false,
                modifiers: self.modifiers,
            });
        }
    }

    /// Publish committed text from the focused GPUI input handler.
    pub fn ime_commit(&mut self, text: impl Into<String>) {
        self.push(InputEvent::ImeCommit(text.into()));
    }

    /// Publish the current marked-text composition from the focused GPUI input handler.
    pub fn ime_preedit(
        &mut self,
        text: impl Into<String>,
        selected_range_utf16: Option<Range<usize>>,
    ) {
        self.push(InputEvent::ImePreedit {
            text: text.into(),
            selected_range_utf16,
        });
    }

    pub fn modifiers_changed(&mut self, event: &ModifiersChangedEvent) {
        let next = modifiers(event.modifiers);
        if self.modifiers == next {
            return;
        }
        self.modifiers = next;
        self.push(InputEvent::ModifiersChanged(self.modifiers));
    }

    pub fn mouse_down(&mut self, event: &MouseDownEvent) {
        self.update_pointer(event.position, event.modifiers);
        self.pressed_mouse_button = Some(pointer_button(event.button));
        self.push_pointer_button(
            event.position,
            event.button,
            true,
            event.click_count,
            event.modifiers,
        );
    }

    pub fn mouse_up(&mut self, event: &MouseUpEvent) {
        self.update_pointer(event.position, event.modifiers);
        let released = pointer_button(event.button);
        if self.pressed_mouse_button == Some(released) {
            self.pressed_mouse_button = None;
        }
        self.push_pointer_button(
            event.position,
            event.button,
            false,
            event.click_count,
            event.modifiers,
        );
    }

    pub fn mouse_move(&mut self, event: &MouseMoveEvent) {
        self.update_pointer(event.position, event.modifiers);
        // Prefer GPUI's current held-button snapshot over callback history. This prevents a missed
        // press from turning later motion into a stuck drag.
        self.pressed_mouse_button = event.pressed_button.map(pointer_button);
        self.push(InputEvent::PointerMoved(point(event.position)));
    }

    pub fn pointer_gone(&mut self) {
        self.hover_position = None;
        self.pressed_mouse_button = None;
        self.push(InputEvent::PointerGone);
    }

    pub fn scroll(&mut self, event: &ScrollWheelEvent) {
        self.update_pointer(event.position, event.modifiers);
        let (unit, delta) = match event.delta {
            ScrollDelta::Pixels(delta) => (WheelUnit::Pixels, point(delta)),
            ScrollDelta::Lines(delta) => (
                WheelUnit::Lines,
                Point {
                    x: delta.x,
                    y: delta.y,
                },
            ),
        };
        self.push(InputEvent::MouseWheel {
            unit,
            delta,
            phase: scroll_phase(event.touch_phase),
            modifiers: self.modifiers,
        });
    }

    pub fn window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
        self.push(InputEvent::WindowFocused(focused));
        if !focused {
            self.modifiers = Modifiers::default();
            self.pressed_mouse_button = None;
            self.push(InputEvent::ModifiersChanged(Modifiers::default()));
        }
    }

    pub fn file_drop(&mut self, paths: &ExternalPaths, position: GpuiPoint<Pixels>) {
        self.hover_position = Some(point(position));
        self.dropped_file_paths.extend_from_slice(paths.paths());
    }

    pub fn drain_frame(&mut self) -> FrameInputSnapshot {
        let events = self.with_shared(|shared| std::mem::take(&mut shared.events));
        FrameInputSnapshot {
            events,
            dropped_file_paths: std::mem::take(&mut self.dropped_file_paths),
            modifiers: self.modifiers,
            hover_position: self.hover_position,
            pressed_mouse_button: self.pressed_mouse_button,
            window_focused: self.window_focused,
        }
    }

    fn update_pointer(&mut self, position: GpuiPoint<Pixels>, value: gpui_kit::Modifiers) {
        self.hover_position = Some(point(position));
        self.modifiers = modifiers(value);
    }

    fn push_pointer_button(
        &self,
        position: GpuiPoint<Pixels>,
        button: gpui_kit::MouseButton,
        pressed: bool,
        click_count: usize,
        value: gpui_kit::Modifiers,
    ) {
        self.push(InputEvent::PointerButton {
            position: point(position),
            button: pointer_button(button),
            pressed,
            click_count,
            modifiers: modifiers(value),
        });
    }

    fn push(&self, event: InputEvent) {
        self.with_shared(|shared| shared.events.push(event));
    }

    fn with_shared<T>(&self, update: impl FnOnce(&mut SharedInput) -> T) -> T {
        with_shared(&self.shared, update)
    }
}

/// Append-only terminal text input with composition state managed by GPUI.
#[derive(Clone)]
pub struct ImeHandler {
    shared: Arc<Mutex<SharedInput>>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl InputHandler for ImeHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, _cx: &mut App) -> Option<Range<usize>> {
        with_shared(&self.shared, |shared| {
            (!shared.marked_text.is_empty()).then(|| 0..shared.marked_text.encode_utf16().count())
        })
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        with_shared(&self.shared, |shared| {
            let text = utf16_slice(&shared.marked_text, range_utf16.clone())?;
            *adjusted_range = Some(range_utf16);
            Some(text)
        })
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        _cx: &mut App,
    ) {
        with_shared(&self.shared, |shared| {
            shared.marked_text.clear();
            if !text.is_empty() {
                shared.events.push(InputEvent::ImeCommit(text.to_owned()));
            }
        });
        if let Some(wake) = &self.wake {
            wake();
        } else {
            window.refresh();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        _cx: &mut App,
    ) {
        with_shared(&self.shared, |shared| {
            new_text.clone_into(&mut shared.marked_text);
            shared.events.push(InputEvent::ImePreedit {
                text: new_text.to_owned(),
                selected_range_utf16: new_selected_range,
            });
        });
        if let Some(wake) = &self.wake {
            wake();
        } else {
            window.refresh();
        }
    }

    fn unmark_text(&mut self, window: &mut Window, _cx: &mut App) {
        with_shared(&self.shared, |shared| {
            shared.marked_text.clear();
            shared.events.push(InputEvent::ImePreedit {
                text: String::new(),
                selected_range_utf16: None,
            });
        });
        if let Some(wake) = &self.wake {
            wake();
        } else {
            window.refresh();
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        with_shared(&self.shared, |shared| shared.ime_caret_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: GpuiPoint<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        Some(0)
    }
}

fn with_shared<T>(shared: &Mutex<SharedInput>, update: impl FnOnce(&mut SharedInput) -> T) -> T {
    let mut shared = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    update(&mut shared)
}

const fn modifiers(value: gpui_kit::Modifiers) -> Modifiers {
    Modifiers {
        control: value.control,
        alt: value.alt,
        shift: value.shift,
        platform: value.platform,
        function: value.function,
    }
}

fn point(value: GpuiPoint<Pixels>) -> Point {
    Point {
        x: value.x.into(),
        y: value.y.into(),
    }
}

const fn pointer_button(value: gpui_kit::MouseButton) -> PointerButton {
    match value {
        gpui_kit::MouseButton::Left => PointerButton::Left,
        gpui_kit::MouseButton::Right => PointerButton::Right,
        gpui_kit::MouseButton::Middle => PointerButton::Middle,
        gpui_kit::MouseButton::Navigate(NavigationDirection::Back) => PointerButton::Back,
        gpui_kit::MouseButton::Navigate(NavigationDirection::Forward) => PointerButton::Forward,
    }
}

const fn scroll_phase(value: gpui_kit::TouchPhase) -> ScrollPhase {
    match value {
        gpui_kit::TouchPhase::Started => ScrollPhase::Started,
        gpui_kit::TouchPhase::Moved => ScrollPhase::Moved,
        gpui_kit::TouchPhase::Ended | gpui_kit::TouchPhase::Cancelled => ScrollPhase::Ended,
    }
}

fn key(value: &str) -> Option<Key> {
    let normalized = value.to_ascii_lowercase();
    Some(match normalized.as_str() {
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "up" => Key::ArrowUp,
        "escape" => Key::Escape,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "enter" => Key::Enter,
        "space" | " " => Key::Space,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "copy" => Key::Copy,
        "cut" => Key::Cut,
        "paste" => Key::Paste,
        ":" => Key::Colon,
        "," | "<" => Key::Comma,
        "\\" => Key::Backslash,
        "/" => Key::Slash,
        "|" => Key::Pipe,
        "?" => Key::QuestionMark,
        "!" => Key::ExclamationMark,
        "[" => Key::OpenBracket,
        "]" => Key::CloseBracket,
        "{" => Key::OpenCurlyBracket,
        "}" => Key::CloseCurlyBracket,
        "`" | "~" => Key::Backtick,
        "-" | "_" => Key::Minus,
        "." | ">" => Key::Period,
        "+" => Key::Plus,
        "=" => Key::Equals,
        ";" => Key::Semicolon,
        "'" | "\"" => Key::Quote,
        "back" => Key::BrowserBack,
        value if value.len() == 1 => {
            let value = *value.as_bytes().first()?;
            if value.is_ascii_digit() {
                Key::Digit(value.checked_sub(b'0')?)
            } else if value.is_ascii_lowercase() {
                Key::Letter(char::from(value))
            } else {
                return None;
            }
        }
        value
            if value
                .strip_prefix('f')
                .and_then(|number| number.parse::<u8>().ok())
                .is_some_and(|number| (1..=35).contains(&number)) =>
        {
            Key::Function(value.strip_prefix('f')?.parse().ok()?)
        }
        _ => return None,
    })
}

fn utf16_slice(text: &str, range: Range<usize>) -> Option<String> {
    let utf16 = text.encode_utf16().collect::<Vec<_>>();
    String::from_utf16(utf16.get(range)?).ok()
}
