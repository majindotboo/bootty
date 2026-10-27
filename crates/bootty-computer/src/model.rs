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
    /// Make the selected window the one its app sends keys to. On macOS the app stays where it
    /// is; elsewhere the window comes to the front.
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
    /// List the window's accessibility elements.
    Elements,
    /// List the menu bar of the window's app without opening a menu.
    MenuBar,
    /// Replace an editable element's value through accessibility, without focus or input events.
    SetValue {
        element: Box<ComputerElement>,
        value: String,
    },
    /// Press an element from an earlier listing: its accessibility action when it has one,
    /// otherwise a click at its center.
    Press {
        element: Box<ComputerElement>,
        /// Refuse controls without an accessibility action when false.
        #[serde(
            default = "default_allow_click",
            skip_serializing_if = "allow_click_enabled"
        )]
        allow_click: bool,
    },
}

const fn default_allow_click() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde skip predicates borrow their field."
)]
const fn allow_click_enabled(value: &bool) -> bool {
    *value
}

impl ComputerAction {
    #[must_use]
    pub const fn is_capture(&self) -> bool {
        matches!(self, Self::Snapshot | Self::SnapshotRegion { .. })
    }

    /// Reads the window's pixels or elements without input.
    #[must_use]
    pub const fn observes(&self) -> bool {
        matches!(
            self,
            Self::Snapshot | Self::SnapshotRegion { .. } | Self::Elements | Self::MenuBar
        )
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
            Self::SetValue { value, .. } if value.len() > 4096 || value.contains('\0') => {
                Err(ComputerError::InvalidAction(
                    "value must contain at most 4096 bytes and no NUL".into(),
                ))
            }
            Self::Press { element, .. } | Self::SetValue { element, .. } => element.validate(),
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

/// Mirrors the JSON shape so error messages can be compared with listed bounds.
impl std::fmt::Display for DisplayBounds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{{x: {}, y: {}, width: {}, height: {}}}",
            self.x, self.y, self.width, self.height
        )
    }
}

/// Most accessibility elements one listing returns.
pub const MAX_ELEMENTS: usize = 300;
/// The deepest element path, in child steps from its root.
pub const MAX_ELEMENT_DEPTH: usize = 64;

/// Where an element's path starts.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ElementRoot {
    /// The target window.
    #[default]
    Window,
    /// The menu bar of the window's app. Its elements have no frame: menu items have none while
    /// their menu is closed, and the app's menu bar is not on screen while it is in the background.
    MenuBar,
}

/// One accessibility element of a window or its app's menu bar, as the app reported it.
///
/// Disabled and unlabeled window elements are listed too, so every visible control can be matched
/// to a screenshot by its frame.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComputerElement {
    /// Child positions from `root`. A press finds the element again by this path.
    pub path: Vec<u16>,
    /// Elements listed before roots existed have none, and are window elements.
    #[serde(default)]
    pub root: ElementRoot,
    /// The platform role, with its subrole after a colon, such as `RadioButton:TabButton`.
    pub role: String,
    /// The title, description or help text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The app's own identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// Global desktop points. Window elements always have one; menu bar elements never do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<DisplayBounds>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<ElementState>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ElementState {
    Disabled,
    Selected,
    Checked,
    Focused,
}

impl ComputerElement {
    /// Where a press clicks when the element has no accessibility action. Menu bar elements have
    /// no frame, so they are never clicked.
    #[must_use]
    pub fn center(&self) -> Option<(f64, f64)> {
        self.frame
            .as_ref()
            .map(|frame| (frame.x + frame.width / 2.0, frame.y + frame.height / 2.0))
    }

    /// The same element after a fresh read: its identity, role, name and frame are unchanged.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        self.path == other.path
            && self.root == other.root
            && self.role == other.role
            && self.name == other.name
            && self.identifier == other.identifier
            && self.frame == other.frame
    }

    pub(crate) fn validate(&self) -> Result<(), ComputerError> {
        let long = |text: &Option<String>| text.as_ref().is_some_and(|text| text.len() > 1024);
        if self.path.is_empty()
            || self.path.len() > MAX_ELEMENT_DEPTH
            || self.role.is_empty()
            || self.role.len() > 256
            || long(&self.name)
            || long(&self.value)
            || long(&self.identifier)
            || self.states.len() > 4
        {
            return Err(ComputerError::InvalidAction(
                "invalid accessibility element".into(),
            ));
        }
        match (self.root, &self.frame) {
            (ElementRoot::Window, Some(frame)) => frame.validate(),
            (ElementRoot::MenuBar, None) => Ok(()),
            (ElementRoot::Window, None) => Err(ComputerError::InvalidAction(
                "window elements need a frame".into(),
            )),
            (ElementRoot::MenuBar, Some(_)) => Err(ComputerError::InvalidAction(
                "menu bar elements have no frame".into(),
            )),
        }
    }
}

/// How a press reached the element.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PressMethod {
    /// The element's own accessibility press action.
    Action,
    /// A click at the element's center, for elements without one.
    Click,
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
            return Err(ComputerError::InvalidAction(format!(
                "capture rectangle {} is outside the target frame {frame}",
                self.rect
            )));
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
    /// Exact identity of a semantic-only accessibility window. Such windows cannot capture pixels
    /// or receive synthesized input. Their frames are toolkit-reported, possibly window-relative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
    pub process_id: i32,
    pub bundle_id: String,
    /// Presentation metadata; it does not authorize or identify a window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_name: Option<String>,
    pub launch_time: f64,
    pub bounds: DisplayBounds,
    pub title: Option<String>,
}

impl ComputerTarget {
    /// Whether this token supports only accessibility reads and semantic actions.
    #[must_use]
    pub const fn is_semantic_only(&self) -> bool {
        self.native_id.is_some()
    }

    /// Validate the shape of an observed window token without touching the OS.
    ///
    /// # Errors
    /// Returns an invalid-target error for missing identity or invalid geometry.
    pub fn validate(&self) -> Result<(), ComputerError> {
        let bounds = &self.bounds;
        if self.window_id == 0
            || self
                .native_id
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 1024)
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
            || self
                .application_name
                .as_ref()
                .is_some_and(|name| name.len() > 1024)
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
            ComputerAction::SnapshotRegion { rect } if !self.bounds.contains_rect(rect) => {
                Err(ComputerError::InvalidAction(format!(
                    "capture rectangle {rect} is outside the target bounds {}",
                    self.bounds
                )))
            }
            ComputerAction::Click { x, y, .. }
            | ComputerAction::Move { x, y }
            | ComputerAction::Scroll { x, y, .. }
                if !self.contains(*x, *y) =>
            {
                Err(ComputerError::InvalidAction(format!(
                    "coordinates ({x}, {y}) are outside the target bounds {}; \
                     x and y are screen points, not window-relative points",
                    self.bounds
                )))
            }
            ComputerAction::SetValue { element, .. } if element.root == ElementRoot::MenuBar => {
                Err(ComputerError::InvalidAction(
                    "menu bar items can only be pressed".into(),
                ))
            }
            ComputerAction::Press { element, .. } | ComputerAction::SetValue { element, .. } => {
                match element.center() {
                    // The menu bar belongs to the target's app, not to a point in its window.
                    None => Ok(()),
                    Some((x, y)) if self.contains(x, y) => Ok(()),
                    Some((x, y)) => Err(ComputerError::InvalidAction(format!(
                        "the element center ({x}, {y}) is outside the target bounds {}",
                        self.bounds
                    ))),
                }
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
    /// The OS accepted the operation. Read elements or capture again to observe application state.
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
    Elements {
        /// Screen order, at most [`MAX_ELEMENTS`].
        elements: Vec<ComputerElement>,
        /// More elements were left out past the limit or the time budget.
        omitted: bool,
        target: Box<ComputerTarget>,
    },
    /// The press was delivered: of the element, or of the menu item a key's shortcut names.
    /// Application acceptance is observed by a subsequent snapshot.
    Pressed { method: PressMethod },
}

impl ComputerResult {
    pub(crate) fn validate(
        &self,
        target: &ComputerTarget,
        action: &ComputerAction,
    ) -> Result<(), ComputerError> {
        match (self, action) {
            (
                Self::Posted,
                ComputerAction::Focus
                | ComputerAction::Click { .. }
                | ComputerAction::Move { .. }
                | ComputerAction::TypeText { .. }
                | ComputerAction::SetValue { .. }
                | ComputerAction::Key { .. }
                | ComputerAction::Scroll { .. },
            )
            | (Self::Pressed { .. }, ComputerAction::Press { .. } | ComputerAction::Key { .. }) => {
                Ok(())
            }
            (
                Self::Elements {
                    elements,
                    target: listed,
                    ..
                },
                ComputerAction::Elements | ComputerAction::MenuBar,
            ) => {
                let root = match action {
                    ComputerAction::MenuBar => ElementRoot::MenuBar,
                    _ => ElementRoot::Window,
                };
                validate_listing(listed, target, elements, root)
            }
            (
                Self::Snapshot {
                    target: captured,
                    pixel_width,
                    pixel_height,
                    png_base64,
                    region,
                    requested_region,
                },
                ComputerAction::Snapshot | ComputerAction::SnapshotRegion { .. },
            ) => {
                captured.validate()?;
                let expected_region = match action {
                    ComputerAction::SnapshotRegion { rect } => Some(rect),
                    _ => None,
                };
                if requested_region.as_ref() != expected_region {
                    return Err(ComputerError::Platform(
                        "capture source geometry changed".into(),
                    ));
                }
                match (expected_region, region) {
                    (Some(requested), Some(actual)) => {
                        actual.validate().map_err(|_| {
                            ComputerError::Platform("invalid capture geometry".into())
                        })?;
                        if !requested.contains_rect(actual)
                            || !captured.bounds.contains_rect(actual)
                        {
                            return Err(ComputerError::Platform(
                                "capture lies outside the requested region".into(),
                            ));
                        }
                    }
                    (None, None) => {}
                    _ => {
                        return Err(ComputerError::Platform(
                            "capture source geometry missing".into(),
                        ));
                    }
                }

                if !same_window(captured, target) {
                    return Err(ComputerError::StaleTarget);
                }
                if *pixel_width == 0
                    || *pixel_height == 0
                    || *pixel_width > 1600
                    || *pixel_height > 1600
                    || png_base64.is_empty()
                    || png_base64.len() > 11_184_812
                {
                    return Err(ComputerError::Platform(
                        "screenshot exceeds the image limit".into(),
                    ));
                }
                Ok(())
            }
            _ => Err(ComputerError::Platform(
                "computer use returned an unexpected result".into(),
            )),
        }
    }
}

/// A listing of exactly this window with at most [`MAX_ELEMENTS`] valid elements under `root`.
fn validate_listing(
    listed: &ComputerTarget,
    target: &ComputerTarget,
    elements: &[ComputerElement],
    root: ElementRoot,
) -> Result<(), ComputerError> {
    listed.validate()?;
    if !same_window(listed, target) {
        return Err(ComputerError::StaleTarget);
    }
    if elements.len() > MAX_ELEMENTS {
        return Err(ComputerError::Platform(
            "the element list exceeds its limit".into(),
        ));
    }
    if elements
        .iter()
        .any(|element| element.root != root || element.validate().is_err())
    {
        return Err(ComputerError::Platform(
            "invalid accessibility element".into(),
        ));
    }
    Ok(())
}

/// The same observed window token, compared exactly.
fn same_window(observed: &ComputerTarget, target: &ComputerTarget) -> bool {
    observed.window_id == target.window_id
        && observed.native_id == target.native_id
        && observed.process_id == target.process_id
        && observed.bundle_id == target.bundle_id
        && observed.launch_time.to_bits() == target.launch_time.to_bits()
        && observed.bounds == target.bounds
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
    #[error("the element moved, changed or closed; list elements again")]
    StaleElement,
    #[error("computer target is not the focused window")]
    TargetNotFocused,
    #[error("secure input is active; computer use is paused")]
    SecureInput,
    #[error("invalid computer action: {0}")]
    InvalidAction(String),
    #[error("computer use timed out; the action was not retried")]
    Timeout,
    #[error("computer use failed: {0}")]
    Platform(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
