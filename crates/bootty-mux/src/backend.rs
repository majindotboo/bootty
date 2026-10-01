use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::{command::MuxCommand, snapshot::MuxSnapshot};

/// Input for one backend pane, delivered without changing selection or UI focus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneInput {
    /// Terminal bytes exactly as typed.
    Write(Vec<u8>),
    /// Text pasted the way the pane's application asked for: bracketed when it enabled
    /// bracketed paste, so multi-line text is not submitted line by line.
    Paste(String),
    /// The Enter key, encoded for the pane's keyboard mode.
    Submit,
}

/// What to read from a backend pane: the last `max_lines` rows of the screen, or of the
/// scrollback including the screen, matching a terminal capture's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneCapture {
    pub history: bool,
    pub max_lines: u32,
    /// Keep SGR escape sequences instead of plain text.
    pub ansi: bool,
}

/// The rows a [`PaneCapture`] reads from one pane, in tmux's `capture-pane` numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneCaptureRows {
    /// The first row to read: screen rows count from 0, history rows are negative above them.
    pub first: i64,
    /// The last row to read, the screen's bottom row when measured. Bounding both ends keeps a
    /// pane that resizes mid-capture from returning more rows than were counted.
    pub last: i64,
    pub captured: u64,
    pub omitted: u64,
}

impl PaneCapture {
    /// The rows to read from a pane with `history` scrollback rows above a `height`-row screen.
    #[must_use]
    pub fn rows(self, history: u64, height: u64) -> PaneCaptureRows {
        let total = if self.history {
            history.saturating_add(height)
        } else {
            height
        };
        let captured = total.min(u64::from(self.max_lines));
        let omitted = total.saturating_sub(captured);
        let first = if self.history {
            i128::from(omitted).saturating_sub(i128::from(history))
        } else {
            i128::from(omitted)
        };
        PaneCaptureRows {
            first: i64::try_from(first).unwrap_or(if first < 0 { i64::MIN } else { i64::MAX }),
            last: i64::try_from(height)
                .unwrap_or(i64::MAX)
                .saturating_sub(1)
                .max(0),
            captured,
            omitted,
        }
    }
}

impl PaneCaptureRows {
    /// The capture a backend returned for these rows. Counts the lines actually read, since a
    /// resize or trimmed history between measuring and reading can return fewer rows.
    #[must_use]
    pub fn text(self, text: String) -> PaneText {
        let read = u64::try_from(text.lines().count()).unwrap_or(u64::MAX);
        PaneText {
            text,
            captured_lines: read.min(self.captured),
            omitted_lines: self.omitted,
        }
    }
}

/// A paste buffer name no other paste, Bootty process, or user buffer uses, for a backend that
/// pastes through a named server buffer.
pub(crate) fn private_paste_buffer() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let index = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("bootty-paste-{}-{index}", std::process::id())
}

/// A backend pane capture and how many earlier rows it left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneText {
    pub text: String,
    pub captured_lines: u64,
    pub omitted_lines: u64,
}

pub trait MuxBackend {
    /// # Errors
    /// Returns a transport or backend error when current topology cannot be read.
    fn snapshot(&self) -> Result<MuxSnapshot>;
    /// # Errors
    /// Returns invalid target, transport, or backend command errors.
    fn execute(&mut self, command: MuxCommand) -> Result<()>;
    /// Deliver input to one pane addressed by its backend pane id.
    ///
    /// # Errors
    /// Returns an error when this backend cannot address panes directly or rejects the input.
    fn send_pane_input(&self, _pane_id: &str, _input: &PaneInput) -> Result<()> {
        bail!("this backend cannot deliver input to a pane directly")
    }
    /// Read one pane's text addressed by its backend pane id.
    ///
    /// # Errors
    /// Returns an error when this backend cannot capture panes directly or the capture fails.
    fn capture_pane(&self, _pane_id: &str, _capture: PaneCapture) -> Result<PaneText> {
        bail!("this backend cannot capture a pane directly")
    }
}
