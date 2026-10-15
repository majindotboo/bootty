//! The platform seam. Each OS lists windows, reports focus, captures and posts input; the shared
//! control rules decide what is allowed, so every platform enforces the same window identity.
use crate::{
    ComputerElement, ComputerError, ComputerStatus, ComputerTarget, DisplayBounds, Key, Modifier,
    MouseButton,
};

/// One on-screen window, front to back. `app` is present only for an ordinary application window
/// an agent may target; other windows still count when deciding whether a point is covered.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeWindow {
    pub id: u32,
    pub bounds: DisplayBounds,
    /// A fully transparent window never covers a point.
    pub visible: bool,
    pub app: Option<ComputerTarget>,
}

/// The frontmost application and the bounds of its focused window, when the OS reports one.
#[derive(Clone, Debug, PartialEq)]
pub struct Focus {
    pub process_id: i32,
    pub window: Option<DisplayBounds>,
}

/// One input event in global desktop points.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Move {
        x: f64,
        y: f64,
    },
    Click {
        x: f64,
        y: f64,
        button: MouseButton,
    },
    Scroll {
        x: f64,
        y: f64,
        delta_x: i32,
        delta_y: i32,
    },
    /// At most [`crate::control::MAX_TEXT_UNITS`] UTF-16 units; longer text arrives in pieces.
    Text(String),
    Key {
        key: Key,
        modifiers: Vec<Modifier>,
    },
}

/// Tightly packed 8-bit RGBA pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Whole pixels in a desktop length, at least one.
#[must_use]
pub fn pixel_count(points: f64) -> u32 {
    pixel_edge(points).max(1)
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Float-to-integer casts saturate; callers pass finite, non-negative pixel edges."
)]
pub(crate) const fn pixel_edge(value: f64) -> u32 {
    value as u32
}

impl Image {
    /// `columns` pixels of one row, starting at `column`.
    fn row(&self, row: u32, column: u32, columns: u32) -> Option<&[u8]> {
        let index = |value: u32| usize::try_from(value).ok();
        let start = index(row)?
            .checked_mul(index(self.width)?)?
            .checked_add(index(column)?)?
            .checked_mul(4)?;
        let end = start.checked_add(index(columns)?.checked_mul(4)?)?;
        self.rgba.get(start..end)
    }

    /// The pixels inside one rectangle, or `None` when it extends past the image.
    #[must_use]
    pub fn cropped(&self, x: u32, y: u32, width: u32, height: u32) -> Option<Self> {
        let mut rgba = Vec::new();
        for row in y..y.checked_add(height)? {
            rgba.extend_from_slice(self.row(row, x, width)?);
        }
        Some(Self {
            width,
            height,
            rgba,
        })
    }

    /// Nearest-neighbour scaling, for platforms that capture at native size.
    #[must_use]
    pub fn scaled(&self, width: u32, height: u32) -> Self {
        if self.width == width && self.height == height {
            return self.clone();
        }
        let source = |index: u32, extent: u32, source_extent: u32| {
            u64::from(index)
                .checked_mul(u64::from(source_extent))
                .and_then(|scaled| scaled.checked_div(u64::from(extent)))
                .and_then(|source| u32::try_from(source).ok())
                .unwrap_or(0)
        };
        let mut rgba = Vec::new();
        for row in 0..height {
            let source_row = source(row, height, self.height);
            for column in 0..width {
                let pixel = self.row(source_row, source(column, width, self.width), 1);
                rgba.extend_from_slice(pixel.unwrap_or(&[0; 4]));
            }
        }
        Self {
            width,
            height,
            rgba,
        }
    }
}

/// What one operating system provides. Implementations report what they observe and never retry
/// or reinterpret an action.
pub trait Desktop: Send + Sync {
    /// Current OS permission state, without prompting.
    fn status(&self) -> ComputerStatus;
    /// On-screen windows, front to back.
    fn windows(&self) -> Vec<NativeWindow>;
    fn focus(&self) -> Option<Focus>;
    /// True while the OS hides keystrokes from other processes, such as in a password field.
    fn secure_input(&self) -> bool;
    /// Capture one window scaled to `width` by `height` pixels.
    ///
    /// # Errors
    /// Returns permission, availability or platform errors.
    fn capture(
        &self,
        window: &NativeWindow,
        width: u32,
        height: u32,
        cursor: bool,
    ) -> Result<Image, ComputerError>;
    /// Deliver one event to the window's application.
    ///
    /// # Errors
    /// Returns platform errors; delivery is never retried.
    fn post(&self, window: &NativeWindow, event: &InputEvent) -> Result<(), ComputerError>;
    /// Raise and focus the window. The caller confirms the resulting focus.
    ///
    /// # Errors
    /// Returns focus or platform errors.
    fn raise(&self, window: &NativeWindow) -> Result<(), ComputerError>;
    /// A square icon for an application, `side` pixels wide, when the OS has one.
    fn icon(&self, bundle_id: &str, side: u32) -> Option<Image>;

    // Deliberate limit: only macOS reads accessibility elements. Implement the three methods
    // below with UI Automation on Windows and AT-SPI on Linux when agents need elements there.

    /// The window's accessibility elements in screen order: at most `limit`, and whether any were
    /// left out.
    ///
    /// # Errors
    /// Returns unsupported, availability or platform errors.
    fn elements(
        &self,
        _window: &NativeWindow,
        _limit: usize,
    ) -> Result<(Vec<ComputerElement>, bool), ComputerError> {
        Err(ComputerError::Unsupported)
    }
    /// The element at `path` as it is now, or `None` when the window has none there.
    ///
    /// # Errors
    /// Returns unsupported, availability or platform errors.
    fn element(
        &self,
        _window: &NativeWindow,
        _path: &[u16],
    ) -> Result<Option<ComputerElement>, ComputerError> {
        Err(ComputerError::Unsupported)
    }
    /// Perform the element's press action. `false` means it has none, so the caller clicks it.
    ///
    /// # Errors
    /// Returns unsupported, availability or platform errors; a press is never retried.
    fn press(&self, _window: &NativeWindow, _path: &[u16]) -> Result<bool, ComputerError> {
        Err(ComputerError::Unsupported)
    }
}
