//! Sessions a caller creates or closes by name, in any Space, without moving selection.

use std::{path::Path, task::Poll};

use super::{BindingRuntime, WorkspaceRuntime};
use crate::{
    command::MuxCommand,
    controller::{MuxCommandError, SpaceId},
    executor::preflight_binding_command,
    provider::PaneTopology,
    repository::{BindingMembershipMutation, WorkspacePersistenceError},
    snapshot::{MuxPaneAnchor, MuxSession},
};

/// The longest session name a caller may choose, in bytes.
pub use crate::session_names::SESSION_NAME_MAX_BYTES;
/// The most argv elements a caller may pass for a new session's first pane.
pub const SESSION_ARGV_MAX_ELEMENTS: usize = 64;
/// The most argv bytes a caller may pass, counting one terminator per element.
///
/// Deliberate limit: tmux carries a whole client command, the create's stamps included, in one
/// message of about 16 KiB. Raise this only with a launch path that tmux does not bound that way,
/// such as handing the pane a file to run.
pub const SESSION_ARGV_MAX_BYTES: usize = 12 * 1024;

/// The first pane Bootty started for an explicit create, by the ids it had when the create
/// landed. A session closed and recreated under the same name, or a replaced binding, is never
/// mistaken for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartingSession {
    scope: SpaceId,
    generation: u64,
    session_id: String,
    pane_id: String,
    window_id: String,
    created_session: bool,
    pane_ids: Vec<String>,
}

impl StartingSession {
    #[must_use]
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }
    #[must_use]
    pub fn window_id(&self) -> &str {
        &self.window_id
    }
    #[must_use]
    pub const fn created_session(&self) -> bool {
        self.created_session
    }

    /// The Space whose binding holds the session.
    #[must_use]
    pub const fn scope(&self) -> SpaceId {
        self.scope
    }

    /// The backend id of the session this create made.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Why a session request was refused before anything reached the backend.
#[derive(Debug)]
pub enum SessionRequestError {
    /// The request itself is invalid.
    Invalid(String),
    /// A session on the binding's server already has the requested name.
    NameTaken(String),
    /// The binding's backend cannot do what was asked.
    Unsupported(String),
    /// The Space, the session, or the binding's backend is not available.
    Unavailable(String),
    /// The membership journal could not be written.
    Persistence(WorkspacePersistenceError),
}

impl From<WorkspacePersistenceError> for SessionRequestError {
    fn from(error: WorkspacePersistenceError) -> Self {
        Self::Persistence(error)
    }
}

/// A request ready to submit: the backend command and the membership it journaled.
pub type PreparedSessionRequest = (MuxCommand, Option<BindingMembershipMutation>);

impl WorkspaceRuntime {
    /// Prepare a literal command in a new tab of an existing held session.
    /// # Errors
    /// Rejects invalid cwd/argv, foreign sessions and unsupported backends before mutation.
    pub fn begin_tab_create(
        &self,
        scope: SpaceId,
        session_id: &str,
        cwd: Option<&str>,
        argv: Vec<String>,
    ) -> Result<PreparedSessionRequest, SessionRequestError> {
        validate_argv(&argv)?;
        let binding = self.live_binding(scope)?;
        let session = binding
            .mux
            .backend_session_by_id_or_name(session_id)
            .ok_or_else(|| SessionRequestError::Unavailable("the session was closed".to_owned()))?;
        if !binding.holds(session) {
            return Err(SessionRequestError::Invalid(
                "this Space does not hold the session".to_owned(),
            ));
        }
        if let Some(cwd) = cwd {
            validate_cwd(cwd)?;
            if binding.backend_policy.panes.topology == PaneTopology::ProcessLocal
                && !Path::new(cwd).is_dir()
            {
                return Err(SessionRequestError::Invalid(format!(
                    "cwd {cwd:?} is not a directory"
                )));
            }
        }
        let command = MuxCommand::NewWindow {
            session_id: session.id.clone(),
            cwd: cwd.map(str::to_owned),
            argv: Some(argv),
        };
        preflight(binding, &command)?;
        Ok((command, None))
    }

    /// Prepare a literal command in a new pane of an exact held terminal session.
    /// # Errors
    /// Rejects foreign panes, invalid cwd/argv and unsupported backends before mutation.
    pub fn begin_pane_create(
        &self,
        scope: SpaceId,
        session_id: &str,
        pane_id: &str,
        direction: crate::command::MuxSplitDirection,
        cwd: Option<&str>,
        argv: Vec<String>,
    ) -> Result<PreparedSessionRequest, SessionRequestError> {
        self.begin_tab_create(scope, session_id, cwd, argv.clone())?;
        let binding = self.live_binding(scope)?;
        let session = binding
            .mux
            .backend_session_by_id_or_name(session_id)
            .ok_or_else(|| SessionRequestError::Unavailable("the session was closed".into()))?;
        if !session.windows.iter().any(|window| {
            window
                .panes
                .iter()
                .any(|pane| pane.pane_id.as_deref() == Some(pane_id))
        }) {
            return Err(SessionRequestError::Unavailable(
                "the exact pane was closed".into(),
            ));
        }
        let command = MuxCommand::CreatePane {
            session_id: session.id.clone(),
            pane_id: Some(pane_id.to_owned()),
            direction,
            cwd: cwd.map(str::to_owned),
            argv,
        };
        preflight(binding, &command)?;
        Ok((command, None))
    }

    /// Validate an explicit session create in the Space at `scope` and journal its membership.
    ///
    /// The name must be free on the binding's server: the create never adopts or renames an
    /// existing session. The Space claims the session under the caller's name, so generated-name
    /// reconciliation leaves it alone.
    /// # Errors
    /// Returns why the request was refused. Nothing has been sent to the backend.
    pub fn begin_session_create(
        &mut self,
        scope: SpaceId,
        name: &str,
        cwd: &str,
        argv: Vec<String>,
    ) -> Result<PreparedSessionRequest, SessionRequestError> {
        self.begin_session_create_request(scope, name, cwd, argv, None)
    }

    /// Create or retry the exact saved identity issued for a retained caller draft.
    /// Existing detached work keeps its purpose, order and lifecycle; no live task is adopted.
    /// # Errors
    /// Rejects foreign, attached, deleted or pending identities and changed saved directories.
    pub fn begin_session_create_saved(
        &mut self,
        scope: SpaceId,
        name: &str,
        cwd: &str,
        argv: Vec<String>,
        identity: &str,
        title: &str,
    ) -> Result<PreparedSessionRequest, SessionRequestError> {
        self.begin_session_create_request(scope, name, cwd, argv, Some((identity, title)))
    }

    fn begin_session_create_request(
        &mut self,
        scope: SpaceId,
        name: &str,
        cwd: &str,
        argv: Vec<String>,
        saved: Option<(&str, &str)>,
    ) -> Result<PreparedSessionRequest, SessionRequestError> {
        validate_session_name(name)?;
        validate_cwd(cwd)?;
        validate_argv(&argv)?;
        let naming = saved
            .map(|(identity, title)| self.saved_create_metadata(scope, name, cwd, identity, title))
            .transpose()?;
        let binding = self.live_binding(scope)?;
        // Bootty spawns a native pane itself, and its PTY layer starts in the home directory when
        // the cwd is not one. tmux and rmux resolve the directory on their own host.
        if binding.backend_policy.panes.topology == PaneTopology::ProcessLocal
            && !Path::new(cwd).is_dir()
        {
            return Err(SessionRequestError::Invalid(format!(
                "cwd {cwd:?} is not a directory"
            )));
        }
        let taken = binding
            .mux
            .all_sessions()
            .iter()
            .any(|session| session.name == name)
            || binding
                .pending_generated_names
                .values()
                .any(|pending| pending.name == name);
        if taken {
            return Err(SessionRequestError::NameTaken(format!(
                "a session named {name} already exists"
            )));
        }
        let command = MuxCommand::CreateProjectSession {
            session_id: name.to_owned(),
            cwd: cwd.to_owned(),
            tag: saved.map_or_else(
                || binding.new_session_tag(),
                |(identity, _)| crate::snapshot::MuxSessionTag {
                    identity: Some(identity.to_owned()),
                    space: Some(binding.space_tag.clone()),
                },
            ),
            argv: Some(argv),
        };
        preflight(binding, &command)?;
        let membership =
            self.begin_binding_membership_mutation(scope, &command, naming.as_ref())?;
        Ok((command, membership))
    }

    fn saved_create_metadata(
        &mut self,
        scope: SpaceId,
        name: &str,
        cwd: &str,
        identity: &str,
        title: &str,
    ) -> Result<super::PendingGeneratedName, SessionRequestError> {
        if identity.is_empty() || identity.len() > 256 || identity.chars().any(char::is_control) {
            return Err(SessionRequestError::Invalid(
                "saved identity must be 1–256 bytes without control characters".to_owned(),
            ));
        }
        super::binding_session_names::validate_session_title(title)
            .map_err(|error| SessionRequestError::Invalid(error.to_string()))?;
        let binding = self.live_binding(scope)?;
        if !binding.tracks_session_membership() {
            return Err(SessionRequestError::Unsupported(
                "this backend cannot create saved session identities".to_owned(),
            ));
        }
        let saved = binding.sessions.get(identity).cloned();
        if self.all_bindings().any(|binding| {
            (binding.scope != scope && binding.sessions.contains(identity))
                || binding
                    .mux
                    .all_sessions()
                    .iter()
                    .any(|session| session.tag.identity.as_deref() == Some(identity))
        }) {
            return Err(SessionRequestError::Unavailable(
                "this saved identity belongs to another Space or already has a terminal attachment"
                    .to_owned(),
            ));
        }
        if let Some(saved) = &saved {
            if saved.state.deleted {
                return Err(SessionRequestError::Unavailable(
                    "restore the deleted session before retrying it".to_owned(),
                ));
            }
            if saved.cwd != cwd {
                return Err(SessionRequestError::Invalid(
                    "retry must use the saved project directory".to_owned(),
                ));
            }
        }
        let scopes = self
            .all_bindings()
            .map(|binding| binding.scope)
            .collect::<Vec<_>>();
        for pending_scope in scopes {
            if self
                .repository
                .pending_binding_membership_mutations(pending_scope)?
                .iter()
                .any(|operation| operation.mutation().identity() == identity)
            {
                return Err(SessionRequestError::Unavailable(
                    "this saved identity already has a pending operation".to_owned(),
                ));
            }
        }
        Ok(super::PendingGeneratedName {
            name: name.to_owned(),
            display_name: saved.as_ref().map_or_else(
                || title.trim().to_owned(),
                |saved| saved.display_name.clone(),
            ),
            explicit: saved.is_none_or(|saved| saved.explicit),
        })
    }

    /// Reattach the exact original, restore its saved state, or start a fresh shell.
    /// # Errors
    /// Deleted, pending, unsupported and invalid saved directories remain unchanged.
    pub fn begin_session_reopen(
        &mut self,
        scope: SpaceId,
        identity: &str,
    ) -> Result<PreparedSessionRequest, SessionRequestError> {
        let repaint = self.repaint.clone();
        let binding = self.binding_mut(scope).ok_or_else(|| {
            SessionRequestError::Unavailable("the target Space was closed".into())
        })?;
        if binding.membership_completion_is_immediate() {
            // Native topology outlives a window; inspect it even before that window's first frame.
            let config = binding.multiplexer.clone();
            if let Some(error) = binding
                .mux
                .refresh_sessions(&repaint, &config, std::time::Duration::ZERO)
                .error
            {
                return Err(SessionRequestError::Unavailable(error));
            }
        }
        let pending = self
            .repository
            .pending_binding_membership_mutations(scope)?;
        let binding = self.live_binding(scope)?;
        if !binding.tracks_session_membership() {
            return Err(SessionRequestError::Unsupported(
                "this backend cannot resolve saved session identities".to_owned(),
            ));
        }
        let saved = binding.sessions.get(identity).ok_or_else(|| {
            SessionRequestError::Unavailable(
                "this Space does not hold the saved session".to_owned(),
            )
        })?;
        if saved.state.deleted {
            return Err(SessionRequestError::Unavailable(
                "restore the deleted session before opening it".to_owned(),
            ));
        }
        if pending
            .iter()
            .any(|operation| operation.mutation().identity() == identity)
        {
            return Err(SessionRequestError::Unavailable(
                "this saved session already has a pending operation".to_owned(),
            ));
        }
        if let Some(session) = binding.session_attachment(identity) {
            let window = session
                .windows
                .iter()
                .find(|window| session.active_window_id.as_deref() == Some(window.id.as_str()))
                .or_else(|| session.windows.iter().find(|window| window.active))
                .or_else(|| session.windows.first())
                .ok_or_else(|| {
                    SessionRequestError::Unavailable(
                        "the original terminal session has no available window".into(),
                    )
                })?;
            let command = MuxCommand::ActivateWindow {
                session_id: session.id.clone(),
                window_id: window.id.clone(),
            };
            preflight(binding, &command)?;
            return Ok((command, None));
        }
        let name = binding.reopen_session_name(&saved.backend_name)?;
        let Some(saved_snapshot) = saved.terminal_snapshot.clone() else {
            let cwd = saved.cwd.clone();
            let title = saved.label().to_owned();
            return self.begin_session_create_saved(
                scope,
                &name,
                &cwd,
                Vec::new(),
                identity,
                &title,
            );
        };
        validate_restore_snapshot(&saved_snapshot, binding.backend_policy.panes.topology)?;
        let mut snapshot = (*saved_snapshot).clone();
        // Native/rmux readers seed local history. tmux's copy mode needs rows on its owner.
        if binding.backend_policy.panes.topology != crate::provider::PaneTopology::Attach {
            for window in &mut snapshot.windows {
                for pane in &mut window.panes {
                    pane.text.clear();
                    pane.omitted_lines = 0;
                }
            }
        }
        let command = MuxCommand::RestoreSession {
            session_id: name,
            tag: crate::snapshot::MuxSessionTag {
                identity: Some(identity.to_owned()),
                space: Some(binding.space_tag.clone()),
            },
            snapshot,
        };
        preflight(binding, &command)?;
        let membership = self.begin_binding_membership_mutation(scope, &command, None)?;
        Ok((command, membership))
    }

    /// Journal closing a session that the Space at `scope` holds.
    /// # Errors
    /// Returns why the request was refused. Nothing has been sent to the backend.
    pub fn begin_session_close(
        &mut self,
        scope: SpaceId,
        session_id: &str,
    ) -> Result<PreparedSessionRequest, SessionRequestError> {
        let binding = self.live_binding(scope)?;
        let session = binding
            .mux
            .backend_session_by_id_or_name(session_id)
            .ok_or_else(|| {
                SessionRequestError::Unavailable(format!("session {session_id} no longer exists"))
            })?;
        if !binding.holds(session) {
            return Err(SessionRequestError::Invalid(format!(
                "this Space does not hold session {}",
                session.name
            )));
        }
        let command = MuxCommand::DitchSession {
            session_id: session.id.clone(),
        };
        preflight(binding, &command)?;
        let membership = self.begin_binding_membership_mutation(scope, &command, None)?;
        Ok((command, membership))
    }

    /// Start the first pane of a session `command` explicitly created (argv or the default shell),
    /// when Bootty rather than the backend runs that pane. tmux and rmux started it with the
    /// session. A native pane would start the first time it is shown, so this starts it now,
    /// hidden, in the owner that will later show it. The argv lives only in the command: nothing
    /// replays it.
    pub(super) fn start_session_command(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
    ) -> Result<(), MuxCommandError> {
        // An explicit create starts its first pane now, even with the default shell, so the new
        // session takes input and capture before anyone shows it.
        let Some((pane, argv)) = self.session_command_pane(scope, command)? else {
            return Ok(());
        };
        self.space_terminal_owner(scope)
            .and_then(|owner| {
                owner
                    .terminal
                    .start_scoped_native_command(scope, pane, argv.to_vec())
            })
            .map_err(|error| MuxCommandError::Failed(format!("{error:#}")))
    }

    /// The pane [`Self::start_session_command`] started for `command`, which just landed. `None`
    /// when Bootty started no pane for it.
    /// # Errors
    /// Returns why the created session cannot be found.
    pub fn starting_session(
        &self,
        scope: SpaceId,
        command: &MuxCommand,
    ) -> Result<Option<StartingSession>, MuxCommandError> {
        let Some((pane, _)) = self.session_command_pane(scope, command)? else {
            return Ok(None);
        };
        let binding = self.binding(scope).ok_or(MuxCommandError::Stale)?;
        let window_id = binding
            .mux
            .backend_session_by_id_or_name(&pane.session_id)
            .and_then(|session| {
                session.windows.iter().find(|window| {
                    window
                        .panes
                        .iter()
                        .any(|candidate| candidate.pane_id == pane.pane_id)
                })
            })
            .map(|window| window.id.clone())
            .ok_or_else(|| MuxCommandError::Failed("created pane has no window".to_owned()))?;
        let pane_ids = if matches!(command, MuxCommand::RestoreSession { .. }) {
            binding
                .mux
                .backend_session_by_id_or_name(&pane.session_id)
                .into_iter()
                .flat_map(|session| &session.windows)
                .flat_map(|window| &window.panes)
                .filter_map(|pane| pane.pane_id.clone())
                .collect()
        } else {
            pane.pane_id.clone().into_iter().collect()
        };
        Ok(Some(StartingSession {
            pane_ids,
            window_id,
            created_session: matches!(
                command,
                MuxCommand::CreateProjectSession { .. }
                    | MuxCommand::CreateWorktreeSession { .. }
                    | MuxCommand::RestoreSession { .. }
            ),
            scope,
            generation: binding.mux.binding_generation(),
            session_id: pane.session_id,
            pane_id: pane.pane_id.unwrap_or_default(),
        }))
    }

    /// Whether `starting`'s pane is running: `Pending` while its process starts in the
    /// background, and why it could not start otherwise, such as a program that is not on the
    /// pane's `PATH`.
    pub fn session_startup(
        &mut self,
        starting: &StartingSession,
    ) -> Poll<Result<(), MuxCommandError>> {
        if !self.holds_starting_session(starting) {
            return Poll::Ready(Err(MuxCommandError::Failed(format!(
                "session {} closed before it started",
                starting.session_id
            ))));
        }
        for id in &starting.pane_ids {
            let Some(runtime) = self.space_terminal_runtime(starting.scope, id) else {
                return Poll::Ready(Err(MuxCommandError::Failed(format!(
                    "pane {id} closed before it started"
                ))));
            };
            match runtime.started() {
                Ok(true) => {}
                Ok(false) => return Poll::Pending,
                Err(error) => {
                    return Poll::Ready(Err(MuxCommandError::Failed(format!("{error:#}"))));
                }
            }
        }
        Poll::Ready(Ok(()))
    }

    /// Whether the session `starting` watches still exists as the create made it: same binding,
    /// same session, still holding the pane.
    #[must_use]
    pub fn holds_starting_session(&self, starting: &StartingSession) -> bool {
        self.binding(starting.scope).is_some_and(|binding| {
            binding.mux.binding_generation() == starting.generation
                && binding
                    .mux
                    .backend_session_by_id_or_name(&starting.session_id)
                    .is_some_and(|session| {
                        session.id == starting.session_id
                            && session.windows.iter().any(|window| {
                                window
                                    .panes
                                    .iter()
                                    .any(|pane| pane.pane_id.as_deref() == Some(&starting.pane_id))
                            })
                    })
        })
    }

    /// The first pane of the session an explicit create made, with its argv, when Bootty rather
    /// than the backend runs the pane. `None` for any other command or backend.
    fn session_command_pane<'command>(
        &self,
        scope: SpaceId,
        command: &'command MuxCommand,
    ) -> Result<Option<(MuxPaneAnchor, &'command [String])>, MuxCommandError> {
        let (session_id, argv, window_created) = match command {
            MuxCommand::CreateProjectSession {
                session_id,
                argv: Some(argv),
                ..
            } => (session_id, argv.as_slice(), false),
            MuxCommand::NewWindow {
                session_id, argv, ..
            } => (session_id, argv.as_deref().unwrap_or_default(), true),
            MuxCommand::CreatePane {
                session_id, argv, ..
            } => (session_id, argv.as_slice(), true),
            MuxCommand::CreateWorktreeSession { session_id, .. } => (session_id, &[][..], false),
            MuxCommand::SplitPane { session_id, .. }
            | MuxCommand::RestoreSession { session_id, .. } => (session_id, &[][..], true),
            _ => return Ok(None),
        };
        let Some(binding) = self.binding(scope) else {
            return Err(MuxCommandError::Stale);
        };
        if binding.backend_policy.panes.topology != PaneTopology::ProcessLocal {
            return Ok(None);
        }
        let pane = binding
            .mux
            .backend_session_by_id_or_name(session_id)
            .and_then(|session| {
                let window = if window_created {
                    session
                        .windows
                        .iter()
                        .find(|window| Some(&window.id) == session.active_window_id.as_ref())?
                } else {
                    session.windows.first()?
                };
                if matches!(command, MuxCommand::RestoreSession { .. }) {
                    Some(window.anchor.clone())
                } else if matches!(
                    command,
                    MuxCommand::CreatePane { .. } | MuxCommand::SplitPane { .. }
                ) {
                    window.panes.last().cloned()
                } else {
                    window.panes.first().cloned()
                }
            })
            .ok_or_else(|| {
                MuxCommandError::Failed(format!("session {session_id} has no pane to start"))
            })?;
        Ok(Some((pane, argv)))
    }

    fn live_binding(&self, scope: SpaceId) -> Result<&BindingRuntime, SessionRequestError> {
        self.binding(scope)
            .ok_or_else(|| SessionRequestError::Unavailable("the target Space was closed".into()))
    }
}

impl BindingRuntime {
    fn reopen_session_name(&self, original: &str) -> Result<String, SessionRequestError> {
        let mut name = original.to_owned();
        let mut suffix = 1usize;
        while self
            .mux
            .all_sessions()
            .iter()
            .any(|session| session.name == name || session.id == name)
            || self
                .pending_generated_names
                .values()
                .any(|pending| pending.name == name)
        {
            name = format!("{original}-restore-{suffix}");
            suffix = suffix.checked_add(1).ok_or_else(|| {
                SessionRequestError::Unavailable("restored session name capacity exhausted".into())
            })?;
        }
        validate_session_name(&name)?;
        Ok(name)
    }

    /// A saved identity's observed backend attachment; process status remains backend-owned.
    #[must_use]
    pub fn session_attachment(&self, identity: &str) -> Option<&MuxSession> {
        self.mux.all_sessions().iter().find(|session| {
            session.tag.identity.as_deref() == Some(identity)
                && session.tag.space.as_deref() == Some(self.space_tag.as_str())
        })
    }

    /// The backend sessions this binding holds, in its order: its tagged members when it tracks
    /// membership, otherwise everything its backend reports.
    #[must_use]
    pub fn member_sessions(&self) -> Vec<&MuxSession> {
        let reported = self.mux.all_sessions();
        if !self.tracks_session_membership() {
            return reported.iter().collect();
        }
        self.sessions
            .sessions()
            .iter()
            .filter_map(|claimed| self.session_attachment(&claimed.identity))
            .collect()
    }

    fn holds(&self, session: &MuxSession) -> bool {
        !self.tracks_session_membership()
            || session
                .tag
                .identity
                .as_deref()
                .is_some_and(|identity| self.sessions.get(identity).is_some())
    }
}

fn validate_restore_snapshot(
    snapshot: &crate::session_snapshot::SavedTerminalSession,
    topology: PaneTopology,
) -> Result<(), SessionRequestError> {
    snapshot.validate().map_err(SessionRequestError::Invalid)?;
    if topology == PaneTopology::ProcessLocal {
        for pane in snapshot.windows.iter().flat_map(|window| &window.panes) {
            if !Path::new(&pane.cwd).is_dir() {
                return Err(SessionRequestError::Invalid(format!(
                    "saved cwd {:?} is not a directory",
                    pane.cwd
                )));
            }
        }
    }
    Ok(())
}

fn preflight(binding: &BindingRuntime, command: &MuxCommand) -> Result<(), SessionRequestError> {
    preflight_binding_command(binding, command).map_err(|error| match error {
        MuxCommandError::Unsupported => SessionRequestError::Unsupported(
            "this Space's backend does not support this session operation".to_owned(),
        ),
        MuxCommandError::Failed(message) => SessionRequestError::Unavailable(message),
        error => SessionRequestError::Unavailable(error.to_string()),
    })
}

/// A name every backend stores and resolves exactly as given.
fn validate_session_name(name: &str) -> Result<(), SessionRequestError> {
    let invalid = |reason: String| Err(SessionRequestError::Invalid(reason));
    if name.is_empty() {
        return invalid("the session name is empty".to_owned());
    }
    if name.len() > SESSION_NAME_MAX_BYTES {
        return invalid(format!(
            "the session name is longer than {SESSION_NAME_MAX_BYTES} bytes"
        ));
    }
    if name.chars().any(char::is_control) {
        return invalid(format!(
            "session name {name:?} contains a control character"
        ));
    }
    // tmux and rmux rewrite `:` and `.` and escape `\`; tmux expands `#` formats in a name.
    if let Some(character) = name.chars().find(|c| matches!(c, ':' | '.' | '\\' | '#')) {
        return invalid(format!("session name {name:?} contains {character:?}"));
    }
    // `-` reads as a flag, and tmux targets read `$`, `@` and `%` as ids and `=` as exact-match.
    if name.starts_with(['-', '$', '@', '%', '=']) {
        return invalid(format!(
            "session name {name:?} cannot start with '-', '$', '@', '%' or '='"
        ));
    }
    Ok(())
}

fn validate_cwd(cwd: &str) -> Result<(), SessionRequestError> {
    // A remote host's path is POSIX even when this host's is not.
    if !(cwd.starts_with('/') || Path::new(cwd).is_absolute()) || cwd.contains('\0') {
        return Err(SessionRequestError::Invalid(format!(
            "cwd {cwd:?} is not an absolute path"
        )));
    }
    Ok(())
}

fn validate_argv(argv: &[String]) -> Result<(), SessionRequestError> {
    let invalid = |reason: String| Err(SessionRequestError::Invalid(reason));
    if argv.len() > SESSION_ARGV_MAX_ELEMENTS {
        return invalid(format!(
            "argv has {} elements; the limit is {SESSION_ARGV_MAX_ELEMENTS}",
            argv.len()
        ));
    }
    if argv.first().is_some_and(String::is_empty) {
        return invalid("argv names an empty program".to_owned());
    }
    if argv.iter().any(|argument| argument.contains('\0')) {
        return invalid("argv elements cannot contain NUL".to_owned());
    }
    let bytes = argv.iter().fold(0_usize, |total, argument| {
        total.saturating_add(argument.len()).saturating_add(1)
    });
    if bytes > SESSION_ARGV_MAX_BYTES {
        return invalid(format!(
            "argv is {bytes} bytes; the limit is {SESSION_ARGV_MAX_BYTES}"
        ));
    }
    Ok(())
}
