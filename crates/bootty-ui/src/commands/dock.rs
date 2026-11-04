use crate::action_catalog::Command;

/// Workspace presentation commands. Group IDs name live tab groups in the target window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockAction {
    ToggleLeft,
    ToggleSessions,
    Spaces,
    ToggleHiddenTabs,
    ToggleTabBar,
    Sidebar,
}

impl DockAction {
    const ALL: [Self; 6] = [
        Self::ToggleLeft,
        Self::ToggleSessions,
        Self::Spaces,
        Self::ToggleHiddenTabs,
        Self::ToggleTabBar,
        Self::Sidebar,
    ];

    #[must_use]
    pub const fn command(self) -> Command {
        match self {
            Self::ToggleLeft => Command::ToggleLeftDock,
            Self::ToggleSessions => Command::ToggleSessionsPanel,
            Self::Spaces => Command::ShowSpaces,
            Self::ToggleHiddenTabs => Command::ToggleHiddenTabs,
            Self::ToggleTabBar => Command::ToggleTabBar,
            Self::Sidebar => Command::ShowSidebar,
        }
    }

    #[must_use]
    pub fn from_command(command: Command) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.command() == command)
    }
}

/// A command completed by the window that owns the Dock layout.
#[derive(Clone, Debug)]
pub struct DockRequest {
    pub action: DockAction,
    pub group: Option<u64>,
    completion: std::sync::Arc<DockCompletion>,
}

#[derive(Debug)]
struct DockCompletion {
    execution: Option<(std::time::Instant, bootty_control::CommandCancellation)>,
    response: Option<std::sync::mpsc::Sender<bootty_control::CommandOutcome>>,
}

impl PartialEq for DockRequest {
    fn eq(&self, other: &Self) -> bool {
        self.action == other.action
            && self.group == other.group
            && std::sync::Arc::ptr_eq(&self.completion, &other.completion)
    }
}

impl DockRequest {
    pub(crate) fn new(
        action: DockAction,
        group: Option<u64>,
        execution: Option<(std::time::Instant, bootty_control::CommandCancellation)>,
        response: Option<std::sync::mpsc::Sender<bootty_control::CommandOutcome>>,
    ) -> Self {
        Self {
            action,
            group,
            completion: std::sync::Arc::new(DockCompletion {
                execution,
                response,
            }),
        }
    }

    pub(crate) fn local(action: DockAction) -> Self {
        Self::new(action, None, None, None)
    }

    pub(crate) fn begin(&self) -> Result<(), bootty_mux::controller::MuxCommandError> {
        bootty_mux::executor::begin_synchronous_command(self.completion.execution.clone())
    }

    pub fn complete(self, outcome: bootty_control::CommandOutcome) {
        if let Some(response) = &self.completion.response {
            let _ = response.send(outcome);
        }
    }
}
