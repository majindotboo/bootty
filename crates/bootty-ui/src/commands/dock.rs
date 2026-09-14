use crate::action_catalog::Command;

/// Workspace presentation commands. Group IDs name live tab groups in the target window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockAction {
    ToggleLeft,
    ToggleRight,
    TogglePanel(bootty_config::config::PanelKind),
    CodexBar,
    Spaces,
    ToggleHiddenTabs,
    ToggleTabBar,
    Sidebar,
    Files,
    Changes,
    Diff,
    Agents,
}

impl DockAction {
    pub const PANELS: [Self; 5] = [
        Self::Sidebar,
        Self::Files,
        Self::Changes,
        Self::Diff,
        Self::Agents,
    ];

    #[must_use]
    pub const fn panel(self) -> Option<bootty_config::config::PanelKind> {
        use bootty_config::config::PanelKind;
        Some(match self {
            Self::TogglePanel(kind) => kind,
            Self::Sidebar | Self::Spaces => PanelKind::Sessions,
            Self::Agents | Self::CodexBar => PanelKind::Agents,
            Self::Files => PanelKind::Files,
            Self::Changes => PanelKind::Changes,
            Self::Diff => PanelKind::Diff,
            _ => return None,
        })
    }
    #[must_use]
    pub const fn show_panel(kind: bootty_config::config::PanelKind) -> Self {
        use bootty_config::config::PanelKind;
        match kind {
            PanelKind::Sessions => Self::Sidebar,
            PanelKind::Files => Self::Files,
            PanelKind::Changes => Self::Changes,
            PanelKind::Diff => Self::Diff,
            PanelKind::Agents => Self::Agents,
        }
    }
    #[must_use]
    pub const fn command(self) -> Command {
        match self {
            Self::TogglePanel(kind) => match kind {
                bootty_config::config::PanelKind::Sessions => Command::ToggleSessionsPanel,
                bootty_config::config::PanelKind::Files => Command::ToggleFilesPanel,
                bootty_config::config::PanelKind::Changes => Command::ToggleChangesPanel,
                bootty_config::config::PanelKind::Diff => Command::ToggleDiffPanel,
                bootty_config::config::PanelKind::Agents => Command::ToggleAgentsPanel,
            },
            Self::ToggleLeft => Command::ToggleLeftDock,
            Self::ToggleRight => Command::ToggleRightDock,
            Self::CodexBar => Command::ShowCodexBar,
            Self::Spaces => Command::ShowSpaces,
            Self::ToggleHiddenTabs => Command::ToggleHiddenTabs,
            Self::ToggleTabBar => Command::ToggleTabBar,
            Self::Sidebar => Command::ShowSidebar,
            Self::Files => Command::ShowFiles,
            Self::Changes => Command::ShowChanges,
            Self::Diff => Command::ShowDiff,
            Self::Agents => Command::ShowAgents,
        }
    }

    pub fn from_command(command: Command) -> Option<Self> {
        [
            Self::ToggleLeft,
            Self::ToggleRight,
            Self::ToggleTabBar,
            Self::ToggleHiddenTabs,
            Self::CodexBar,
            Self::Spaces,
        ]
        .into_iter()
        .chain(Self::PANELS)
        .chain(bootty_config::config::PanelKind::ALL.map(Self::TogglePanel))
        .find(|action| action.command() == command)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelCreation {
    Command(DockAction),
    Context,
}

#[derive(Clone)]
pub struct PanelDescriptor {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub icon: gpui_kit::component::IconName,
    pub creation: PanelCreation,
}

/// The single product catalog for native Dock panels.
pub const PANELS: &[PanelDescriptor] = &[
    PanelDescriptor {
        name: "bootty.sessions",
        label: "Sessions",
        description: "Switch Spaces and browse terminal sessions.",
        icon: gpui_kit::component::IconName::PanelLeft,
        creation: PanelCreation::Command(DockAction::Sidebar),
    },
    PanelDescriptor {
        name: "bootty.files",
        label: "Files",
        description: "Browse files from the active terminal directory.",
        icon: gpui_kit::component::IconName::Folder,
        creation: PanelCreation::Command(DockAction::Files),
    },
    PanelDescriptor {
        name: "bootty.changes",
        label: "Changes",
        description: "Stage, unstage, commit, and inspect repository state.",
        icon: gpui_kit::component::IconName::Replace,
        creation: PanelCreation::Command(DockAction::Changes),
    },
    PanelDescriptor {
        name: "bootty.diff",
        label: "Diff",
        description: "Inspect the selected Git change.",
        icon: gpui_kit::component::IconName::Replace,
        creation: PanelCreation::Command(DockAction::Diff),
    },
    PanelDescriptor {
        name: "bootty.agents",
        label: "Agents",
        description: "Inspect agent sessions, attention state, and usage quotas.",
        icon: gpui_kit::component::IconName::Bot,
        creation: PanelCreation::Command(DockAction::Agents),
    },
    PanelDescriptor {
        name: "bootty.terminal",
        label: "Terminal",
        description: "The active backend window and its terminal panes.",
        icon: gpui_kit::component::IconName::SquareTerminal,
        creation: PanelCreation::Context,
    },
    PanelDescriptor {
        name: "bootty.document",
        label: "Document",
        description: "A file editor panel created when a document is opened.",
        icon: gpui_kit::component::IconName::FileText,
        creation: PanelCreation::Context,
    },
];

#[must_use]
pub fn panel_descriptor(name: &str) -> Option<&'static PanelDescriptor> {
    PANELS.iter().find(|panel| panel.name == name)
}

/// A command completed by the window that owns the Dock layout.
#[derive(Clone, Debug)]
pub struct DockRequest {
    pub action: DockAction,
    pub group: Option<u64>,
    pub(crate) directory: Option<String>,
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
            && self.directory == other.directory
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
            directory: None,
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
