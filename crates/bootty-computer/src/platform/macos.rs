//! macOS: `CoreGraphics` window lists and events, Accessibility focus and `ScreenCaptureKit`
//! capture.
//! Events go straight to the target process, so the user's pointer never moves.
#![allow(
    unsafe_code,
    reason = "Accessibility, Carbon and Objective-C calls are confined to this native adapter."
)]

use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, mpsc},
    time::Duration,
};

use block2::RcBlock;
use objc2::{AnyThread as _, rc::Retained};
use objc2_app_kit::{NSApplicationActivationOptions, NSImage, NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFGetTypeID, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect,
    CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGEvent, CGEventField, CGEventFlags,
    CGEventType, CGImage, CGImageAlphaInfo, CGMouseButton, CGPreflightScreenCaptureAccess,
    CGRectMakeWithDictionaryRepresentation, CGScrollEventUnit, CGWindowListCopyWindowInfo,
    CGWindowListOption, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer,
    kCGWindowName, kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_foundation::{NSError, NSRect, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};

use crate::{
    ComputerError, ComputerStatus, ComputerTarget, DisplayBounds, Key, Modifier, MouseButton,
    PermissionStatus,
    desktop::{Desktop, Focus, Image, InputEvent, NativeWindow},
};

// ScreenCaptureKit answers on its own queue; a capture that has not answered by then failed.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
// Accessibility calls into another app return within this many seconds or fail.
const AX_TIMEOUT: f32 = 0.2;
const AX_VALUE_CG_POINT: u32 = 1;
const AX_VALUE_CG_SIZE: u32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementSetMessagingTimeout(element: *const c_void, timeout: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementPerformAction(element: *const c_void, action: *const c_void) -> i32;
    fn AXUIElementGetTypeID() -> usize;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(value: *const c_void, kind: u32, out: *mut c_void) -> bool;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

pub fn native() -> Arc<dyn Desktop> {
    Arc::new(MacDesktop)
}

struct MacDesktop;

impl Desktop for MacDesktop {
    fn status(&self) -> ComputerStatus {
        ComputerStatus {
            accessibility: permission(unsafe { AXIsProcessTrusted() }),
            screen_recording: permission(CGPreflightScreenCaptureAccess()),
            secure_input: self.secure_input(),
        }
    }

    fn windows(&self) -> Vec<NativeWindow> {
        let Some(list) = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
            kCGNullWindowID,
        ) else {
            return Vec::new();
        };
        // The window list is documented as an array of dictionaries keyed by strings.
        let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
            unsafe { CFRetained::cast_unchecked(list) };
        list.iter()
            .filter_map(|info| native_window(&info))
            .collect()
    }

    fn focus(&self) -> Option<Focus> {
        let process_id = NSWorkspace::sharedWorkspace()
            .frontmostApplication()?
            .processIdentifier();
        let window = ax_application(process_id)
            .and_then(|app| ax_element(&app, "AXFocusedWindow"))
            .and_then(|window| element_bounds(&window));
        Some(Focus { process_id, window })
    }

    fn secure_input(&self) -> bool {
        if unsafe { IsSecureEventInputEnabled() } != 0 {
            return true;
        }
        let Some(element) = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .and_then(|app| ax_application(app.processIdentifier()))
            .and_then(|app| ax_element(&app, "AXFocusedUIElement"))
        else {
            return false;
        };
        ["AXRole", "AXSubrole"].into_iter().any(|name| {
            ax_attribute(&element, name)
                .and_then(|value| value.downcast::<CFString>().ok())
                .is_some_and(|role| role.to_string() == "AXSecureTextField")
        })
    }

    fn capture(
        &self,
        window: &NativeWindow,
        width: u32,
        height: u32,
        cursor: bool,
    ) -> Result<Image, ComputerError> {
        let process_id = window
            .app
            .as_ref()
            .map(|app| app.process_id)
            .ok_or(ComputerError::TargetUnavailable)?;
        let (sender, receiver) = mpsc::channel();
        let found = RcBlock::new(move |content: *mut SCShareableContent, _: *mut NSError| {
            // The handler may run once; retain the content past the callback.
            _ = sender.send(unsafe { Retained::retain(content) });
        });
        unsafe {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                false, true, &found,
            );
        }
        let content = receiver
            .recv_timeout(CAPTURE_TIMEOUT)
            .ok()
            .flatten()
            .ok_or(ComputerError::TargetUnavailable)?;
        let shareable = unsafe { content.windows() }
            .iter()
            .find(|candidate| unsafe {
                candidate.windowID() == window.id
                    && candidate
                        .owningApplication()
                        .is_some_and(|app| app.processID() == process_id)
            })
            .ok_or(ComputerError::TargetUnavailable)?;
        let filter = unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &shareable)
        };
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth(usize::try_from(width).unwrap_or(1));
            config.setHeight(usize::try_from(height).unwrap_or(1));
            config.setShowsCursor(cursor);
            // Pixels map to the window frame only, without compositor shadow padding.
            config.setIgnoreShadowsSingleWindow(true);
        }
        let (sender, receiver) = mpsc::channel();
        let captured = RcBlock::new(move |image: *mut CGImage, _: *mut NSError| {
            let image = NonNull::new(image).map(|image| unsafe { CFRetained::retain(image) });
            _ = sender.send(image);
        });
        unsafe {
            SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                &filter,
                &config,
                Some(&captured),
            );
        }
        let image = receiver
            .recv_timeout(CAPTURE_TIMEOUT)
            .ok()
            .flatten()
            .ok_or_else(|| ComputerError::Platform("window capture failed".into()))?;
        let (width, height) = (
            u32::try_from(CGImage::width(Some(&image))).unwrap_or(width),
            u32::try_from(CGImage::height(Some(&image))).unwrap_or(height),
        );
        draw(&image, width, height)
    }

    fn post(&self, window: &NativeWindow, event: &InputEvent) -> Result<(), ComputerError> {
        let process_id = window
            .app
            .as_ref()
            .map(|app| app.process_id)
            .ok_or(ComputerError::TargetUnavailable)?;
        let events = match event {
            InputEvent::Move { x, y } => {
                vec![mouse(CGEventType::MouseMoved, *x, *y, MouseButton::Left)?]
            }
            InputEvent::Click { x, y, button } => {
                let (down, up) = match button {
                    MouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
                    MouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
                    MouseButton::Middle => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
                };
                // Apps that hit-test against the last pointer position, such as GPUI apps, need
                // the pointer over the target before the press arrives.
                let moved = mouse(CGEventType::MouseMoved, *x, *y, *button)?;
                let (down, up) = (mouse(down, *x, *y, *button)?, mouse(up, *x, *y, *button)?);
                for press in [&down, &up] {
                    CGEvent::set_integer_value_field(
                        Some(press),
                        CGEventField::MouseEventClickState,
                        1,
                    );
                }
                vec![moved, down, up]
            }
            InputEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                let scroll = CGEvent::new_scroll_wheel_event2(
                    None,
                    CGScrollEventUnit::Pixel,
                    2,
                    *delta_y,
                    *delta_x,
                    0,
                )
                .ok_or_else(input_error)?;
                CGEvent::set_location(Some(&scroll), CGPoint { x: *x, y: *y });
                vec![scroll]
            }
            InputEvent::Text(text) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                let (down, up) = keyboard(0, CGEventFlags::empty())?;
                for event in [&down, &up] {
                    unsafe {
                        CGEvent::keyboard_set_unicode_string(
                            Some(event),
                            u64::try_from(units.len()).unwrap_or(0),
                            units.as_ptr(),
                        );
                    }
                }
                vec![down, up]
            }
            InputEvent::Key { key, modifiers } => {
                let flags = modifiers
                    .iter()
                    .fold(CGEventFlags::empty(), |flags, modifier| {
                        flags
                            | match modifier {
                                Modifier::Command => CGEventFlags::MaskCommand,
                                Modifier::Control => CGEventFlags::MaskControl,
                                Modifier::Option => CGEventFlags::MaskAlternate,
                                Modifier::Shift => CGEventFlags::MaskShift,
                            }
                    });
                let (down, up) = keyboard(key_code(*key), flags)?;
                vec![down, up]
            }
        };
        // Every event is allocated before the first is posted, so a press always gets its release.
        for event in &events {
            CGEvent::post_to_pid(process_id, Some(event));
        }
        Ok(())
    }

    fn raise(&self, window: &NativeWindow) -> Result<(), ComputerError> {
        let process_id = window
            .app
            .as_ref()
            .map(|app| app.process_id)
            .ok_or(ComputerError::TargetUnavailable)?;
        let app = ax_application(process_id).ok_or(ComputerError::TargetUnavailable)?;
        let windows = ax_attribute(&app, "AXWindows")
            .and_then(|value| value.downcast::<CFArray>().ok())
            .ok_or(ComputerError::TargetUnavailable)?;
        // Accessibility elements are CFTypes; the array holds only window elements.
        let windows: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(windows) };
        let exact: Vec<_> = windows
            .iter()
            .filter(|element| element_bounds(element).as_ref() == Some(&window.bounds))
            .collect();
        let [element] = exact.as_slice() else {
            return Err(ComputerError::TargetNotFocused);
        };
        let raise = CFString::from_static_str("AXRaise");
        let raised = unsafe { AXUIElementPerformAction(cf(element), cf(&raise)) } == 0;
        let activated = NSRunningApplication::runningApplicationWithProcessIdentifier(process_id)
            .is_some_and(|app| {
                app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
            });
        if raised && activated {
            Ok(())
        } else {
            Err(ComputerError::TargetNotFocused)
        }
    }

    fn icon(&self, bundle_id: &str, side: u32) -> Option<Image> {
        let bundle = NSString::from_str(bundle_id);
        let icon: Retained<NSImage> =
            NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle)
                .firstObject()
                .and_then(|app| app.icon())
                .or_else(|| {
                    let workspace = NSWorkspace::sharedWorkspace();
                    let path = workspace
                        .URLForApplicationWithBundleIdentifier(&bundle)?
                        .path()?;
                    Some(workspace.iconForFile(&path))
                })?;
        let mut rect = NSRect::new(
            objc2_foundation::NSPoint::new(0.0, 0.0),
            objc2_foundation::NSSize::new(f64::from(side), f64::from(side)),
        );
        let image =
            unsafe { icon.CGImageForProposedRect_context_hints(&raw mut rect, None, None) }?;
        draw(&image, side, side).ok()
    }
}

const fn permission(granted: bool) -> PermissionStatus {
    if granted {
        PermissionStatus::Granted
    } else {
        PermissionStatus::NotGranted
    }
}

fn native_window(info: &CFDictionary<CFString, CFType>) -> Option<NativeWindow> {
    let number = |key: &CFString| {
        info.get(key)
            .and_then(|value| value.downcast::<CFNumber>().ok())
    };
    let id = u32::try_from(number(unsafe { kCGWindowNumber })?.as_i64()?).ok()?;
    let process_id = number(unsafe { kCGWindowOwnerPID })?.as_i32()?;
    let layer = number(unsafe { kCGWindowLayer })
        .and_then(|layer| layer.as_i32())
        .unwrap_or(0);
    let alpha = number(unsafe { kCGWindowAlpha })
        .and_then(|alpha| alpha.as_f64())
        .unwrap_or(1.0);
    let bounds = info
        .get(unsafe { kCGWindowBounds })
        .and_then(|value| value.downcast::<CFDictionary>().ok())?;
    let mut rect = CGRect::default();
    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(&bounds), &raw mut rect) } {
        return None;
    }
    let bounds = DisplayBounds {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    };
    let title = info
        .get(unsafe { kCGWindowName })
        .and_then(|value| value.downcast::<CFString>().ok())
        .map(|title| title.to_string().chars().take(512).collect());
    let app = (layer == 0 && bounds.width > 0.0 && bounds.height > 0.0)
        .then(|| application(process_id, id, &bounds, title))
        .flatten();
    Some(NativeWindow {
        id,
        bounds,
        visible: alpha > 0.0,
        app,
    })
}

fn application(
    process_id: i32,
    window_id: u32,
    bounds: &DisplayBounds,
    title: Option<String>,
) -> Option<ComputerTarget> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(process_id)?;
    Some(ComputerTarget {
        window_id,
        process_id,
        bundle_id: app.bundleIdentifier()?.to_string(),
        application_name: app
            .localizedName()
            .map(|name| name.to_string().chars().take(256).collect()),
        launch_time: app.launchDate()?.timeIntervalSince1970(),
        bounds: bounds.clone(),
        title,
    })
}

const fn cf<T: ?Sized>(value: &T) -> *const c_void {
    std::ptr::from_ref(value).cast()
}

fn ax_application(process_id: i32) -> Option<CFRetained<CFType>> {
    let raw = NonNull::new(unsafe { AXUIElementCreateApplication(process_id) }.cast::<CFType>())?;
    // AXUIElementCreateApplication returns a +1 reference.
    let app = unsafe { CFRetained::from_raw(raw) };
    unsafe { AXUIElementSetMessagingTimeout(cf(&*app), AX_TIMEOUT) };
    Some(app)
}

fn ax_attribute(element: &CFType, name: &str) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut value = std::ptr::null_mut();
    if unsafe { AXUIElementCopyAttributeValue(cf(element), cf(&*name), &raw mut value) } != 0 {
        return None;
    }
    // Copy functions return a +1 reference.
    NonNull::new(value.cast::<CFType>()).map(|value| unsafe { CFRetained::from_raw(value) })
}

fn ax_element(element: &CFType, name: &str) -> Option<CFRetained<CFType>> {
    ax_attribute(element, name)
        .filter(|value| CFGetTypeID(Some(value)) == unsafe { AXUIElementGetTypeID() })
}

fn element_bounds(element: &CFType) -> Option<DisplayBounds> {
    let value = |name, kind, out: *mut c_void| {
        ax_attribute(element, name)
            .filter(|value| CFGetTypeID(Some(value)) == unsafe { AXValueGetTypeID() })
            .is_some_and(|value| unsafe { AXValueGetValue(cf(&*value), kind, out) })
    };
    let mut origin = CGPoint::default();
    let mut size = CGSize::default();
    (value("AXPosition", AX_VALUE_CG_POINT, (&raw mut origin).cast())
        && value("AXSize", AX_VALUE_CG_SIZE, (&raw mut size).cast()))
    .then_some(DisplayBounds {
        x: origin.x,
        y: origin.y,
        width: size.width,
        height: size.height,
    })
}

fn input_error() -> ComputerError {
    ComputerError::Platform("could not create an input event".into())
}

fn mouse(
    kind: CGEventType,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<CFRetained<CGEvent>, ComputerError> {
    let button = match button {
        MouseButton::Left => CGMouseButton::Left,
        MouseButton::Right => CGMouseButton::Right,
        MouseButton::Middle => CGMouseButton::Center,
    };
    CGEvent::new_mouse_event(None, kind, CGPoint { x, y }, button).ok_or_else(input_error)
}

fn keyboard(
    code: u16,
    flags: CGEventFlags,
) -> Result<(CFRetained<CGEvent>, CFRetained<CGEvent>), ComputerError> {
    let down = CGEvent::new_keyboard_event(None, code, true).ok_or_else(input_error)?;
    let up = CGEvent::new_keyboard_event(None, code, false).ok_or_else(input_error)?;
    CGEvent::set_flags(Some(&down), flags);
    CGEvent::set_flags(Some(&up), flags);
    Ok((down, up))
}

/// Draw a `CGImage` into tightly packed RGBA pixels of the given size.
fn draw(image: &CGImage, width: u32, height: u32) -> Result<Image, ComputerError> {
    let unreadable = || ComputerError::Platform("could not read captured pixels".into());
    let columns = usize::try_from(width).map_err(|_| unreadable())?;
    let rows = usize::try_from(height).map_err(|_| unreadable())?;
    let stride = columns.checked_mul(4).ok_or_else(unreadable)?;
    let mut rgba = vec![0_u8; stride.checked_mul(rows).ok_or_else(unreadable)?];
    let space = CGColorSpace::new_device_rgb().ok_or_else(unreadable)?;
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            columns,
            rows,
            8,
            stride,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
    .ok_or_else(unreadable)?;
    CGContext::draw_image(
        Some(&context),
        CGRect::new(
            CGPoint::default(),
            CGSize {
                width: f64::from(width),
                height: f64::from(height),
            },
        ),
        Some(image),
    );
    drop(context);
    // PNG stores straight alpha; window captures are opaque except at rounded corners.
    for [red, green, blue, alpha] in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(*alpha);
        if alpha != 0 && alpha != 255 {
            for channel in [red, green, blue] {
                let straight = u16::from(*channel)
                    .saturating_mul(255)
                    .checked_div(alpha)
                    .unwrap_or(255);
                *channel = u8::try_from(straight).unwrap_or(u8::MAX);
            }
        }
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}

/// macOS virtual key codes from Carbon's `Events.h`.
const fn key_code(key: Key) -> u16 {
    match key {
        Key::Return => 0x24,
        Key::Tab => 0x30,
        Key::Escape => 0x35,
        Key::Space => 0x31,
        Key::Backspace => 0x33,
        Key::Delete => 0x75,
        Key::Left => 0x7B,
        Key::Right => 0x7C,
        Key::Up => 0x7E,
        Key::Down => 0x7D,
        Key::Home => 0x73,
        Key::End => 0x77,
        Key::PageUp => 0x74,
        Key::PageDown => 0x79,
        Key::A => 0x00,
        Key::B => 0x0B,
        Key::C => 0x08,
        Key::D => 0x02,
        Key::E => 0x0E,
        Key::F => 0x03,
        Key::G => 0x05,
        Key::H => 0x04,
        Key::I => 0x22,
        Key::J => 0x26,
        Key::K => 0x28,
        Key::L => 0x25,
        Key::M => 0x2E,
        Key::N => 0x2D,
        Key::O => 0x1F,
        Key::P => 0x23,
        Key::Q => 0x0C,
        Key::R => 0x0F,
        Key::S => 0x01,
        Key::T => 0x11,
        Key::U => 0x20,
        Key::V => 0x09,
        Key::W => 0x0D,
        Key::X => 0x07,
        Key::Y => 0x10,
        Key::Z => 0x06,
        Key::Digit0 => 0x1D,
        Key::Digit1 => 0x12,
        Key::Digit2 => 0x13,
        Key::Digit3 => 0x14,
        Key::Digit4 => 0x15,
        Key::Digit5 => 0x17,
        Key::Digit6 => 0x16,
        Key::Digit7 => 0x1A,
        Key::Digit8 => 0x1C,
        Key::Digit9 => 0x19,
        Key::F1 => 0x7A,
        Key::F2 => 0x78,
        Key::F3 => 0x63,
        Key::F4 => 0x76,
        Key::F5 => 0x60,
        Key::F6 => 0x61,
        Key::F7 => 0x62,
        Key::F8 => 0x64,
        Key::F9 => 0x65,
        Key::F10 => 0x6D,
        Key::F11 => 0x67,
        Key::F12 => 0x6F,
        Key::F13 => 0x69,
        Key::F14 => 0x6B,
        Key::F15 => 0x71,
        Key::F16 => 0x6A,
        Key::F17 => 0x40,
        Key::F18 => 0x4F,
        Key::F19 => 0x50,
        Key::F20 => 0x5A,
        Key::Minus => 0x1B,
        Key::Equal => 0x18,
        Key::LeftBracket => 0x21,
        Key::RightBracket => 0x1E,
        Key::Backslash => 0x2A,
        Key::Semicolon => 0x29,
        Key::Quote => 0x27,
        Key::Comma => 0x2B,
        Key::Period => 0x2F,
        Key::Slash => 0x2C,
        Key::Backtick => 0x32,
    }
}
