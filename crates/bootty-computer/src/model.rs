use serde::{Deserialize, Serialize};

/// The desktop host creates this capability from its user-owned setting.
#[derive(Clone, Copy, Debug)]
pub struct ComputerAccess(bool);

impl ComputerAccess {
    #[must_use]
    pub const fn from_user_setting(enabled: bool) -> Self {
        Self(enabled)
    }

    pub(crate) const fn enabled(self) -> bool {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Accessibility,
    ScreenRecording,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    Granted,
    NotGranted,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ComputerStatus {
    pub accessibility: PermissionStatus,
    pub screen_recording: PermissionStatus,
    pub secure_input: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Return,
    Tab,
    Escape,
    Space,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    #[serde(rename = "0")]
    Digit0,
    #[serde(rename = "1")]
    Digit1,
    #[serde(rename = "2")]
    Digit2,
    #[serde(rename = "3")]
    Digit3,
    #[serde(rename = "4")]
    Digit4,
    #[serde(rename = "5")]
    Digit5,
    #[serde(rename = "6")]
    Digit6,
    #[serde(rename = "7")]
    Digit7,
    #[serde(rename = "8")]
    Digit8,
    #[serde(rename = "9")]
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    Minus,
    Equal,
    LeftBracket,
    RightBracket,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    Backtick,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Command,
    Control,
    Option,
    Shift,
}

/// Coordinates are global desktop points inside the selected window.
/// Snapshot bounds map PNG pixels to those points.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerAction {
    /// Raise only the explicitly selected window before subsequent input.
    Focus,
    Snapshot,
    /// Capture only this absolute desktop rectangle inside the exact window.
    SnapshotRegion {
        rect: DisplayBounds,
    },
    Click {
        x: f64,
        y: f64,
        button: MouseButton,
    },
    Move {
        x: f64,
        y: f64,
    },
    TypeText {
        text: String,
    },
    Key {
        key: Key,
        modifiers: Vec<Modifier>,
    },
    Scroll {
        x: f64,
        y: f64,
        delta_x: i32,
        delta_y: i32,
    },
}

impl ComputerAction {
    #[must_use]
    pub const fn is_capture(&self) -> bool {
        matches!(self, Self::Snapshot | Self::SnapshotRegion { .. })
    }

    /// Validate bounded input without touching the OS.
    ///
    /// # Errors
    /// Returns invalid-action errors for non-finite coordinates or oversized input.
    pub fn validate(&self) -> Result<(), ComputerError> {
        match self {
            Self::SnapshotRegion { rect } => rect.validate(),
            Self::Click { x, y, .. } | Self::Move { x, y } | Self::Scroll { x, y, .. }
                if !x.is_finite() || !y.is_finite() =>
            {
                Err(ComputerError::InvalidAction(
                    "coordinates must be finite".into(),
                ))
            }
            Self::TypeText { text } if text.is_empty() || text.len() > 4_096 => Err(
                ComputerError::InvalidAction("text must contain 1–4096 bytes".into()),
            ),
            Self::Key { modifiers, .. } if modifiers.len() > 4 => Err(
                ComputerError::InvalidAction("at most four modifiers are allowed".into()),
            ),
            Self::Scroll {
                delta_x, delta_y, ..
            } if delta_x.unsigned_abs() > 10_000 || delta_y.unsigned_abs() > 10_000 => Err(
                ComputerError::InvalidAction("scroll delta exceeds 10000 pixels".into()),
            ),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DisplayBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl DisplayBounds {
    pub(crate) fn validate(&self) -> Result<(), ComputerError> {
        if ![self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            || self.width <= 0.0
            || self.height <= 0.0
            || !(self.x + self.width).is_finite()
            || !(self.y + self.height).is_finite()
        {
            return Err(ComputerError::InvalidAction(
                "capture rectangle is invalid".into(),
            ));
        }
        Ok(())
    }

    fn contains_rect(&self, rect: &Self) -> bool {
        rect.x >= self.x
            && rect.y >= self.y
            && rect.x + rect.width <= self.x + self.width
            && rect.y + rect.height <= self.y + self.height
    }
}

/// Geometry observed from the exact native host before dispatch, in window-frame points.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostCaptureRegion {
    pub frame_width: f64,
    pub frame_height: f64,
    pub rect: DisplayBounds,
}

impl HostCaptureRegion {
    /// Validate a bounded frame-local crop before submitting capture work.
    ///
    /// # Errors
    /// Rejects invalid frames and rectangles outside that frame.
    pub fn validate(&self) -> Result<(), ComputerError> {
        let frame = DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: self.frame_width,
            height: self.frame_height,
        };
        frame.validate()?;
        self.rect.validate()?;
        if !frame.contains_rect(&self.rect) {
            return Err(ComputerError::InvalidAction(
                "capture rectangle is outside the target".into(),
            ));
        }
        Ok(())
    }

    /// Resolve frame-local page geometry against a freshly observed exact window token.
    ///
    /// # Errors
    /// Rejects resized windows, invalid geometry, and partially out-of-window rectangles.
    pub fn resolve(&self, target: &ComputerTarget) -> Result<DisplayBounds, ComputerError> {
        self.validate()?;
        target.validate()?;
        if self.frame_width.to_bits() != target.bounds.width.to_bits()
            || self.frame_height.to_bits() != target.bounds.height.to_bits()
        {
            return Err(ComputerError::StaleTarget);
        }
        let rect = DisplayBounds {
            x: target.bounds.x + self.rect.x,
            y: target.bounds.y + self.rect.y,
            width: self.rect.width,
            height: self.rect.height,
        };
        target.validate_action(&ComputerAction::SnapshotRegion { rect: rect.clone() })?;
        Ok(rect)
    }
}

/// An observed window and its process incarnation. Moving or replacing it invalidates the token.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComputerTarget {
    pub window_id: u32,
    pub process_id: i32,
    pub bundle_id: String,
    pub launch_time: f64,
    pub bounds: DisplayBounds,
    pub title: Option<String>,
}

impl ComputerTarget {
    /// Validate the shape of an observed window token without touching the OS.
    ///
    /// # Errors
    /// Returns an invalid-target error for missing identity or invalid geometry.
    pub fn validate(&self) -> Result<(), ComputerError> {
        let bounds = &self.bounds;
        if self.window_id == 0
            || self.process_id <= 0
            || self.bundle_id.is_empty()
            || self.bundle_id.len() > 255
            || !self.launch_time.is_finite()
            || !bounds.x.is_finite()
            || !bounds.y.is_finite()
            || !bounds.width.is_finite()
            || !bounds.height.is_finite()
            || bounds.width <= 0.0
            || bounds.height <= 0.0
            || self.title.as_ref().is_some_and(|title| title.len() > 2048)
        {
            return Err(ComputerError::InvalidTarget);
        }
        Ok(())
    }

    /// Validate bounded input against this window's geometry without touching the OS.
    ///
    /// # Errors
    /// Returns invalid-target or invalid-action errors, including pointer points outside the window.
    pub fn validate_action(&self, action: &ComputerAction) -> Result<(), ComputerError> {
        action.validate()?;
        self.validate()?;
        match action {
            ComputerAction::SnapshotRegion { rect } if !self.bounds.contains_rect(rect) => Err(
                ComputerError::InvalidAction("capture rectangle is outside the target".into()),
            ),
            ComputerAction::Click { x, y, .. }
            | ComputerAction::Move { x, y }
            | ComputerAction::Scroll { x, y, .. }
                if !self.contains(*x, *y) =>
            {
                Err(ComputerError::InvalidAction(
                    "coordinates are outside the target".into(),
                ))
            }
            _ => Ok(()),
        }
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.bounds.x
            && y >= self.bounds.y
            && x < self.bounds.x + self.bounds.width
            && y < self.bounds.y + self.bounds.height
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerResult {
    /// Input was posted to the OS. Application acceptance is observed by a subsequent snapshot.
    Posted,
    Snapshot {
        png_base64: String,
        /// Actual absolute source rectangle after inward pixel snapping.
        /// Absent for whole-window snapshots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<DisplayBounds>,
        /// Requested crop, retained separately from its actual pixel geometry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_region: Option<DisplayBounds>,
        pixel_width: u32,
        pixel_height: u32,
        target: Box<ComputerTarget>,
    },
}

impl ComputerResult {
    pub(crate) fn validate(
        &self,
        target: &ComputerTarget,
        action: &ComputerAction,
    ) -> Result<(), ComputerError> {
        match (self, action) {
            (Self::Posted, ComputerAction::Snapshot | ComputerAction::SnapshotRegion { .. })
            | (
                Self::Snapshot { .. },
                ComputerAction::Click { .. }
                | ComputerAction::Move { .. }
                | ComputerAction::TypeText { .. }
                | ComputerAction::Key { .. }
                | ComputerAction::Scroll { .. },
            ) => Err(ComputerError::Helper(
                "helper returned an unexpected result".into(),
            )),
            (
                Self::Snapshot {
                    target: captured,
                    pixel_width,
                    pixel_height,
                    png_base64,
                    region,
                    requested_region,
                },
                _,
            ) => {
                captured.validate()?;
                let expected_region = match action {
                    ComputerAction::SnapshotRegion { rect } => Some(rect),
                    _ => None,
                };
                if requested_region.as_ref() != expected_region {
                    return Err(ComputerError::Helper(
                        "capture source geometry changed".into(),
                    ));
                }
                match (expected_region, region) {
                    (Some(requested), Some(actual)) => {
                        actual.validate().map_err(|_| {
                            ComputerError::Helper("invalid capture geometry".into())
                        })?;
                        if !requested.contains_rect(actual)
                            || !captured.bounds.contains_rect(actual)
                        {
                            return Err(ComputerError::Helper(
                                "capture lies outside the requested region".into(),
                            ));
                        }
                    }
                    (None, None) => {}
                    _ => {
                        return Err(ComputerError::Helper(
                            "capture source geometry missing".into(),
                        ));
                    }
                }

                if captured.window_id != target.window_id
                    || captured.process_id != target.process_id
                    || captured.bundle_id != target.bundle_id
                    || captured.launch_time.to_bits() != target.launch_time.to_bits()
                    || captured.bounds != target.bounds
                {
                    return Err(ComputerError::StaleTarget);
                }
                if *pixel_width == 0
                    || *pixel_height == 0
                    || *pixel_width > 1600
                    || *pixel_height > 1600
                    || png_base64.is_empty()
                    || png_base64.len() > 11_184_812
                {
                    return Err(ComputerError::Helper(
                        "screenshot exceeds the image limit".into(),
                    ));
                }
                Ok(())
            }
            (Self::Posted, _) => Ok(()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ComputerError {
    #[error("computer use is disabled; enable it in Bootty settings")]
    Disabled,
    #[error("computer use is unsupported on this platform")]
    Unsupported,
    #[error("permission not granted: {0:?}")]
    PermissionDenied(Permission),
    #[error("invalid computer target")]
    InvalidTarget,
    #[error("computer target is unavailable")]
    TargetUnavailable,
    #[error("computer target changed; select it again")]
    StaleTarget,
    #[error("computer target is not the focused window")]
    TargetNotFocused,
    #[error("secure input is active; computer use is paused")]
    SecureInput,
    #[error("invalid computer action: {0}")]
    InvalidAction(String),
    #[error("computer-control helper timed out; the action was not retried")]
    Timeout,
    #[error("computer-control helper failed: {0}")]
    Helper(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
