use serde::{Deserialize, Serialize};

use crate::snapshot::MuxSessionTag;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum MuxSplitDirection {
    Right,
    Down,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum MuxCommand {
    ResizePanes {
        session_id: String,
        window_id: String,
        layout: crate::snapshot::MuxPaneLayout,
    },
    ActivatePane {
        session_id: String,
        window_id: String,
        pane_id: String,
    },
    ActivateWindow {
        session_id: String,
        window_id: String,
    },
    RestoreSession {
        session_id: String,
        tag: MuxSessionTag,
        snapshot: crate::session_snapshot::SavedTerminalSession,
    },
    CreatePane {
        session_id: String,
        pane_id: Option<String>,
        direction: MuxSplitDirection,
        cwd: Option<String>,
        argv: Vec<String>,
    },
    NewWindow {
        session_id: String,
        cwd: Option<String>,
        #[serde(default)]
        argv: Option<Vec<String>>,
    },
    RenameWindow {
        session_id: String,
        window_id: String,
        name: String,
    },
    ActivateNextWindow {
        session_id: String,
    },
    ActivatePreviousWindow {
        session_id: String,
    },
    ActivateLastWindow {
        session_id: String,
    },
    ActivateWindowIndex {
        session_id: String,
        index: u32,
    },
    MoveWindow {
        session_id: String,
        window_id: Option<String>,
        delta: i32,
    },
    /// Reorder a specific window, then restore the window that was active before the move.
    /// Context-menu moves use this so moving an inactive tab does not steal focus.
    MoveWindowPreservingSelection {
        session_id: String,
        window_id: String,
        delta: i32,
        selected_window_id: String,
    },
    SplitPane {
        session_id: String,
        /// The pane to split (its cwd seeds the new pane). `None` splits the window's active pane.
        pane_id: Option<String>,
        direction: MuxSplitDirection,
    },
    MergeWindows {
        session_id: String,
        source_window_id: String,
        target_window_id: String,
    },
    SwapPanes {
        session_id: String,
        source_pane_id: String,
        target_pane_id: String,
    },
    MovePane {
        session_id: String,
        pane_id: String,
        target_pane_id: String,
        direction: MuxDirection,
    },
    ExtractPane {
        session_id: String,
        pane_id: String,
    },
    SelectPane {
        session_id: String,
        /// The window whose pane selection should move. `None` uses the session's active window.
        window_id: Option<String>,
        direction: MuxDirection,
    },
    SelectNextPane {
        session_id: String,
        window_id: Option<String>,
    },
    SelectPreviousPane {
        session_id: String,
        window_id: Option<String>,
    },
    KillPane {
        session_id: String,
        /// The pane to remove. `None` targets the window's active pane.
        pane_id: Option<String>,
    },
    // Close the active pane and cascade: an emptied window (tab) is removed; a session whose last
    // window is removed is left empty rather than deleted.
    ClosePane {
        session_id: String,
        /// The pane to close. `None` targets the window's active pane.
        pane_id: Option<String>,
    },
    CreateProjectSession {
        session_id: String,
        cwd: String,
        /// What to stamp onto the new session. Bootty mints the identity rather than the backend
        /// so a create whose result never came back can be settled by looking for this id.
        tag: MuxSessionTag,
        /// `None` creates a project session under a name Bootty minted; a backend may adopt a
        /// session that already has that name, so a retry after a lost reply is safe.
        ///
        /// `Some` is an explicit create: it fails rather than adopt an existing session, and is
        /// never retried after an ambiguous result. Its argv starts the first pane: empty starts
        /// the default shell, one element runs through the backend's default shell, and more
        /// elements run directly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argv: Option<Vec<String>>,
    },
    CreateWorktreeSession {
        session_id: String,
        cwd: String,
        tag: MuxSessionTag,
    },
    RenameSession {
        session_id: String,
        name: String,
    },
    DitchSession {
        session_id: String,
    },
    /// Write `tag` onto a session that already exists: adopting one bootty did not create, or
    /// restoring one whose server restarted and dropped its tag.
    StampSession {
        session_id: String,
        tag: MuxSessionTag,
    },
}

impl MuxCommand {
    #[must_use]
    pub const fn arranges_panes(&self) -> bool {
        matches!(
            self,
            Self::ResizePanes { .. }
                | Self::SplitPane { .. }
                | Self::CreatePane { .. }
                | Self::MergeWindows { .. }
                | Self::SwapPanes { .. }
                | Self::MovePane { .. }
                | Self::ExtractPane { .. }
        )
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::ResizePanes { session_id, .. }
            | Self::ActivatePane { session_id, .. }
            | Self::ActivateWindow { session_id, .. }
            | Self::RestoreSession { session_id, .. }
            | Self::CreatePane { session_id, .. }
            | Self::NewWindow { session_id, .. }
            | Self::RenameWindow { session_id, .. }
            | Self::ActivateNextWindow { session_id }
            | Self::ActivatePreviousWindow { session_id }
            | Self::ActivateLastWindow { session_id }
            | Self::ActivateWindowIndex { session_id, .. }
            | Self::MoveWindow { session_id, .. }
            | Self::MoveWindowPreservingSelection { session_id, .. }
            | Self::MergeWindows { session_id, .. }
            | Self::SwapPanes { session_id, .. }
            | Self::MovePane { session_id, .. }
            | Self::ExtractPane { session_id, .. }
            | Self::SplitPane { session_id, .. }
            | Self::SelectPane { session_id, .. }
            | Self::SelectNextPane { session_id, .. }
            | Self::SelectPreviousPane { session_id, .. }
            | Self::KillPane { session_id, .. }
            | Self::ClosePane { session_id, .. }
            | Self::CreateProjectSession { session_id, .. }
            | Self::CreateWorktreeSession { session_id, .. }
            | Self::RenameSession { session_id, .. }
            | Self::DitchSession { session_id }
            | Self::StampSession { session_id, .. } => session_id,
        }
    }
}

/// Input for one backend pane, delivered without changing selection or UI focus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneInput {
    /// Terminal bytes exactly as typed.
    Write(#[serde(with = "bootty_terminal::wire_bytes")] Vec<u8>),
    /// Text pasted the way the pane's application asked for: bracketed when it enabled
    /// bracketed paste, so multi-line text is not submitted line by line.
    Paste(String),
    /// The Enter key, encoded for the pane's keyboard mode.
    Submit,
}
