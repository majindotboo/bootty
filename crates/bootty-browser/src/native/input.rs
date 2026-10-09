//! Safe AppKit/Wry handles dispatch to the owned web content view, never the global event queue.
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSView, NSWindow};
use objc2_core_graphics::{CGEvent, CGScrollEventUnit};
use objc2_foundation::{NSPoint, NSProcessInfo, NSString};
use wry::{WebView, WebViewExtMacOS as _};

use crate::{BrowserInput, BrowserModifier, BrowserMouseButton, NativeBrowserError};

fn unavailable() -> NativeBrowserError {
    NativeBrowserError::Platform("The browser input target is unavailable".into())
}

pub(super) fn dispatch(view: &WebView, action: &BrowserInput) -> Result<(), NativeBrowserError> {
    let webview = view.webview();
    let window = webview.window().ok_or_else(unavailable)?;
    let bounds = webview.bounds();
    let time = NSProcessInfo::processInfo().systemUptime();
    match action {
        BrowserInput::Click { x, y, .. } | BrowserInput::Scroll { x, y, .. } => {
            let x = f64::from(*x);
            let y = f64::from(*y);
            if x >= bounds.size.width || y >= bounds.size.height {
                return Err(NativeBrowserError::Platform(
                    "Browser coordinates are outside the page".into(),
                ));
            }
            let local = NSPoint::new(
                x,
                if webview.isFlipped() {
                    y
                } else {
                    bounds.size.height - y
                },
            );
            // Wry's child view is axis-aligned; hitTest takes a point in its parent's coordinates.
            let frame = webview.frame();
            let hit = webview
                .hitTest(NSPoint::new(
                    frame.origin.x + local.x,
                    frame.origin.y + local.y,
                ))
                .ok_or_else(unavailable)?;
            if !hit.isDescendantOf(&webview) {
                return Err(unavailable());
            }
            let point = webview.convertPoint_toView(local, None);
            if let BrowserInput::Scroll {
                delta_x, delta_y, ..
            } = action
            {
                let event = CGEvent::new_scroll_wheel_event2(
                    None,
                    CGScrollEventUnit::Pixel,
                    2,
                    delta_y.saturating_neg(),
                    delta_x.saturating_neg(),
                    0,
                )
                .ok_or_else(unavailable)?;
                let event = NSEvent::eventWithCGEvent(&event).ok_or_else(unavailable)?;
                hit.scrollWheel(&event);
            } else {
                let BrowserInput::Click { button, .. } = action else {
                    return Err(unavailable());
                };
                let kinds = if *button == BrowserMouseButton::Left {
                    [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp]
                } else {
                    [NSEventType::RightMouseDown, NSEventType::RightMouseUp]
                };
                for kind in kinds {
                    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, NSEventModifierFlags::empty(), time, window.windowNumber(), None, 0, 1, 1.0).ok_or_else(unavailable)?;
                    match kind {
                        NSEventType::LeftMouseDown => hit.mouseDown(&event),
                        NSEventType::LeftMouseUp => hit.mouseUp(&event),
                        NSEventType::RightMouseDown => hit.rightMouseDown(&event),
                        _ => hit.rightMouseUp(&event),
                    }
                }
            }
        }
        BrowserInput::Type { text } | BrowserInput::Key { key: text, .. } => {
            let mut responder = window
                .firstResponder()
                .and_then(|responder| responder.downcast::<NSView>().ok())
                .filter(|responder| responder.isDescendantOf(&webview));
            if responder.is_none() {
                if !window.makeFirstResponder(Some(&webview)) {
                    return Err(unavailable());
                }
                responder = window
                    .firstResponder()
                    .and_then(|responder| responder.downcast::<NSView>().ok())
                    .filter(|responder| responder.isDescendantOf(&webview));
            }
            let responder = responder.ok_or_else(unavailable)?;
            let (code, characters, flags) = if let BrowserInput::Key { modifiers, .. } = action {
                // Wry deliberately disables child-view Command key equivalents. Keep this
                // explicit until its public API can dispatch them to this exact content view.
                if modifiers.contains(&BrowserModifier::Super) {
                    return Err(NativeBrowserError::CommandShortcutUnavailable);
                }
                let (code, characters) = crate::input::key_code(text).ok_or_else(unavailable)?;
                let flags = modifier_flags(modifiers);
                (code, characters, flags)
            } else {
                (0, text.as_str(), NSEventModifierFlags::empty())
            };
            send_key(&responder, &window, code, characters, flags)?;
        }
    }
    Ok(())
}

fn modifier_flags(modifiers: &[BrowserModifier]) -> NSEventModifierFlags {
    modifiers
        .iter()
        .fold(NSEventModifierFlags::empty(), |flags, modifier| {
            flags
                | match modifier {
                    BrowserModifier::Shift => NSEventModifierFlags::Shift,
                    BrowserModifier::Control => NSEventModifierFlags::Control,
                    BrowserModifier::Alt => NSEventModifierFlags::Option,
                    BrowserModifier::Super => NSEventModifierFlags::Command,
                }
        })
}

fn send_key(
    responder: &NSView,
    window: &NSWindow,
    code: u16,
    characters: &str,
    flags: NSEventModifierFlags,
) -> Result<(), NativeBrowserError> {
    let characters = NSString::from_str(&if flags.contains(NSEventModifierFlags::Shift) {
        characters.to_uppercase()
    } else {
        characters.to_owned()
    });
    for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(kind, NSPoint::new(0.0, 0.0), flags, NSProcessInfo::processInfo().systemUptime(), window.windowNumber(), None, &characters, &characters, false, code).ok_or_else(unavailable)?;
        if kind == NSEventType::KeyDown {
            responder.keyDown(&event);
        } else {
            responder.keyUp(&event);
        }
    }
    Ok(())
}
