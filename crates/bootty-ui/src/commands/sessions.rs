use bootty_control::{CommandDescriptor, CompactSchema, ResourceKind, ValueType};
use bootty_mux::workspace::{
    SESSION_ARGV_MAX_BYTES, SESSION_ARGV_MAX_ELEMENTS, SESSION_NAME_MAX_BYTES,
};

command_actions! {
    SessionAction {
        ListProjects => ("project.list", "List registered projects", [], Read),
        RegisterProject => ("project.register", "Register project", ["cwd"], Write),
        ConfigureProject => ("project.configure", "Configure project", ["cwd", "settings"], Write),
        ToggleProjectCollapsed => ("project.toggle_collapsed", "Toggle project disclosure", ["cwd"], Write),
        ListSaved => ("session.saved", "List saved sessions", [], Read),
        SetTitle => ("session.set_title", "Edit session title", ["identity", "title"], Write),
        Pin => ("session.pin", "Pin session", ["identity"], Write),
        Unpin => ("session.unpin", "Unpin session", ["identity"], Write),
        Activity => ("session.activity", "Record accepted session activity", ["identity", "at"], Write),
        AcceptedInput => ("session.input_accepted", "Record new session input", ["identity", "at"], Write),
        SettleCurrent => ("settle_session", "Settle Session", [], Write),
        Settle => ("session.settle", "Settle session", ["identity"], Write),
        Activate => ("session.activate", "Mark session active", ["identity"], Write),
        Archive => ("session.archive", "Archive session", ["identity"], Write),
        Unarchive => ("session.unarchive", "Restore archived session", ["identity"], Write),
        Snooze => ("session.snooze", "Snooze session", ["identity", "until"], Write),
        Unsnooze => ("session.unsnooze", "Clear session snooze", ["identity"], Write),
        Hide => ("session.hide", "Hide session", ["identity"], Write),
        Show => ("session.show", "Show hidden session", ["identity"], Write),
        Delete => ("session.delete", "Delete saved session", ["identity"], Destructive),
        Restore => ("session.restore", "Restore deleted session", ["identity"], Write),
        Reopen => ("session.reopen", "Reopen saved session", ["identity"], Write),
        Create => ("session.create", "Create Session", ["name", "cwd", "argv", "identity", "title"], Write),
        CreateTab => ("terminal.create_tab", "Create Terminal Tab", ["argv", "cwd"], Write),
        CreatePane => ("terminal.create_pane", "Create Terminal Pane", ["direction", "argv", "cwd"], Write),
        Close => ("session.close", "Close Session", [], Destructive),
        ClosePane => ("pane.close", "Close Pane", [], Destructive),
        ListSpaces => ("spaces.list", "List Spaces", [], Read),
        InspectSpace => ("spaces.inspect", "Inspect Space", [], Read),
        ListTerminals => ("terminal.activities", "List Space terminals", [], Read),
    }
}

impl SessionAction {
    #[must_use]
    pub fn descriptor(self) -> CommandDescriptor {
        let (id, title, names, mutation) = self.metadata();
        let arguments = names
            .iter()
            .copied()
            .map(|name| {
                let mut argument = super::argument(name, ValueType::String);
                argument.required = if self == Self::CreatePane {
                    name != "cwd"
                } else if self == Self::CreateTab {
                    name == "argv"
                } else if self == Self::Create {
                    !matches!(name, "argv" | "identity" | "title")
                } else {
                    name != "argv"
                };
                if self == Self::CreatePane && name == "direction" {
                    argument.choices = vec!["right".to_owned(), "down".to_owned()];
                }
                argument
            })
            .collect();
        let (description, target) = match self {
            Self::ListProjects => ("List projects registered on the exact Space's host, including empty projects.".to_owned(), Some(ResourceKind::Binding)),
            Self::RegisterProject => ("Register an absolute project path on the exact Space's host without starting a session.".to_owned(), Some(ResourceKind::Binding)),
            Self::ConfigureProject => ("Commit project settings JSON on the exact Space; applies to new sessions and worktrees.".to_owned(), Some(ResourceKind::Binding)),
            Self::ToggleProjectCollapsed => ("Toggle the registered project's sidebar disclosure without changing its sessions.".to_owned(), Some(ResourceKind::Binding)),
            Self::ListSaved => ("List saved identities, purpose titles, project directories and observed attachments in the exact Space. Attachment is not agent or process status.".to_owned(), Some(ResourceKind::Binding)),
            Self::SetTitle => ("Edit the saved purpose title without changing the backend name or starting/stopping any terminal. The title must be 1–256 bytes without control characters.".to_owned(), Some(ResourceKind::Binding)),
            Self::Pin => ("Pin saved work, mark it active and clear its snooze. Preserve its identity, order and terminal attachment.".to_owned(), Some(ResourceKind::Binding)),
            Self::Unpin => ("Remove the saved pin while preserving lifecycle, visibility, order and terminal attachment.".to_owned(), Some(ResourceKind::Binding)),
            Self::Activity => ("Record accepted input at non-negative absolute UTC seconds no later than the host clock. Preserve newer activity, identity, manual order and attachment.".to_owned(), Some(ResourceKind::Binding)),
            Self::AcceptedInput => ("Record newly accepted input and return settled work to the active list. Preserve newer activity, identity, order and attachment.".to_owned(), Some(ResourceKind::Binding)),
            Self::SettleCurrent => ("Resolve the exact issued session to its saved identity and settle that work without closing a terminal. Reject stale or unsaved sessions.".to_owned(), Some(ResourceKind::Session)),
            Self::Settle => ("Mark saved work settled and remove its pin without closing any backend terminal.".to_owned(), Some(ResourceKind::Binding)),
            Self::Activate => ("Mark saved work active without starting or selecting any backend terminal.".to_owned(), Some(ResourceKind::Binding)),
            Self::Archive => ("Archive saved work while retaining its lifecycle, visibility, snooze and terminal attachment.".to_owned(), Some(ResourceKind::Binding)),
            Self::Unarchive => ("Restore archived work with its previous lifecycle, visibility and snooze.".to_owned(), Some(ResourceKind::Binding)),
            Self::Snooze => ("Snooze saved work until non-negative absolute UTC seconds. Its previous lifecycle and attachment remain unchanged.".to_owned(), Some(ResourceKind::Binding)),
            Self::Unsnooze => ("Clear the saved snooze deadline while preserving lifecycle and visibility.".to_owned(), Some(ResourceKind::Binding)),
            Self::Hide => ("Hide saved work while preserving lifecycle, snooze and terminal attachment.".to_owned(), Some(ResourceKind::Binding)),
            Self::Show => ("Show hidden saved work while preserving lifecycle and snooze.".to_owned(), Some(ResourceKind::Binding)),
            Self::Delete => ("Permanently remove a closed session's saved record and terminal checkpoint from this Space. Close live sessions first; use Archive to retain recoverable work. Native agent history, worktrees and branches remain.".to_owned(), Some(ResourceKind::Binding)),
            Self::Restore => ("Restore a deleted saved identity with its previous lifecycle, archive, visibility and snooze. Never start a terminal implicitly.".to_owned(), Some(ResourceKind::Binding)),
            Self::Reopen => ("Reattach the exact original terminal when live; otherwise restore its saved layout, panes, directories and plain history, or start a fresh shell in the saved directory when no checkpoint exists. Preserve logical identity, title and order; never adopt by name or replay shell commands or prompts.".to_owned(), Some(ResourceKind::Binding)),
            Self::Create => (
                format!(
                    "Create a detached backend session in the target Space without changing selection, focus or the active Space. The name (at most {SESSION_NAME_MAX_BYTES} bytes, no control characters, ':', '.', '\\' or '#', not starting with '-', '$', '@', '%' or '=') must be free on the Space's server; an existing session is never reused. cwd is an absolute path on the Space's host. argv is an optional JSON array of strings (at most {SESSION_ARGV_MAX_ELEMENTS} elements and {SESSION_ARGV_MAX_BYTES} bytes) for the first pane: absent or empty starts the default shell; every element otherwise remains a literal executable or argument. Optional identity and title retain one saved task across failed launches; retry requires the same detached, nondeleted identity and directory, preserves saved state and purpose, and never adopts a live terminal. Returns the created session and its first pane's terminal target."
                ),
                Some(ResourceKind::Binding),
            ),
            Self::Close => (
                "Close the target terminal session in any Space without changing selection or focus. Its saved identity/title/project remain detached; worktrees and branches are left alone.".to_owned(),
                Some(ResourceKind::Session),
            ),
            Self::CreateTab => (
                "Create a real backend terminal tab in the exact target session with literal argv JSON and optional host cwd. Returns the new terminal target.".to_owned(),
                Some(ResourceKind::Session),
            ),
            Self::CreatePane => (
                "Create a real backend terminal pane beside the exact captured pane with literal argv JSON and optional host cwd. Returns the new terminal target.".to_owned(),
                Some(ResourceKind::Terminal),
            ),
            Self::ClosePane => (
                "Close the target pane in any Space without changing selection or focus. Closing a window's last pane closes the window; closing a session's last window ends the session.".to_owned(),
                Some(ResourceKind::Terminal),
            ),
            Self::ListSpaces => (
                "List every Space with its backend, host, binding target and the sessions it holds.".to_owned(),
                None,
            ),
            Self::InspectSpace => (
                "Read the exact Space's display name, backend and host label without exposing other Spaces or account paths.".to_owned(),
                Some(ResourceKind::Binding),
            ),
            Self::ListTerminals => (
                "List up to 128 observed terminal panes in the exact Space, excluding native agent carriers. Includes names and opaque targets, without process or directory fields or other Spaces.".to_owned(),
                Some(ResourceKind::Binding),
            ),
        };
        CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description,
            arguments: CompactSchema { arguments },
            mutation,
            target,
            palette: self == Self::SettleCurrent,
        }
    }
}
