//! Window identity and action rules shared by every platform.
//!
//! An action reaches only the exact window token it names. Keys need that window to be the one
//! its process sends keys to. Pointer input addressed to the window reaches it wherever it is;
//! shared pointer input also needs it focused and uncovered. Captures never include pixels
//! outside the requested window or region.
use std::{io::Cursor, thread, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{
    ComputerAction, ComputerElement, ComputerError, ComputerResult, ComputerTarget, DisplayBounds,
    ElementRoot, ElementState, Key, MAX_ELEMENTS, Modifier, MouseButton, Permission,
    PermissionStatus, PressMethod,
    desktop::{Desktop, Image, InputEvent, InputRoute, NativeWindow, pixel_count, pixel_edge},
};

/// Quartz rejects text events longer than 20 UTF-16 units; every platform uses the same pieces.
pub const MAX_TEXT_UNITS: usize = 20;
/// The longest edge of a returned screenshot, in pixels.
pub const MAX_CAPTURE_EDGE: f64 = 1600.0;
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
// AppKit may merge adjacent synthetic text events that arrive without input spacing.
const TEXT_SPACING: Duration = Duration::from_millis(50);
// Focusing a window is asynchronous; focus is confirmed within this many short waits.
const FOCUS_CHECKS: usize = 10;
const FOCUS_WAIT: Duration = Duration::from_millis(20);

/// Narrower or shorter windows, in desktop units, are tooltips, drag images or stray helpers rather
/// than somewhere to work. They still cover points. Deliberate limit: lower it if a real app needs
/// input in a smaller window.
const MIN_TARGET_EDGE: f64 = 48.0;

/// Windows an agent can work in, in platform observation order.
pub fn targets(desktop: &dyn Desktop) -> Vec<ComputerTarget> {
    desktop
        .windows()
        .into_iter()
        .filter(|window| {
            window.bounds.width >= MIN_TARGET_EDGE && window.bounds.height >= MIN_TARGET_EDGE
        })
        .filter_map(|window| window.app)
        .take(256)
        .collect()
}

/// A PNG of the application's icon, `side` pixels square.
pub fn icon_png(desktop: &dyn Desktop, bundle_id: &str, side: u32) -> Option<Vec<u8>> {
    let icon = desktop.icon(bundle_id, side)?;
    encode_png(&icon).ok()
}

/// Run one validated action against its exact window.
///
/// # Errors
/// Returns identity, focus, permission, secure-input and platform errors.
pub fn execute(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    action: &ComputerAction,
) -> Result<ComputerResult, ComputerError> {
    if desktop.secure_input() {
        return Err(ComputerError::SecureInput);
    }
    match action {
        ComputerAction::Snapshot | ComputerAction::SnapshotRegion { .. } => {
            snapshot(desktop, target, action)
        }
        ComputerAction::Focus => focus(desktop, target),
        ComputerAction::Click { x, y, button } => click(desktop, target, *x, *y, *button),
        ComputerAction::Move { x, y } => {
            pointer(desktop, target, *x, *y, &InputEvent::Move { x: *x, y: *y })
        }
        ComputerAction::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => pointer(
            desktop,
            target,
            *x,
            *y,
            &InputEvent::Scroll {
                x: *x,
                y: *y,
                delta_x: *delta_x,
                delta_y: *delta_y,
            },
        ),
        ComputerAction::TypeText { text } => type_text(desktop, target, text),
        ComputerAction::Elements => elements(desktop, target, ElementRoot::Window),
        ComputerAction::MenuBar => elements(desktop, target, ElementRoot::MenuBar),
        ComputerAction::SetValue { element, value } => {
            let window = validate(desktop, target, Need::Element)?;
            let current = desktop
                .element(&window, element.root, &element.path)?
                .filter(|current| current.same_as(element))
                .ok_or(ComputerError::StaleElement)?;
            if current.states.contains(&ElementState::Disabled) {
                return Err(ComputerError::InvalidAction(
                    "the element is disabled".into(),
                ));
            }
            desktop.set_value(&window, &element.path, value)?;
            Ok(ComputerResult::Posted)
        }
        ComputerAction::Press {
            element,
            allow_click,
        } => press(desktop, target, element, *allow_click),
        ComputerAction::Key { key, modifiers } => press_key(desktop, target, *key, modifiers),
    }
}

/// What an action needs from its window beyond identity.
#[derive(Clone, Copy)]
enum Need {
    /// Captures and element listings only read the window.
    Read,
    /// The OS delivers an accessibility action to the element itself, wherever its window is.
    Element,
    /// Keys reach the window the target's process sends keys to.
    Keys,
    /// A pointer event at this desktop point.
    Pointer(f64, f64),
}

/// The current window for a token, once it meets what the action needs.
fn validate(
    desktop: &dyn Desktop,
    expected: &ComputerTarget,
    need: Need,
) -> Result<NativeWindow, ComputerError> {
    expected.validate()?;
    let windows = desktop.windows();
    let window = windows
        .iter()
        .find(|window| {
            window.id == expected.window_id
                && window
                    .app
                    .as_ref()
                    .is_some_and(|app| app.native_id == expected.native_id)
        })
        .ok_or(ComputerError::TargetUnavailable)?;
    let current = window
        .app
        .as_ref()
        .ok_or(ComputerError::TargetUnavailable)?;
    if current.process_id != expected.process_id
        || current.bundle_id != expected.bundle_id
        || current.launch_time.to_bits() != expected.launch_time.to_bits()
        || current.bounds != expected.bounds
    {
        return Err(ComputerError::StaleTarget);
    }
    if matches!(need, Need::Read) {
        return Ok(window.clone());
    }
    require(desktop.status().accessibility, Permission::Accessibility)?;
    if desktop.secure_input() {
        return Err(ComputerError::SecureInput);
    }
    match (need, desktop.route()) {
        (Need::Read | Need::Element, _) => {}
        (Need::Keys | Need::Pointer(_, _), _) if current.is_semantic_only() => {
            return Err(ComputerError::Unsupported);
        }
        (Need::Keys, _) => receives_keys(desktop, &windows, current)?,
        (Need::Pointer(x, y), route) => {
            if !contains(&window.bounds, x, y) {
                return Err(ComputerError::InvalidAction(
                    "coordinates are outside the target".into(),
                ));
            }
            // Shared pointer input lands on whatever is in front at the point.
            if route == InputRoute::Shared {
                receives_keys(desktop, &windows, current)?;
                uncovered(&windows, window, x, y)?;
            }
        }
    }
    Ok(window.clone())
}

/// The target must be the window its process sends keys to. The OS reports that window by
/// bounds, so two windows of one process at the same bounds are ambiguous.
fn receives_keys(
    desktop: &dyn Desktop,
    windows: &[NativeWindow],
    target: &ComputerTarget,
) -> Result<(), ComputerError> {
    if desktop.focus(target.process_id).as_ref() != Some(&target.bounds)
        || same_frame_windows(windows, target) != 1
    {
        return Err(ComputerError::TargetNotFocused);
    }
    Ok(())
}

fn same_frame_windows(windows: &[NativeWindow], target: &ComputerTarget) -> usize {
    windows
        .iter()
        .filter(|window| {
            window.app.as_ref().is_some_and(|app| {
                !app.is_semantic_only()
                    && app.process_id == target.process_id
                    && app.bounds == target.bounds
            })
        })
        .count()
}

const fn require(status: PermissionStatus, permission: Permission) -> Result<(), ComputerError> {
    match status {
        PermissionStatus::Granted => Ok(()),
        PermissionStatus::NotGranted => Err(ComputerError::PermissionDenied(permission)),
        PermissionStatus::Unsupported => Err(ComputerError::Unsupported),
    }
}

/// Shared pointer input must land on no other visible window.
fn uncovered(
    windows: &[NativeWindow],
    window: &NativeWindow,
    x: f64,
    y: f64,
) -> Result<(), ComputerError> {
    let hit = windows
        .iter()
        .find(|candidate| candidate.visible && contains(&candidate.bounds, x, y));
    if hit.is_none_or(|hit| hit.id != window.id) {
        return Err(ComputerError::TargetNotFocused);
    }
    Ok(())
}

fn contains(bounds: &DisplayBounds, x: f64, y: f64) -> bool {
    x >= bounds.x && y >= bounds.y && x < bounds.x + bounds.width && y < bounds.y + bounds.height
}

fn pointer(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    x: f64,
    y: f64,
    event: &InputEvent,
) -> Result<ComputerResult, ComputerError> {
    let window = validate(desktop, target, Need::Pointer(x, y))?;
    desktop.post(&window, event)?;
    Ok(ComputerResult::Posted)
}

/// `AppKit` ignores a click in a background app's window on views that need their window active,
/// such as web content, and macOS will not activate the app for it. A left click there presses
/// the element under the point instead when it has its own press action. Every click on the
/// frontmost app is posted as it is, where a first click keys its window as it would for a user.
fn click(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    x: f64,
    y: f64,
    button: MouseButton,
) -> Result<ComputerResult, ComputerError> {
    let window = validate(desktop, target, Need::Pointer(x, y))?;
    if button == MouseButton::Left
        && desktop.route() == InputRoute::Window
        && desktop.frontmost() != Some(target.process_id)
        && let Some(path) = desktop.pressable_at(&window, x, y)?
        && desktop.press(&window, ElementRoot::Window, &path)?
    {
        return Ok(ComputerResult::Posted);
    }
    desktop.post(&window, &InputEvent::Click { x, y, button })?;
    Ok(ComputerResult::Posted)
}

fn elements(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    root: ElementRoot,
) -> Result<ComputerResult, ComputerError> {
    require(desktop.status().accessibility, Permission::Accessibility)?;
    let window = validate(desktop, target, Need::Read)?;
    let token = window.app.clone().ok_or(ComputerError::TargetUnavailable)?;
    let (elements, omitted) = desktop.elements(&window, root, MAX_ELEMENTS)?;
    // The elements describe the window as it was listed; a move or resize makes them stale.
    validate(desktop, target, Need::Read)?;
    Ok(ComputerResult::Elements {
        elements,
        omitted,
        target: Box::new(token),
    })
}

/// The element's own press action first. Apps that draw their own controls often have none, so
/// those get a click at the element's center under the usual pointer rules. Menu bar elements have
/// no center and are never clicked.
fn press(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    element: &ComputerElement,
    allow_click: bool,
) -> Result<ComputerResult, ComputerError> {
    let window = validate(desktop, target, Need::Element)?;
    let current = desktop
        .element(&window, element.root, &element.path)?
        .filter(|current| current.same_as(element))
        .ok_or(ComputerError::StaleElement)?;
    if current.states.contains(&ElementState::Disabled) {
        return Err(ComputerError::InvalidAction(
            "the element is disabled".into(),
        ));
    }
    if desktop.press(&window, element.root, &element.path)? {
        return Ok(ComputerResult::Pressed {
            method: PressMethod::Action,
        });
    }
    let Some((x, y)) = element.center() else {
        return Err(ComputerError::InvalidAction(
            "the menu item has no supported accessibility action".into(),
        ));
    };
    if !allow_click {
        return Err(ComputerError::InvalidAction(
            "the element has no supported accessibility action; pointer fallback is disabled"
                .into(),
        ));
    }
    click(desktop, target, x, y, MouseButton::Left)?;
    Ok(ComputerResult::Pressed {
        method: PressMethod::Click,
    })
}

/// A background app's menus ignore Command shortcuts posted to it, so a Command shortcut for an
/// app that is not frontmost presses the menu item with that shortcut instead. A disabled item
/// fails: its app has no window to act on until it comes to the front. Other keys, shortcuts in no
/// menu, and every key for the frontmost app are posted.
fn press_key(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    key: Key,
    modifiers: &[Modifier],
) -> Result<ComputerResult, ComputerError> {
    let window = validate(desktop, target, Need::Keys)?;
    if modifiers.contains(&Modifier::Command)
        && desktop.route() == InputRoute::Window
        && desktop.frontmost() != Some(target.process_id)
        && let Some(item) = desktop.menu_shortcut(&window, key, modifiers)?
    {
        let name = item.name.as_deref().unwrap_or("untitled");
        if item.states.contains(&ElementState::Disabled) {
            return Err(ComputerError::InvalidAction(format!(
                "the shortcut's menu command `{name}` is disabled; apps in the background \
                 disable commands that act on a window, so press the window's own control instead"
            )));
        }
        if !desktop.press(&window, ElementRoot::MenuBar, &item.path)? {
            return Err(ComputerError::InvalidAction(format!(
                "the shortcut's menu command `{name}` has no supported accessibility action"
            )));
        }
        return Ok(ComputerResult::Pressed {
            method: PressMethod::Action,
        });
    }
    desktop.post(
        &window,
        &InputEvent::Key {
            key,
            modifiers: modifiers.to_vec(),
        },
    )?;
    Ok(ComputerResult::Posted)
}

fn type_text(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    text: &str,
) -> Result<ComputerResult, ComputerError> {
    for (index, piece) in text_pieces(text).into_iter().enumerate() {
        if index > 0 {
            thread::sleep(TEXT_SPACING);
        }
        let window = validate(desktop, target, Need::Keys)?;
        desktop.post(&window, &InputEvent::Text(piece))?;
    }
    Ok(ComputerResult::Posted)
}

/// Split text into pieces of at most [`MAX_TEXT_UNITS`] UTF-16 units without splitting a
/// character.
#[must_use]
pub fn text_pieces(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut units: usize = 0;
    for character in text.chars() {
        let width = character.len_utf16();
        if units.saturating_add(width) > MAX_TEXT_UNITS && !piece.is_empty() {
            pieces.push(std::mem::take(&mut piece));
            units = 0;
        }
        piece.push(character);
        units = units.saturating_add(width);
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

/// Make the window the one its process sends keys to. Pointer input addressed to a window needs
/// no focus, so this matters for keys, or where input drives the shared pointer.
fn focus(desktop: &dyn Desktop, target: &ComputerTarget) -> Result<ComputerResult, ComputerError> {
    if target.is_semantic_only() {
        return Err(ComputerError::Unsupported);
    }
    require(desktop.status().accessibility, Permission::Accessibility)?;
    let window = validate(desktop, target, Need::Read)?;
    let app = window
        .app
        .as_ref()
        .ok_or(ComputerError::TargetUnavailable)?;
    if same_frame_windows(&desktop.windows(), app) != 1 {
        return Err(ComputerError::TargetNotFocused);
    }
    desktop.raise(&window)?;
    for _ in 0..FOCUS_CHECKS {
        if validate(desktop, target, Need::Keys).is_ok() {
            return Ok(ComputerResult::Posted);
        }
        thread::sleep(FOCUS_WAIT);
    }
    validate(desktop, target, Need::Keys)?;
    Ok(ComputerResult::Posted)
}

fn snapshot(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    action: &ComputerAction,
) -> Result<ComputerResult, ComputerError> {
    if target.is_semantic_only() {
        return Err(ComputerError::Unsupported);
    }
    require(
        desktop.status().screen_recording,
        Permission::ScreenRecording,
    )?;
    let window = validate(desktop, target, Need::Read)?;
    let token = window.app.clone().ok_or(ComputerError::TargetUnavailable)?;
    let bounds = &window.bounds;
    let scale = (MAX_CAPTURE_EDGE / bounds.width.max(bounds.height)).min(1.0);
    let region = match action {
        ComputerAction::SnapshotRegion { rect } => Some(rect.clone()),
        _ => None,
    };
    let image = desktop.capture(
        &window,
        pixel_count(bounds.width * scale),
        pixel_count(bounds.height * scale),
        region.is_none(),
    )?;
    require(
        desktop.status().screen_recording,
        Permission::ScreenRecording,
    )?;
    if desktop.secure_input() {
        return Err(ComputerError::SecureInput);
    }
    validate(desktop, target, Need::Read)?;
    let (image, actual) = match &region {
        Some(rect) => {
            let (cropped, actual) = crop(&image, bounds, rect)?;
            (cropped, Some(actual))
        }
        None => (image, None),
    };
    let png = encode_png(&image)?;
    Ok(ComputerResult::Snapshot {
        png_base64: STANDARD.encode(png),
        region: actual,
        requested_region: region,
        pixel_width: image.width,
        pixel_height: image.height,
        target: Box::new(token),
    })
}

/// Crop a region out of a window capture, snapping inward so a fractional edge never includes
/// pixels outside the region. Returns the crop and the desktop rectangle it actually covers.
fn crop(
    image: &Image,
    bounds: &DisplayBounds,
    region: &DisplayBounds,
) -> Result<(Image, DisplayBounds), ComputerError> {
    let sx = f64::from(image.width) / bounds.width;
    let sy = f64::from(image.height) / bounds.height;
    let left = ((region.x - bounds.x) * sx).ceil().max(0.0);
    let top = ((region.y - bounds.y) * sy).ceil().max(0.0);
    let right = ((region.x + region.width - bounds.x) * sx)
        .floor()
        .min(f64::from(image.width));
    let bottom = ((region.y + region.height - bounds.y) * sy)
        .floor()
        .min(f64::from(image.height));
    if right <= left || bottom <= top {
        return Err(ComputerError::InvalidAction(
            "capture rectangle has no complete pixels".into(),
        ));
    }
    let (x0, y0) = (pixel_edge(left), pixel_edge(top));
    let cropped = image
        .cropped(
            x0,
            y0,
            pixel_edge(right).saturating_sub(x0),
            pixel_edge(bottom).saturating_sub(y0),
        )
        .ok_or_else(|| ComputerError::Platform("captured image is truncated".into()))?;
    let actual = DisplayBounds {
        x: bounds.x + left / sx,
        y: bounds.y + top / sy,
        width: (right - left) / sx,
        height: (bottom - top) / sy,
    };
    Ok((cropped, actual))
}

fn encode_png(image: &Image) -> Result<Vec<u8>, ComputerError> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(Cursor::new(&mut png), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let invalid = |_| ComputerError::Platform("screenshot could not be encoded".into());
    encoder
        .write_header()
        .map_err(invalid)?
        .write_image_data(&image.rgba)
        .map_err(invalid)?;
    if png.len() > MAX_PNG_BYTES {
        return Err(ComputerError::Platform(
            "screenshot exceeds the encoding limit".into(),
        ));
    }
    Ok(png)
}
