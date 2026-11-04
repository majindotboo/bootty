use super::{BindingRuntime, PendingGeneratedName, WorkspaceRuntime};
use crate::{RepaintHandle, command::MuxCommand, executor};
use crate::{repository::WorkspacePersistenceError, session_names};

fn resolve_session_cwd(cwd: &str, remote: bool) -> String {
    if remote {
        cwd.to_owned()
    } else {
        session_root(cwd)
    }
}

fn suggested_session_name(cwd: &str, remote: bool) -> String {
    if remote {
        session_names::session_name_for_remote_path(cwd)
    } else {
        bootty_git::suggested_session_name(cwd)
    }
}

impl BindingRuntime {
    /// Where a session lives, as bootty records it: the worktree root for a local session, and
    /// whatever the far side reported for a remote one.
    ///
    /// Memoised, because this is on the frame path and resolving a local one forks `git`.
    pub fn session_cwd(&self, cwd: &str) -> String {
        if self.multiplexer.remote.is_some() {
            return resolve_session_cwd(cwd, true);
        }
        if let Some(resolved) = self.session_roots.borrow().get(cwd) {
            return resolved.clone();
        }
        let resolved = resolve_session_cwd(cwd, false);
        self.session_roots
            .borrow_mut()
            .insert(cwd.to_owned(), resolved.clone());
        resolved
    }

    pub fn poll_membership_command(&mut self) {
        // Reserve generated names only until the backend reports their attachment.
        self.pending_generated_names.retain(|_, pending| {
            !self
                .mux
                .all_sessions()
                .iter()
                .any(|session| session.name == pending.name)
        });
    }

    pub fn clear_pending_generated_names(&mut self) {
        self.pending_generated_names.clear();
    }
}

fn session_root(cwd: &str) -> String {
    let cwd = bootty_git::worktree_root(cwd).unwrap_or_else(|| cwd.to_owned());
    std::fs::canonicalize(&cwd)
        .unwrap_or_else(|_| cwd.into())
        .to_string_lossy()
        .into_owned()
}

impl WorkspaceRuntime {
    /// Release a session only after an authoritative successful close, before topology publication.
    pub(crate) fn release_closed_session(
        &mut self,
        scope: crate::controller::SpaceId,
        command: &crate::command::MuxCommand,
        result: &crate::controller::MuxCommandResult,
    ) -> Result<(), WorkspacePersistenceError> {
        let crate::command::MuxCommand::DitchSession { session_id } = command else {
            return Ok(());
        };
        let Some(binding) = self.binding(scope) else {
            return Ok(());
        };
        if !result
            .as_ref()
            .is_ok_and(|completion| completion.matches_config(binding.multiplexer()))
        {
            return Ok(());
        }
        let identity = self.session_identity(scope, session_id);
        if let Some(identity) = identity
            && let Some(mut candidate) = self.binding_state_candidate(scope)
        {
            candidate.sessions.release(&identity);
            self.commit_binding_state_candidate(candidate)?;
        }
        Ok(())
    }

    fn taken_session_names(&self, keep: Option<&str>) -> Vec<String> {
        self.all_bindings()
            .flat_map(|binding| {
                binding.mux.backend_session_names().iter().cloned().chain(
                    binding
                        .pending_generated_names
                        .values()
                        .map(|pending| pending.name.clone()),
                )
            })
            .filter(|name| Some(name.as_str()) != keep)
            .collect()
    }

    #[must_use]
    pub fn project_session_title(&self, cwd: &str) -> String {
        suggested_session_name(cwd, self.active.binding.multiplexer.remote.is_some())
    }

    pub fn project_session_command(&self, cwd: &str) -> MuxCommand {
        let cwd = self.active.binding.session_cwd(cwd);
        let display_name = self.project_session_title(&cwd);
        let candidate = session_names::generated_session_name(&display_name);
        let session_id = session_names::unique_session_name(
            &candidate,
            self.taken_session_names(None).iter().map(String::as_str),
        );
        MuxCommand::CreateProjectSession {
            session_id,
            cwd,
            tag: self.active.binding.new_session_tag(),
            argv: None,
        }
    }

    /// # Errors
    /// Returns membership journal or persistence errors while creating the session.
    pub fn create_project_session(
        &mut self,
        command: &MuxCommand,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        let remote = self.active.binding.multiplexer.remote.is_some();
        let MuxCommand::CreateProjectSession {
            session_id,
            cwd,
            tag: _,
            argv: None,
        } = command
        else {
            return Err(WorkspacePersistenceError::operation(
                "project session creation received a non-project command",
            ));
        };
        let display_name = suggested_session_name(cwd, remote);
        let pending_name = PendingGeneratedName {
            name: session_id.clone(),
            display_name,
            explicit: false,
            initial_state: crate::session_membership::SessionState::default(),
        };
        let membership =
            self.begin_active_binding_membership_mutation(command, Some(&pending_name))?;
        self.active
            .binding
            .pending_generated_names
            .insert(session_id.clone(), pending_name);
        let pending = executor::submit_authoritative_command(
            self,
            repaint,
            command.clone(),
            membership.map(Box::new),
            None,
        );
        let result = pending
            .result
            .try_recv()
            .map_err(|error| WorkspacePersistenceError::operation(error.to_string()))?;
        let (result, sync_error) = executor::complete_authoritative_command(
            self,
            pending.scope,
            command,
            pending.membership.as_deref(),
            result,
        )?;
        result.map_err(|error| WorkspacePersistenceError::operation(error.to_string()))?;
        if let Some(error) = sync_error {
            return Err(WorkspacePersistenceError::operation(error));
        }
        Ok(true)
    }

    /// Edit a saved purpose title without changing backend topology or names.
    /// # Errors
    /// Rejects invalid titles and commits before publishing the change.
    pub fn set_session_title(
        &mut self,
        scope: crate::controller::SpaceId,
        identity: &str,
        title: &str,
    ) -> Result<bool, WorkspacePersistenceError> {
        validate_session_title(title)?;
        let Some(mut candidate) = self.binding_state_candidate(scope) else {
            return Err(WorkspacePersistenceError::operation(
                "the Space no longer exists",
            ));
        };
        if !candidate.sessions.contains(identity) {
            return Err(WorkspacePersistenceError::operation(
                "this Space does not hold the saved session",
            ));
        }
        if !candidate
            .sessions
            .set_display_name(identity, title.trim(), true)
        {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate).map(|_| true)
    }

    /// Commit saved presentation and visibility independently of a backend attachment.
    /// # Errors
    /// Rejects foreign identities, invalid activity, pending operations and failed writes.
    pub fn set_session_state(
        &mut self,
        scope: crate::controller::SpaceId,
        identity: &str,
        change: crate::session_membership::SessionStateChange,
    ) -> Result<bool, WorkspacePersistenceError> {
        use crate::session_membership::SessionStateChange;

        if matches!(change, SessionStateChange::RecordActivity { at, now } if at < 0 || at > now) {
            return Err(WorkspacePersistenceError::operation(
                "activity must be non-negative UTC seconds no later than the accepted clock",
            ));
        }
        let Some(mut candidate) = self.binding_state_candidate(scope) else {
            return Err(WorkspacePersistenceError::operation(
                "the Space no longer exists",
            ));
        };
        let Some(_) = candidate.sessions.get(identity) else {
            return Err(WorkspacePersistenceError::operation(
                "this Space does not hold the saved session",
            ));
        };
        if !candidate.sessions.set_state(identity, change) {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate).map(|_| true)
    }
}

/// One title boundary for saved edits and explicit creation.
pub(super) fn validate_session_title(title: &str) -> Result<(), WorkspacePersistenceError> {
    if title.trim().is_empty() || title.len() > 256 || title.chars().any(char::is_control) {
        return Err(WorkspacePersistenceError::operation(
            "session title must be 1–256 bytes without control characters",
        ));
    }
    Ok(())
}
