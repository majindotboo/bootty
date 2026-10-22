//! Windows: window enumeration, `PrintWindow` capture, UIA element actions and `SendInput`.
//! UIA actions leave automatic focus disabled. Pointer and key input need the window in front.
//!
//! Deliberate limit: window messages such as `WM_LBUTTONDOWN` reach background windows, but apps
//! that read raw input or the real key state ignore them. Address input to the window that way if
//! the apps agents drive turn out to honor it.
//! Coordinates are physical pixels; Bootty runs per-monitor DPI aware.
#![allow(
    unsafe_code,
    reason = "Win32 window, GDI, process and input calls are confined to this native adapter."
)]

use std::{ffi::c_void, sync::Arc};

mod accessibility;

use windows::{
    Win32::{
        Foundation::{CloseHandle, FILETIME, HWND, LPARAM, RECT},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
            Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
                DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HGDIOBJ, ReleaseDC, SelectObject,
            },
        },
        Storage::{
            FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
            Xps::{PRINT_WINDOW_FLAGS, PrintWindow},
        },
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::{
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
                MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
                MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
                MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
                MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL,
                VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT,
                VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA,
                VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT,
                VK_SPACE, VK_TAB, VK_UP,
            },
            Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW},
            WindowsAndMessaging::{
                DI_NORMAL, DestroyIcon, DrawIconEx, EnumWindows, GW_OWNER, GWL_EXSTYLE,
                GetForegroundWindow, GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect,
                GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
                SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
                SW_RESTORE, SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::{BOOL, PCWSTR, PWSTR},
};

use crate::{
    ComputerElement, ComputerError, ComputerStatus, ComputerTarget, DisplayBounds, Key, Modifier,
    MouseButton, PermissionStatus,
    desktop::{Desktop, Image, InputEvent, InputRoute, NativeWindow, pixel_count},
};

// PrintWindow renders DirectComposition content, such as browsers, only with this flag.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);
// One wheel notch is 120 units; agents scroll in pixels, about 40 per notch.
const WHEEL_UNITS_PER_PIXEL: i32 = 3;

pub fn native() -> Arc<dyn Desktop> {
    Arc::new(WindowsDesktop)
}

struct WindowsDesktop;

impl Desktop for WindowsDesktop {
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

    fn status(&self) -> ComputerStatus {
        // Windows has no capture or input permission for desktop apps.
        ComputerStatus {
            accessibility: PermissionStatus::Granted,
            screen_recording: PermissionStatus::Granted,
            secure_input: false,
        }
    }

    fn windows(&self) -> Vec<NativeWindow> {
        unsafe extern "system" fn collect(window: HWND, list: LPARAM) -> BOOL {
            let list = std::ptr::with_exposed_provenance_mut::<Vec<HWND>>(list.0.cast_unsigned());
            let list = unsafe { &mut *list };
            list.push(window);
            BOOL::from(list.len() < 4096)
        }
        let mut handles: Vec<HWND> = Vec::new();
        let list = LPARAM(
            std::ptr::from_mut(&mut handles)
                .expose_provenance()
                .cast_signed(),
        );
        // EnumWindows walks top-level windows in z-order, front to back.
        _ = unsafe { EnumWindows(Some(collect), list) };
        handles.into_iter().filter_map(native_window).collect()
    }

    fn route(&self) -> InputRoute {
        InputRoute::Shared
    }

    fn frontmost(&self) -> Option<i32> {
        foreground().map(|(owner, _)| owner)
    }

    /// Keys go to the foreground window, so a process in the background receives none.
    fn focus(&self, process_id: i32) -> Option<DisplayBounds> {
        let (owner, window) = foreground()?;
        (owner == process_id).then(|| bounds(window)).flatten()
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
        let handle = hwnd(window.id);
        let (native_width, native_height) = (
            pixel_count(window.bounds.width),
            pixel_count(window.bounds.height),
        );
        let screen = unsafe { GetDC(None) };
        let memory = unsafe { CreateCompatibleDC(Some(screen)) };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let info = bitmap_info(native_width, native_height);
        let bitmap = unsafe {
            CreateDIBSection(
                Some(memory),
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            )
        };
        let result = bitmap
            .map_err(|_| ComputerError::Platform("could not allocate a capture buffer".into()))
            .and_then(|bitmap| {
                let previous = unsafe { SelectObject(memory, HGDIOBJ(bitmap.0)) };
                let printed =
                    unsafe { PrintWindow(handle, memory, PW_RENDERFULLCONTENT) }.as_bool();
                let pixels = printed
                    .then(|| unsafe { dib_pixels(bits, native_width, native_height) })
                    .flatten();
                unsafe {
                    SelectObject(memory, previous);
                    _ = DeleteObject(HGDIOBJ(bitmap.0));
                }
                pixels.ok_or_else(|| ComputerError::Platform("window capture failed".into()))
            });
        unsafe {
            _ = DeleteDC(memory);
            ReleaseDC(None, screen);
        }
        Ok(Image {
            width: native_width,
            height: native_height,
            rgba: result?,
        }
        .scaled(width, height))
    }

    fn post(&self, _window: &NativeWindow, event: &InputEvent) -> Result<(), ComputerError> {
        let inputs = match event {
            InputEvent::Move { x, y } => vec![mouse_at(*x, *y, MOUSE_EVENT_FLAGS(0), 0)],
            InputEvent::Click { x, y, button } => {
                let (down, up) = match button {
                    MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                    MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                    MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                };
                vec![
                    mouse_at(*x, *y, MOUSE_EVENT_FLAGS(0), 0),
                    mouse_at(*x, *y, down, 0),
                    mouse_at(*x, *y, up, 0),
                ]
            }
            InputEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                let mut inputs = vec![mouse_at(*x, *y, MOUSE_EVENT_FLAGS(0), 0)];
                if *delta_y != 0 {
                    inputs.push(mouse_at(
                        *x,
                        *y,
                        MOUSEEVENTF_WHEEL,
                        delta_y.saturating_mul(WHEEL_UNITS_PER_PIXEL),
                    ));
                }
                if *delta_x != 0 {
                    inputs.push(mouse_at(
                        *x,
                        *y,
                        MOUSEEVENTF_HWHEEL,
                        delta_x.saturating_mul(WHEEL_UNITS_PER_PIXEL),
                    ));
                }
                inputs
            }
            InputEvent::Text(text) => text
                .encode_utf16()
                .flat_map(|unit| {
                    [
                        key_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
                        key_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                    ]
                })
                .collect(),
            InputEvent::Key { key, modifiers } => {
                let held: Vec<VIRTUAL_KEY> = modifiers
                    .iter()
                    .map(|modifier| modifier_key(*modifier))
                    .collect();
                let mut inputs: Vec<INPUT> = held
                    .iter()
                    .map(|key| key_input(*key, 0, KEYBD_EVENT_FLAGS(0)))
                    .collect();
                let key = virtual_key(*key);
                inputs.push(key_input(key, 0, KEYBD_EVENT_FLAGS(0)));
                inputs.push(key_input(key, 0, KEYEVENTF_KEYUP));
                inputs.extend(
                    held.iter()
                        .rev()
                        .map(|key| key_input(*key, 0, KEYEVENTF_KEYUP)),
                );
                inputs
            }
        };
        let size = i32::try_from(std::mem::size_of::<INPUT>()).unwrap_or(i32::MAX);
        let sent = unsafe { SendInput(&inputs, size) };
        if usize::try_from(sent).ok() == Some(inputs.len()) {
            Ok(())
        } else {
            Err(ComputerError::Platform("Windows blocked the input".into()))
        }
    }

    fn raise(&self, window: &NativeWindow) -> Result<(), ComputerError> {
        let handle = hwnd(window.id);
        unsafe {
            if IsIconic(handle).as_bool() {
                _ = ShowWindow(handle, SW_RESTORE);
            }
            if SetForegroundWindow(handle).as_bool() {
                Ok(())
            } else {
                Err(ComputerError::TargetNotFocused)
            }
        }
    }

    fn icon(&self, bundle_id: &str, side: u32) -> Option<Image> {
        let path: Vec<u16> = bundle_id.encode_utf16().chain(Some(0)).collect();
        let mut info = SHFILEINFOW::default();
        let found = unsafe {
            SHGetFileInfoW(
                PCWSTR(path.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&raw mut info),
                u32::try_from(std::mem::size_of::<SHFILEINFOW>()).ok()?,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if found == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let screen = unsafe { GetDC(None) };
        let memory = unsafe { CreateCompatibleDC(Some(screen)) };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap_info = bitmap_info(side, side);
        let pixels = unsafe {
            CreateDIBSection(
                Some(memory),
                &raw const bitmap_info,
                DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            )
        }
        .ok()
        .and_then(|bitmap| {
            let previous = unsafe { SelectObject(memory, HGDIOBJ(bitmap.0)) };
            let edge = i32::try_from(side).unwrap_or(32);
            let drawn =
                unsafe { DrawIconEx(memory, 0, 0, info.hIcon, edge, edge, 0, None, DI_NORMAL) }
                    .is_ok();
            let pixels = drawn
                .then(|| unsafe { dib_pixels(bits, side, side) })
                .flatten();
            unsafe {
                SelectObject(memory, previous);
                _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            pixels
        });
        unsafe {
            _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            _ = DestroyIcon(info.hIcon);
        }
        Some(Image {
            width: side,
            height: side,
            rgba: pixels?,
        })
    }
}

fn hwnd(id: u32) -> HWND {
    // Window handles are 32-bit values, even in 64-bit processes.
    HWND(std::ptr::with_exposed_provenance_mut(
        usize::try_from(id).unwrap_or_default(),
    ))
}

/// Copy a top-down 32-bit DIB section into RGBA pixels.
///
/// # Safety
/// `bits` must be null or point to `width * height` BGRA pixels.
unsafe fn dib_pixels(bits: *mut c_void, width: u32, height: u32) -> Option<Vec<u8>> {
    if bits.is_null() {
        return None;
    }
    let length = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    let bgra = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), length) };
    Some(bgra_to_rgba(bgra))
}

fn native_window(window: HWND) -> Option<NativeWindow> {
    if !unsafe { IsWindowVisible(window) }.as_bool() {
        return None;
    }
    let mut cloaked: u32 = 0;
    let cloak_size = u32::try_from(std::mem::size_of::<u32>()).ok()?;
    let is_cloaked = unsafe {
        DwmGetWindowAttribute(window, DWMWA_CLOAKED, (&raw mut cloaked).cast(), cloak_size)
    }
    .is_ok()
        && cloaked != 0;
    if is_cloaked {
        return None;
    }
    let bounds = bounds(window)?;
    let id = u32::try_from(window.0.expose_provenance()).ok()?;
    let style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
    let owned = unsafe { GetWindow(window, GW_OWNER) }.is_ok_and(|owner| !owner.is_invalid());
    let tool = style & isize::try_from(WS_EX_TOOLWINDOW.0).unwrap_or_default() != 0;
    let app = (!tool && !owned && !unsafe { IsIconic(window) }.as_bool())
        .then(|| application(window, id, &bounds))
        .flatten();
    Some(NativeWindow {
        id,
        bounds,
        visible: true,
        app,
    })
}

/// The foreground window and its process.
fn foreground() -> Option<(i32, HWND)> {
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&raw mut owner)) };
    Some((i32::try_from(owner).ok()?, window))
}

fn bounds(window: HWND) -> Option<DisplayBounds> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(window, &raw mut rect) }.ok()?;
    let width = f64::from(rect.right.saturating_sub(rect.left));
    let height = f64::from(rect.bottom.saturating_sub(rect.top));
    (width > 0.0 && height > 0.0).then_some(DisplayBounds {
        x: f64::from(rect.left),
        y: f64::from(rect.top),
        width,
        height,
    })
}

fn application(window: HWND, id: u32, bounds: &DisplayBounds) -> Option<ComputerTarget> {
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&raw mut process_id)) };
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut path = vec![0_u16; 1024];
    let mut length = u32::try_from(path.len()).ok()?;
    let named = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &raw mut length,
        )
    }
    .is_ok();
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    let timed = unsafe {
        GetProcessTimes(
            process,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .is_ok();
    unsafe { _ = CloseHandle(process) };
    if !named || !timed {
        return None;
    }
    // The executable path identifies the app, as a bundle id does on macOS.
    let bundle_id = String::from_utf16_lossy(path.get(..usize::try_from(length).ok()?)?);
    let application_name = std::path::Path::new(&bundle_id)
        .file_stem()
        .map(|stem| stem.to_string_lossy().chars().take(256).collect());
    let mut title = vec![0_u16; 512];
    let title_length = unsafe { GetWindowTextW(window, &mut title) };
    let title = usize::try_from(title_length)
        .ok()
        .filter(|length| *length > 0)
        .and_then(|length| title.get(..length))
        .map(String::from_utf16_lossy);
    // Creation time in 100-ns intervals identifies this process incarnation.
    let launch_time = f64::from(created.dwHighDateTime)
        .mul_add(4_294_967_296.0, f64::from(created.dwLowDateTime))
        / 10_000_000.0;
    Some(ComputerTarget {
        window_id: id,
        process_id: i32::try_from(process_id).ok()?,
        bundle_id,
        application_name,
        launch_time,
        bounds: bounds.clone(),
        title,
    })
}

fn bitmap_info(width: u32, height: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
            biWidth: i32::try_from(width).unwrap_or(1),
            // A negative height stores rows top to bottom.
            biHeight: i32::try_from(height).unwrap_or(1).saturating_neg(),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    }
}

fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|[blue, green, red, alpha]| [*red, *green, *blue, *alpha])
        .collect()
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "Float-to-integer casts saturate; normalized coordinates stay within 0..=65535."
)]
const fn rounded(value: f64) -> i32 {
    value.round() as i32
}

fn mouse_at(x: f64, y: f64, flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    // Absolute coordinates span the virtual desktop as 0..=65535.
    let normalize = |value: f64, origin: i32, extent: i32| {
        rounded((value - f64::from(origin)) * 65535.0 / f64::from(extent.saturating_sub(1)))
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: normalize(x, left, width),
                dy: normalize(y, top, height),
                mouseData: data.cast_unsigned(),
                dwFlags: flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

const fn key_input(key: VIRTUAL_KEY, unit: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Mac-style shortcuts name Command; Windows uses Control for the same shortcuts.
const fn modifier_key(modifier: Modifier) -> VIRTUAL_KEY {
    match modifier {
        Modifier::Command | Modifier::Control => VK_CONTROL,
        Modifier::Option => VK_MENU,
        Modifier::Shift => VK_SHIFT,
    }
}

fn virtual_key(key: Key) -> VIRTUAL_KEY {
    let letter = |offset: u16| VIRTUAL_KEY(0x41_u16.saturating_add(offset));
    let digit = |offset: u16| VIRTUAL_KEY(0x30_u16.saturating_add(offset));
    let function = |offset: u16| VIRTUAL_KEY(VK_F1.0.saturating_add(offset));
    match key {
        Key::Return => VK_RETURN,
        Key::Tab => VK_TAB,
        Key::Escape => VK_ESCAPE,
        Key::Space => VK_SPACE,
        Key::Backspace => VK_BACK,
        Key::Delete => VK_DELETE,
        Key::Left => VK_LEFT,
        Key::Right => VK_RIGHT,
        Key::Up => VK_UP,
        Key::Down => VK_DOWN,
        Key::Home => VK_HOME,
        Key::End => VK_END,
        Key::PageUp => VK_PRIOR,
        Key::PageDown => VK_NEXT,
        Key::A => letter(0),
        Key::B => letter(1),
        Key::C => letter(2),
        Key::D => letter(3),
        Key::E => letter(4),
        Key::F => letter(5),
        Key::G => letter(6),
        Key::H => letter(7),
        Key::I => letter(8),
        Key::J => letter(9),
        Key::K => letter(10),
        Key::L => letter(11),
        Key::M => letter(12),
        Key::N => letter(13),
        Key::O => letter(14),
        Key::P => letter(15),
        Key::Q => letter(16),
        Key::R => letter(17),
        Key::S => letter(18),
        Key::T => letter(19),
        Key::U => letter(20),
        Key::V => letter(21),
        Key::W => letter(22),
        Key::X => letter(23),
        Key::Y => letter(24),
        Key::Z => letter(25),
        Key::Digit0 => digit(0),
        Key::Digit1 => digit(1),
        Key::Digit2 => digit(2),
        Key::Digit3 => digit(3),
        Key::Digit4 => digit(4),
        Key::Digit5 => digit(5),
        Key::Digit6 => digit(6),
        Key::Digit7 => digit(7),
        Key::Digit8 => digit(8),
        Key::Digit9 => digit(9),
        Key::F1 => function(0),
        Key::F2 => function(1),
        Key::F3 => function(2),
        Key::F4 => function(3),
        Key::F5 => function(4),
        Key::F6 => function(5),
        Key::F7 => function(6),
        Key::F8 => function(7),
        Key::F9 => function(8),
        Key::F10 => function(9),
        Key::F11 => function(10),
        Key::F12 => function(11),
        Key::F13 => function(12),
        Key::F14 => function(13),
        Key::F15 => function(14),
        Key::F16 => function(15),
        Key::F17 => function(16),
        Key::F18 => function(17),
        Key::F19 => function(18),
        Key::F20 => function(19),
        Key::Minus => VK_OEM_MINUS,
        Key::Equal => VK_OEM_PLUS,
        Key::LeftBracket => VK_OEM_4,
        Key::RightBracket => VK_OEM_6,
        Key::Backslash => VK_OEM_5,
        Key::Semicolon => VK_OEM_1,
        Key::Quote => VK_OEM_7,
        Key::Comma => VK_OEM_COMMA,
        Key::Period => VK_OEM_PERIOD,
        Key::Slash => VK_OEM_2,
        Key::Backtick => VK_OEM_3,
    }
}
