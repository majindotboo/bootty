//! Captured destinations for transient tab and split creation.

use bootty_control::CommandTarget;
use bootty_mux::pane_layout::SplitDirection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfacePlacement {
    Tab,
    Split(SplitDirection),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceParent {
    Terminal(CommandTarget),
    Conversation(CommandTarget),
    Binding(CommandTarget),
}

/// App-owned request retained until creation succeeds or the chooser is cancelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingNewSurface {
    pub id: u64,
    pub binding: CommandTarget,
    pub task_identity: String,
    pub cwd: String,
    pub parent: SurfaceParent,
    pub placement: SurfacePlacement,
}
