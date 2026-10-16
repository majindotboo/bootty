//! macOS: `CoreGraphics` window lists and events, Accessibility focus and elements, and
//! `ScreenCaptureKit` capture.
//! Events go straight to the target process addressed to its window, so the user's pointer never
//! moves and the window needs no focus.
//!
//! Deliberate limit: a click in a background app's window still meets `AppKit`'s first-click rule,
//! so views that need their window active, such as web content, ignore it. The shared rules press
//! the element under the point instead, which leaves only points without a pressable element, such
//! as a web canvas. Click those directly if macOS offers a public way to make a window key without
//! activating its app.
#![allow(
    unsafe_code,
    reason = "Accessibility, Carbon and Objective-C calls are confined to this native adapter."
)]

use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use block2::RcBlock;
use objc2::{AnyThread as _, rc::Retained};
use objc2_app_kit::{NSImage, NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFGetTypeID, CFNumber, CFRetained, CFString, CFType, CGPoint,
    CGRect, CGSize,
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
    ComputerElement, ComputerError, ComputerStatus, ComputerTarget, DisplayBounds, ElementState,
    Key, MAX_ELEMENT_DEPTH, Modifier, MouseButton, PermissionStatus,
    desktop::{Desktop, Image, InputEvent, InputRoute, NativeWindow},
};

// ScreenCaptureKit answers on its own queue; a capture that has not answered by then failed.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
// Accessibility calls into another app return within this many seconds or fail.
const AX_TIMEOUT: f32 = 0.2;
const AX_VALUE_CG_POINT: u32 = 1;
const AX_VALUE_CG_SIZE: u32 = 2;
const AX_ERROR_CANNOT_COMPLETE: i32 = -25204;
const AX_ERROR_ACTION_UNSUPPORTED: i32 = -25206;
/// The undocumented event field `AppKit` reads as `NSEvent.windowNumber`. Events posted to a process
/// without it reach no window while the app is inactive.
const WINDOW_NUMBER: CGEventField = CGEventField(51);
// A listing reads at most this many elements and stops at this budget, even in huge web pages.
const MAX_ELEMENT_READS: usize = 4_000;
const ELEMENT_BUDGET: Duration = Duration::from_secs(3);
/// Names and values longer than this many characters are cut.
const MAX_ELEMENT_TEXT: usize = 200;
/// Read together, one call per element; `Attributes` indexes values in this order.
const ELEMENT_ATTRIBUTES: [&str; 12] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXHelp",
    "AXValue",
    "AXIdentifier",
    "AXPosition",
    "AXSize",
    "AXEnabled",
    "AXSelected",
    "AXChildren",
];
/// Roles listed even without a name, because they act when pressed or typed into.
const CONTROL_ROLES: [&str; 17] = [
    "AXButton",
    "AXCheckBox",
    "AXRadioButton",
    "AXPopUpButton",
    "AXMenuButton",
    "AXComboBox",
    "AXTextField",
    "AXTextArea",
    "AXSlider",
    "AXIncrementor",
    "AXLink",
    "AXDisclosureTriangle",
    "AXMenuItem",
    "AXColorWell",
    "AXDateField",
    "AXSegmentedControl",
    "AXRow",
];

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
    fn AXUIElementCopyElementAtPosition(
        application: *const c_void,
        x: f32,
        y: f32,
        element: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementCopyMultipleAttributeValues(
        element: *const c_void,
        attributes: *const c_void,
        options: u32,
        values: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementCopyActionNames(element: *const c_void, names: *mut *mut c_void) -> i32;
    fn AXUIElementSetAttributeValue(
        element: *const c_void,
        attribute: *const c_void,
        value: *const c_void,
    ) -> i32;
    fn AXUIElementGetTypeID() -> usize;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(value: *const c_void, kind: u32, out: *mut c_void) -> bool;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

// Exported by the SDK's CoreGraphics stubs without a header. Sets an event's point relative to
// the top-left of its window, which AppKit reads as `NSEvent.locationInWindow`.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSetWindowLocation(event: *const c_void, location: CGPoint);
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

    fn route(&self) -> InputRoute {
        InputRoute::Window
    }

    fn frontmost(&self) -> Option<i32> {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| app.processIdentifier())
    }

    /// Every app keeps its own focused window, in front or not.
    fn focus(&self, process_id: i32) -> Option<DisplayBounds> {
        ax_application(process_id)
            .and_then(|app| ax_element(&app, "AXFocusedWindow"))
            .and_then(|window| element_bounds(&window))
    }

    fn secure_input(&self) -> bool {
        (unsafe { IsSecureEventInputEnabled() }) != 0
            || NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .is_some_and(|app| secure_focus(app.processIdentifier()))
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
        // Keys reach a background app's focused element too, so its password fields stay off
        // limits just as the frontmost app's do.
        if matches!(event, InputEvent::Text(_) | InputEvent::Key { .. }) && secure_focus(process_id)
        {
            return Err(ComputerError::SecureInput);
        }
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
            address(event, window);
            CGEvent::post_to_pid(process_id, Some(event));
        }
        Ok(())
    }

    /// `AXRaise` makes the window its app's focused window without activating the app, which
    /// macOS refuses a background caller anyway. The window moves up behind the frontmost app's.
    fn raise(&self, window: &NativeWindow) -> Result<(), ComputerError> {
        let element = ax_window(window).ok_or(ComputerError::TargetNotFocused)?;
        let raise = CFString::from_static_str("AXRaise");
        if unsafe { AXUIElementPerformAction(cf(&element), cf(&raise)) } == 0 {
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

    fn elements(
        &self,
        window: &NativeWindow,
        limit: usize,
    ) -> Result<(Vec<ComputerElement>, bool), ComputerError> {
        let root = ax_window(window).ok_or(ComputerError::TargetUnavailable)?;
        if let Some(app) = window
            .app
            .as_ref()
            .and_then(|app| ax_application(app.process_id))
        {
            // Chromium and Electron apps expose web content only to assistive apps that ask, and
            // build it in the background, so the first listing of a page may still be partial.
            // Other apps refuse the attribute.
            let manual = CFString::from_static_str("AXManualAccessibility");
            unsafe {
                AXUIElementSetAttributeValue(cf(&app), cf(&manual), cf(CFBoolean::new(true)));
            }
        }
        let focused = window
            .app
            .as_ref()
            .and_then(|app| ax_application(app.process_id))
            .and_then(|app| ax_element(&app, "AXFocusedUIElement"));
        let mut listing = Listing {
            names: attribute_names(),
            focused,
            window: window.bounds.clone(),
            limit,
            deadline: Instant::now()
                .checked_add(ELEMENT_BUDGET)
                .unwrap_or_else(Instant::now),
            reads: 0,
            elements: Vec::new(),
            omitted: false,
        };
        let children =
            Attributes::read(&root, &listing.names).map_or_else(Vec::new, |root| root.children());
        listing.visit(&children, &mut Vec::new(), None);
        Ok((listing.elements, listing.omitted))
    }

    fn element(
        &self,
        window: &NativeWindow,
        path: &[u16],
    ) -> Result<Option<ComputerElement>, ComputerError> {
        let names = attribute_names();
        Ok(element_at(window, path)
            .and_then(|element| Attributes::read(&element, &names))
            .and_then(|attributes| attributes.element(path.to_vec(), false)))
    }

    fn pressable_at(
        &self,
        window: &NativeWindow,
        x: f64,
        y: f64,
    ) -> Result<Option<Vec<u16>>, ComputerError> {
        let root = ax_window(window).ok_or(ComputerError::TargetUnavailable)?;
        let Some(app) = window
            .app
            .as_ref()
            .and_then(|app| ax_application(app.process_id))
        else {
            return Ok(None);
        };
        // Hit-testing through the app element looks only at that app's windows, front to back.
        let mut hit = std::ptr::null_mut();
        if unsafe {
            AXUIElementCopyElementAtPosition(
                cf(&app),
                ax_coordinate(x),
                ax_coordinate(y),
                &raw mut hit,
            )
        } != 0
        {
            return Ok(None);
        }
        let Some(hit) = NonNull::new(hit.cast::<CFType>()) else {
            return Ok(None);
        };
        // Copy functions return a +1 reference. The chain runs from the hit element up to the
        // window; a point showing another window of the app never reaches it.
        let mut chain = vec![unsafe { CFRetained::from_raw(hit) }];
        while chain.last().is_some_and(|element| **element != *root) {
            let parent = chain
                .last()
                .and_then(|element| ax_element(element, "AXParent"));
            match parent {
                Some(parent) if chain.len() <= MAX_ELEMENT_DEPTH => chain.push(parent),
                _ => return Ok(None),
            }
        }
        // The window itself is never pressed. A click goes nowhere on a disabled element, so its
        // ancestors are never pressed for it either.
        let inside = chain.len().saturating_sub(1);
        let Some(found) = chain
            .iter()
            .take(inside)
            .position(|element| disabled(element) || has_action(element, "AXPress"))
        else {
            return Ok(None);
        };
        if chain.get(found).is_some_and(|element| disabled(element)) {
            return Ok(None);
        }
        // Each step names a child by its position under its parent, from the window down.
        Ok(chain
            .windows(2)
            .skip(found)
            .rev()
            .map(|pair| match pair {
                [child, parent] => child_index(parent, child),
                _ => None,
            })
            .collect())
    }

    fn press(&self, window: &NativeWindow, path: &[u16]) -> Result<bool, ComputerError> {
        let element = element_at(window, path).ok_or(ComputerError::StaleElement)?;
        if !has_action(&element, "AXPress") {
            return Ok(false);
        }
        let action = CFString::from_static_str("AXPress");
        match unsafe { AXUIElementPerformAction(cf(&element), cf(&action)) } {
            0 => Ok(true),
            AX_ERROR_ACTION_UNSUPPORTED => Ok(false),
            // The app may still act on a press it did not confirm, so this is not a reason to click.
            AX_ERROR_CANNOT_COMPLETE => Err(ComputerError::Platform(
                "the app did not answer the press in time".into(),
            )),
            code => Err(ComputerError::Platform(format!(
                "the app refused the press ({code})"
            ))),
        }
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

/// The CF object behind a reference. Taking `&CFType` makes a retained wrapper deref to its
/// object, so a call can't pass the wrapper's address instead.
const fn cf(value: &CFType) -> *const c_void {
    std::ptr::from_ref(value).cast()
}

fn ax_application(process_id: i32) -> Option<CFRetained<CFType>> {
    let raw = NonNull::new(unsafe { AXUIElementCreateApplication(process_id) }.cast::<CFType>())?;
    // AXUIElementCreateApplication returns a +1 reference.
    let app = unsafe { CFRetained::from_raw(raw) };
    unsafe { AXUIElementSetMessagingTimeout(cf(&app), AX_TIMEOUT) };
    Some(app)
}

fn ax_attribute(element: &CFType, name: &str) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut value = std::ptr::null_mut();
    if unsafe { AXUIElementCopyAttributeValue(cf(element), cf(&name), &raw mut value) } != 0 {
        return None;
    }
    // Copy functions return a +1 reference.
    NonNull::new(value.cast::<CFType>()).map(|value| unsafe { CFRetained::from_raw(value) })
}

fn ax_element(element: &CFType, name: &str) -> Option<CFRetained<CFType>> {
    ax_attribute(element, name)
        .filter(|value| CFGetTypeID(Some(value)) == unsafe { AXUIElementGetTypeID() })
}

/// The accessibility element of exactly this window, matched by frame.
fn ax_window(window: &NativeWindow) -> Option<CFRetained<CFType>> {
    let app = ax_application(window.app.as_ref()?.process_id)?;
    let windows = ax_attribute(&app, "AXWindows")?
        .downcast::<CFArray>()
        .ok()?;
    // Accessibility elements are CFTypes; the array holds only window elements.
    let windows: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(windows) };
    let mut exact = windows
        .iter()
        .filter(|element| element_bounds(element).as_ref() == Some(&window.bounds));
    let element = exact.next()?;
    exact.next().is_none().then_some(element)
}

/// The element `path` names, by child position from the window.
fn element_at(window: &NativeWindow, path: &[u16]) -> Option<CFRetained<CFType>> {
    let mut element = ax_window(window)?;
    for &index in path {
        let children = ax_attribute(&element, "AXChildren")?
            .downcast::<CFArray>()
            .ok()?;
        // Children of an accessibility element are elements.
        let children: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(children) };
        element = children.get(usize::from(index))?;
    }
    Some(element)
}

fn attribute_names() -> CFRetained<CFArray<CFString>> {
    CFArray::from_retained_objects(&ELEMENT_ATTRIBUTES.map(CFString::from_static_str))
}

/// One element's attributes from a single call, in [`ELEMENT_ATTRIBUTES`] order. A missing
/// attribute holds an error value, which no typed read below accepts.
struct Attributes(CFRetained<CFArray<CFType>>);

impl Attributes {
    fn read(element: &CFType, names: &CFArray<CFString>) -> Option<Self> {
        let mut values = std::ptr::null_mut();
        if unsafe {
            AXUIElementCopyMultipleAttributeValues(cf(element), cf(names), 0, &raw mut values)
        } != 0
        {
            return None;
        }
        let values = NonNull::new(values.cast::<CFArray>())?;
        // Copy functions return a +1 reference to an array of values.
        Some(Self(unsafe {
            CFRetained::cast_unchecked(CFRetained::from_raw(values))
        }))
    }

    fn value(&self, name: &str) -> Option<CFRetained<CFType>> {
        let index = ELEMENT_ATTRIBUTES.iter().position(|known| *known == name)?;
        self.0.get(index)
    }

    fn string(&self, name: &str) -> Option<String> {
        self.value(name)?
            .downcast::<CFString>()
            .ok()
            .and_then(|text| clean_text(&text.to_string()))
    }

    fn flag(&self, name: &str) -> Option<bool> {
        self.value(name)?
            .downcast::<CFBoolean>()
            .ok()
            .map(|flag| flag.as_bool())
    }

    fn number(&self, name: &str) -> Option<f64> {
        self.value(name)?.downcast::<CFNumber>().ok()?.as_f64()
    }

    fn children(&self) -> Vec<CFRetained<CFType>> {
        let Some(children) = self
            .value("AXChildren")
            .and_then(|children| children.downcast::<CFArray>().ok())
        else {
            return Vec::new();
        };
        // Children of an accessibility element are elements.
        let children: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(children) };
        children.to_vec()
    }

    fn frame(&self) -> Option<DisplayBounds> {
        let read = |name, kind, out: *mut c_void| {
            self.value(name)
                .filter(|value| CFGetTypeID(Some(value)) == unsafe { AXValueGetTypeID() })
                .is_some_and(|value| unsafe { AXValueGetValue(cf(&value), kind, out) })
        };
        let mut origin = CGPoint::default();
        let mut size = CGSize::default();
        (read("AXPosition", AX_VALUE_CG_POINT, (&raw mut origin).cast())
            && read("AXSize", AX_VALUE_CG_SIZE, (&raw mut size).cast()))
        .then_some(DisplayBounds {
            x: origin.x,
            y: origin.y,
            width: size.width,
            height: size.height,
        })
    }

    /// The element as listed, when it has a role and a visible frame.
    fn element(&self, path: Vec<u16>, focused: bool) -> Option<ComputerElement> {
        let role = self
            .value("AXRole")?
            .downcast::<CFString>()
            .ok()?
            .to_string();
        let subrole = self
            .value("AXSubrole")
            .and_then(|subrole| subrole.downcast::<CFString>().ok())
            .map(|subrole| subrole.to_string())
            .filter(|subrole| subrole != "AXUnknown");
        let frame = self
            .frame()
            .filter(|frame| frame.width > 0.0 && frame.height > 0.0)?;
        let secure = subrole.as_deref() == Some("AXSecureTextField");
        let toggle = matches!(role.as_str(), "AXRadioButton" | "AXCheckBox");
        let on = toggle && self.number("AXValue") == Some(1.0);
        let mut states = Vec::new();
        if self.flag("AXEnabled") == Some(false) {
            states.push(ElementState::Disabled);
        }
        if self.flag("AXSelected") == Some(true) || on && role == "AXRadioButton" {
            states.push(ElementState::Selected);
        }
        if on && role == "AXCheckBox" {
            states.push(ElementState::Checked);
        }
        if focused {
            states.push(ElementState::Focused);
        }
        Some(ComputerElement {
            path,
            role: subrole.as_ref().map_or_else(
                || bare(&role).to_owned(),
                |subrole| format!("{}:{}", bare(&role), bare(subrole)),
            ),
            name: ["AXTitle", "AXDescription", "AXHelp"]
                .into_iter()
                .find_map(|name| self.string(name)),
            // Password fields never expose their contents; toggles report their state instead.
            value: (!secure && !toggle)
                .then(|| {
                    self.string("AXValue")
                        .or_else(|| self.number("AXValue").map(|number| number.to_string()))
                })
                .flatten(),
            identifier: self.string("AXIdentifier"),
            frame,
            states,
        })
    }
}

/// A role or subrole without its `AX` prefix.
fn bare(name: &str) -> &str {
    name.strip_prefix("AX").unwrap_or(name)
}

/// Single-line text cut to [`MAX_ELEMENT_TEXT`] characters; `None` when blank.
fn clean_text(text: &str) -> Option<String> {
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        return None;
    }
    let mut cut: String = words.chars().take(MAX_ELEMENT_TEXT).collect();
    if cut.len() < words.len() {
        cut.push('…');
    }
    Some(cut)
}

/// A depth-first walk that lists elements in screen order within a fixed budget.
struct Listing {
    names: CFRetained<CFArray<CFString>>,
    /// The app's one focused element; apps also report focus on its ancestors.
    focused: Option<CFRetained<CFType>>,
    window: DisplayBounds,
    limit: usize,
    deadline: Instant,
    reads: usize,
    elements: Vec<ComputerElement>,
    omitted: bool,
}

impl Listing {
    /// `context` is the name of the nearest listed ancestor; text that only repeats it is left
    /// out, as in a link and its own label.
    fn visit(
        &mut self,
        children: &[CFRetained<CFType>],
        path: &mut Vec<u16>,
        context: Option<&str>,
    ) {
        let mut siblings = Vec::new();
        for (index, child) in children.iter().enumerate() {
            if self.reads >= MAX_ELEMENT_READS || Instant::now() >= self.deadline {
                self.omitted = true;
                return;
            }
            self.reads = self.reads.saturating_add(1);
            let (Ok(index), Some(attributes)) =
                (u16::try_from(index), Attributes::read(child, &self.names))
            else {
                continue;
            };
            let frame = attributes.frame();
            // Scrolled-away content and everything inside it stays out of the listing. Scroll
            // bars are left out too: agents scroll with the scroll action.
            if frame
                .as_ref()
                .is_some_and(|frame| !intersects(frame, &self.window))
                || attributes.string("AXRole").as_deref() == Some("AXScrollBar")
            {
                continue;
            }
            let focused = self.focused.as_deref() == Some(&**child);
            siblings.push((frame, (index, focused, attributes)));
        }
        for (_, (index, focused, attributes)) in reading_order(siblings) {
            path.push(index);
            let children = attributes.children();
            let element = (focused || listed(&attributes, children.is_empty(), context))
                .then(|| attributes.element(path.clone(), focused))
                .flatten();
            let name = element.as_ref().and_then(|element| element.name.clone());
            if let Some(element) = element {
                if self.elements.len() >= self.limit {
                    self.omitted = true;
                    path.pop();
                    return;
                }
                self.elements.push(element);
            }
            if path.len() < MAX_ELEMENT_DEPTH {
                self.visit(&children, path, name.as_deref().or(context));
            }
            path.pop();
            if self.omitted {
                return;
            }
        }
    }
}

/// A sibling's frame and what the walk keeps with it.
type Sibling<T> = (Option<DisplayBounds>, T);

/// Apps order children their own way. Reading order is lines from top to bottom, where siblings
/// that overlap vertically share a line, and each line from left to right; so side-by-side panes
/// read left first. Siblings without a frame keep their order at the end.
fn reading_order<T>(mut siblings: Vec<Sibling<T>>) -> Vec<Sibling<T>> {
    let top = |frame: &Option<DisplayBounds>| frame.as_ref().map_or(f64::INFINITY, |frame| frame.y);
    siblings.sort_by(|(left, _), (right, _)| top(left).total_cmp(&top(right)));
    let mut lines: Vec<(f64, Vec<Sibling<T>>)> = Vec::new();
    for sibling in siblings {
        let span = sibling
            .0
            .as_ref()
            .map(|frame| (frame.y, frame.y + frame.height));
        match (span, lines.last_mut()) {
            (Some((top, bottom)), Some((line_bottom, members))) if top < *line_bottom => {
                *line_bottom = line_bottom.max(bottom);
                members.push(sibling);
            }
            (span, _) => lines.push((
                span.map_or(f64::INFINITY, |(_, bottom)| bottom),
                vec![sibling],
            )),
        }
    }
    let left =
        |frame: &Option<DisplayBounds>| frame.as_ref().map_or(f64::INFINITY, |frame| frame.x);
    lines
        .into_iter()
        .flat_map(|(_, mut members)| {
            members.sort_by(|(a, _), (b, _)| left(a).total_cmp(&left(b)));
            members
        })
        .collect()
}

/// Containers are listed only when they say something; controls and leaves always are, so an
/// unlabeled icon button still appears with its frame. Empty groups and text that repeats
/// `context` add nothing.
fn listed(attributes: &Attributes, leaf: bool, context: Option<&str>) -> bool {
    let role = attributes.string("AXRole").unwrap_or_default();
    if role == "AXStaticText" && attributes.string("AXValue").as_deref() == context {
        return false;
    }
    leaf && role != "AXGroup"
        || CONTROL_ROLES.contains(&role.as_str())
        || ["AXTitle", "AXDescription", "AXHelp", "AXValue"]
            .into_iter()
            .any(|name| attributes.string(name).is_some())
        || attributes.flag("AXSelected") == Some(true)
}

fn intersects(frame: &DisplayBounds, window: &DisplayBounds) -> bool {
    frame.x < window.x + window.width
        && frame.y < window.y + window.height
        && frame.x + frame.width > window.x
        && frame.y + frame.height > window.y
}

fn element_bounds(element: &CFType) -> Option<DisplayBounds> {
    let value = |name, kind, out: *mut c_void| {
        ax_attribute(element, name)
            .filter(|value| CFGetTypeID(Some(value)) == unsafe { AXValueGetTypeID() })
            .is_some_and(|value| unsafe { AXValueGetValue(cf(&value), kind, out) })
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

/// Whether the element offers this accessibility action.
fn has_action(element: &CFType, action: &str) -> bool {
    let mut names = std::ptr::null_mut();
    if unsafe { AXUIElementCopyActionNames(cf(element), &raw mut names) } != 0 {
        return false;
    }
    let Some(names) = NonNull::new(names.cast::<CFArray>()) else {
        return false;
    };
    // Copy functions return a +1 reference; action names are strings.
    let names: CFRetained<CFArray<CFType>> =
        unsafe { CFRetained::cast_unchecked(CFRetained::from_raw(names)) };
    names.iter().any(|name| {
        name.downcast::<CFString>()
            .is_ok_and(|name| name.to_string() == action)
    })
}

fn disabled(element: &CFType) -> bool {
    ax_attribute(element, "AXEnabled")
        .and_then(|value| value.downcast::<CFBoolean>().ok())
        .is_some_and(|enabled| !enabled.as_bool())
}

/// The child's position among the parent's children.
fn child_index(parent: &CFType, child: &CFType) -> Option<u16> {
    let children = ax_attribute(parent, "AXChildren")?
        .downcast::<CFArray>()
        .ok()?;
    // Children of an accessibility element are elements.
    let children: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(children) };
    let index = children.iter().position(|candidate| *candidate == *child)?;
    u16::try_from(index).ok()
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "Accessibility hit-testing takes f32 desktop points."
)]
const fn ax_coordinate(value: f64) -> f32 {
    value as f32
}

/// Whether the app's focused element hides what is typed, such as a password field.
fn secure_focus(process_id: i32) -> bool {
    let Some(element) =
        ax_application(process_id).and_then(|app| ax_element(&app, "AXFocusedUIElement"))
    else {
        return false;
    };
    ["AXRole", "AXSubrole"].into_iter().any(|name| {
        ax_attribute(&element, name)
            .and_then(|value| value.downcast::<CFString>().ok())
            .is_some_and(|role| role.to_string() == "AXSecureTextField")
    })
}

/// Address an event to exactly this window, at its point inside the window. `AppKit` routes it there
/// even behind other windows; keys still go to the window the app has focused.
fn address(event: &CGEvent, window: &NativeWindow) {
    CGEvent::set_integer_value_field(Some(event), WINDOW_NUMBER, i64::from(window.id));
    let point = CGEvent::location(Some(event));
    let local = CGPoint {
        x: point.x - window.bounds.x,
        y: point.y - window.bounds.y,
    };
    unsafe { CGEventSetWindowLocation(cf(event), local) };
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
