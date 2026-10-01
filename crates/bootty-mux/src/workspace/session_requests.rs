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
pub const SESSION_NAME_MAX_BYTES: usize = 256;
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
}

impl StartingSession {
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
        validate_session_name(name)?;
        validate_cwd(cwd)?;
        validate_argv(&argv)?;
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
            tag: binding.new_session_tag(),
            argv: Some(argv),
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
        Ok(Some(StartingSession {
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
        let Some(runtime) = self.space_terminal_runtime(starting.scope, &starting.pane_id) else {
            return Poll::Ready(Err(MuxCommandError::Failed(format!(
                "pane {} closed before it started",
                starting.pane_id
            ))));
        };
        match runtime.started() {
            Ok(true) => Poll::Ready(Ok(())),
            Ok(false) => Poll::Pending,
            Err(error) => Poll::Ready(Err(MuxCommandError::Failed(format!("{error:#}")))),
        }
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
        let MuxCommand::CreateProjectSession {
            session_id,
            argv: Some(argv),
            ..
        } = command
        else {
            return Ok(None);
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
            .and_then(|session| session.windows.first()?.panes.first().cloned())
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
            .filter_map(|claimed| {
                reported
                    .iter()
                    .find(|session| session.tag.identity.as_deref() == Some(&claimed.identity))
            })
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
    if let Some(character) = name
        .chars()
        .find(|&c| crate::session_names::invalid_session_name_character(c))
    {
        return invalid(format!("session name {name:?} contains {character:?}"));
    }
    // `-` reads as a flag, and tmux targets read `$`, `@` and `%` as ids and `=` as exact-match.
    if name
        .chars()
        .next()
        .is_some_and(crate::session_names::reserved_session_name_start)
    {
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
