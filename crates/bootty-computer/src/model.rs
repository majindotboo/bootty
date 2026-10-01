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
    C,
    V,
    X,
    Z,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Command,
    Control,
    Option,
    Shift,
}

/// Coordinates are global desktop points. Snapshot bounds map PNG pixels to points.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerAction {
    Snapshot {
        display_id: Option<u32>,
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
    Activate {
        bundle_id: String,
    },
}

impl ComputerAction {
    pub(crate) fn validate(&self) -> Result<(), ComputerError> {
        match self {
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
            Self::Activate { bundle_id } if bundle_id.is_empty() || bundle_id.len() > 255 => Err(
                ComputerError::InvalidAction("invalid application bundle identifier".into()),
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
pub struct DisplayBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AccessibilityElement {
    pub role: String,
    pub label: Option<String>,
    pub value: Option<String>,
    pub bounds: Option<DisplayBounds>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ComputerResult {
    /// Input was posted to the OS. Application acceptance is observed by a subsequent snapshot.
    Posted,
    Snapshot {
        png_base64: String,
        pixel_width: u32,
        pixel_height: u32,
        display_id: u32,
        bounds: DisplayBounds,
        application: Option<String>,
        bundle_id: Option<String>,
        elements: Vec<AccessibilityElement>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ComputerError {
    #[error("computer use is disabled; enable it in Bootty settings")]
    Disabled,
    #[error("computer use is unsupported on this platform")]
    Unsupported,
    #[error("permission not granted: {0:?}")]
    PermissionDenied(Permission),
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
