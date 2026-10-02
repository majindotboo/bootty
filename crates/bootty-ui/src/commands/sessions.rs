use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};
use bootty_mux::workspace::{
    SESSION_ARGV_MAX_BYTES, SESSION_ARGV_MAX_ELEMENTS, SESSION_NAME_MAX_BYTES,
};

command_actions! {
    SessionAction {
        Create => ("session.create", "Create Session", ["name", "cwd", "argv"], Write),
        Start => ("session.start", "Start Session", ["name", "cwd", "argv"], Write),
        StartProject => ("session.start_project", "Start Project Session", ["cwd", "argv"], Write),
        Close => ("session.close", "Close Session", [], Destructive),
        ClosePane => ("pane.close", "Close Pane", [], Destructive),
        ListSpaces => ("spaces.list", "List Spaces", [], Read),
    }
}

impl SessionAction {
    pub(super) const fn command_selection(self) -> bootty_mux::controller::CommandSelection {
        if matches!(self, Self::Start | Self::StartProject) {
            bootty_mux::controller::CommandSelection::Follow
        } else {
            bootty_mux::controller::CommandSelection::Preserve
        }
    }

    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        let arguments = names
            .iter()
            .copied()
            .map(|name| {
                let mut argument = super::argument(name, ValueType::String);
                argument.required = name != "argv";
                argument
            })
            .collect();
        let (description, target) = match self {
            Self::Create => (
                format!(
                    "Create a detached backend session in the target Space without changing selection, focus or the active Space. The name (at most {SESSION_NAME_MAX_BYTES} bytes, no control characters, ':', '.', '\\' or '#', not starting with '-', '$', '@', '%' or '=') must be free on the Space's server; an existing session is never reused. cwd is an absolute path on the Space's host. argv is an optional JSON array of strings (at most {SESSION_ARGV_MAX_ELEMENTS} elements and {SESSION_ARGV_MAX_BYTES} bytes) for the first pane: absent or empty starts the default shell; native runs literal argv, while tmux and rmux interpret one element through their default shell. Use explicit shell argv for portable command text. Returns the created session and its first pane's terminal target."
                ),
                Some(ResourceKind::Binding),
            ),
            Self::Start => (
                "Create and select a new session in the target Space. Uses the same name, checkout and literal argv validation as session.create; absent argv opens a shell. Explicit shell argv runs command text portably. Other Spaces keep their selection.".to_owned(),
                Some(ResourceKind::Binding),
            ),
            Self::StartProject => (
                "Create and select a project session in the target Space. Generate a free backend name from cwd, preserving its project display label. Optional argv uses the same literal validation as session.create; absent argv opens a shell.".to_owned(),
                Some(ResourceKind::Binding),
            ),
            Self::Close => (
                "Kill the target session in any Space without changing selection or focus. Worktrees and branches are left alone.".to_owned(),
                Some(ResourceKind::Session),
            ),
            Self::ClosePane => (
                "Close the target pane in any Space without changing selection or focus. Closing a window's last pane closes the window; closing a session's last window ends the session.".to_owned(),
                Some(ResourceKind::Terminal),
            ),
            Self::ListSpaces => (
                "List every Space with its backend, host, binding target and the sessions it holds.".to_owned(),
                None,
            ),
        };
        CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description,
            arguments: CompactSchema { arguments },
            mutation,
            target,
            palette: false,
        }
    }
}
