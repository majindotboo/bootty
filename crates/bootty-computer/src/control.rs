//! Window identity and action rules shared by every platform.
//!
//! An action reaches only the exact window token it names, input needs that window focused and
//! uncovered, and captures never include pixels outside the requested window or region.
use std::{io::Cursor, thread, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{
    ComputerAction, ComputerError, ComputerResult, ComputerTarget, DisplayBounds, Permission,
    PermissionStatus,
    desktop::{Desktop, Image, InputEvent, NativeWindow, pixel_count, pixel_edge},
};

/// Quartz rejects text events longer than 20 UTF-16 units; every platform uses the same pieces.
pub const MAX_TEXT_UNITS: usize = 20;
/// The longest edge of a returned screenshot, in pixels.
pub const MAX_CAPTURE_EDGE: f64 = 1600.0;
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
// AppKit may merge adjacent synthetic text events that arrive without input spacing.
const TEXT_SPACING: Duration = Duration::from_millis(50);
// Raising a window is asynchronous; focus is confirmed within this many short waits.
const FOCUS_CHECKS: usize = 10;
const FOCUS_WAIT: Duration = Duration::from_millis(20);

pub fn targets(desktop: &dyn Desktop) -> Vec<ComputerTarget> {
    desktop
        .windows()
        .into_iter()
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
        ComputerAction::Click { x, y, button } => pointer(
            desktop,
            target,
            *x,
            *y,
            &InputEvent::Click {
                x: *x,
                y: *y,
                button: *button,
            },
        ),
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
        ComputerAction::Key { key, modifiers } => {
            let window = validate(desktop, target, true)?;
            desktop.post(
                &window,
                &InputEvent::Key {
                    key: *key,
                    modifiers: modifiers.clone(),
                },
            )?;
            Ok(ComputerResult::Posted)
        }
    }
}

/// The current window for a token. Input additionally needs the window to be the focused window
/// of the frontmost application and unambiguous among that application's windows.
fn validate(
    desktop: &dyn Desktop,
    expected: &ComputerTarget,
    input: bool,
) -> Result<NativeWindow, ComputerError> {
    expected.validate()?;
    let windows = desktop.windows();
    let window = windows
        .iter()
        .find(|window| window.id == expected.window_id)
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
    if input {
        require(desktop.status().accessibility, Permission::Accessibility)?;
        if desktop.secure_input() {
            return Err(ComputerError::SecureInput);
        }
        let focus = desktop.focus().ok_or(ComputerError::TargetNotFocused)?;
        if focus.process_id != current.process_id || focus.window.as_ref() != Some(&current.bounds)
        {
            return Err(ComputerError::TargetNotFocused);
        }
        // Focus is matched by bounds; two windows of one app at the same bounds are ambiguous.
        if same_frame_windows(&windows, current) != 1 {
            return Err(ComputerError::TargetNotFocused);
        }
    }
    Ok(window.clone())
}

fn same_frame_windows(windows: &[NativeWindow], target: &ComputerTarget) -> usize {
    windows
        .iter()
        .filter(|window| {
            window.app.as_ref().is_some_and(|app| {
                app.process_id == target.process_id && app.bounds == target.bounds
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

/// A pointer action must land inside the window and on no other visible window.
fn uncovered(
    desktop: &dyn Desktop,
    window: &NativeWindow,
    x: f64,
    y: f64,
) -> Result<(), ComputerError> {
    if !contains(&window.bounds, x, y) {
        return Err(ComputerError::InvalidAction(
            "coordinates are outside the target".into(),
        ));
    }
    let hit = desktop
        .windows()
        .into_iter()
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
    let window = validate(desktop, target, true)?;
    uncovered(desktop, &window, x, y)?;
    desktop.post(&window, event)?;
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
        let window = validate(desktop, target, true)?;
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

fn focus(desktop: &dyn Desktop, target: &ComputerTarget) -> Result<ComputerResult, ComputerError> {
    require(desktop.status().accessibility, Permission::Accessibility)?;
    let window = validate(desktop, target, false)?;
    let app = window
        .app
        .as_ref()
        .ok_or(ComputerError::TargetUnavailable)?;
    if same_frame_windows(&desktop.windows(), app) != 1 {
        return Err(ComputerError::TargetNotFocused);
    }
    desktop.raise(&window)?;
    for _ in 0..FOCUS_CHECKS {
        if validate(desktop, target, true).is_ok() {
            return Ok(ComputerResult::Posted);
        }
        thread::sleep(FOCUS_WAIT);
    }
    validate(desktop, target, true)?;
    Ok(ComputerResult::Posted)
}

fn snapshot(
    desktop: &dyn Desktop,
    target: &ComputerTarget,
    action: &ComputerAction,
) -> Result<ComputerResult, ComputerError> {
    require(
        desktop.status().screen_recording,
        Permission::ScreenRecording,
    )?;
    let window = validate(desktop, target, false)?;
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
    validate(desktop, target, false)?;
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
