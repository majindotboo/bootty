//! Explicit session create and close in any Space, and the Space listing scripts address them by.

use std::{task::Poll, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome, ResourceKind};
use bootty_mux::{
    command::MuxCommand,
    controller::SpaceId,
    executor,
    provider::PaneTopology,
    workspace::{SessionRequestError, StartingSession},
};

use super::{
    CommandDispatch, PendingCommandResult, command_outcome_for_mux_error, command_outcome_message,
};
use crate::{
    commands::{ExactMuxTarget, SessionAction},
    state::AppState,
};

impl AppState {
    /// Create or close a session in the target's Space, leaving selection, focus and the active
    /// Space alone, or list the Spaces.
    pub(super) fn dispatch_session_command(
        &mut self,
        action: SessionAction,
        arguments: &[String],
        exact_target: Option<ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        // Native panes whose processes end once their topology close succeeds.
        let mut closed_native_panes = Vec::new();
        let prepared = match (action, exact_target) {
            (SessionAction::ListSpaces, _) => {
                let outcome = executor::begin_synchronous_command(execution).map_or_else(
                    command_outcome_for_mux_error,
                    |()| CommandOutcome::Success {
                        value: self.spaces_listing(),
                        warnings: Vec::new(),
                    },
                );
                return CommandDispatch::Complete(outcome);
            }
            (
                SessionAction::Create | SessionAction::Start,
                Some(ExactMuxTarget::Binding(scope)),
            ) => {
                let (name, cwd, argv) = match session_create_arguments(arguments) {
                    Ok(arguments) => arguments,
                    Err(outcome) => return self.reject_command(outcome),
                };
                self.workspace
                    .begin_session_create(scope, name, cwd, argv)
                    .map(|prepared| (scope, prepared))
            }
            (SessionAction::Close, Some(ExactMuxTarget::Session(scope, session))) => {
                let closing = MuxCommand::DitchSession {
                    session_id: session.clone(),
                };
                if self.mux_command_pending(scope, &closing) {
                    return self.reject_command(CommandOutcome::Unavailable {
                        message: "This session is already being closed".to_owned(),
                    });
                }
                closed_native_panes = self.native_session_panes(scope, &session);
                self.workspace
                    .begin_session_close(scope, &session)
                    .map(|prepared| (scope, prepared))
            }
            (
                SessionAction::ClosePane,
                Some(ExactMuxTarget::Pane(scope, session, window, pane)),
            ) => {
                // A native session ends with its last pane, as tmux and rmux sessions do.
                if self.is_last_native_pane(scope, &session, &pane) {
                    return self.dispatch_session_command(
                        SessionAction::Close,
                        &[],
                        Some(ExactMuxTarget::Session(scope, session)),
                        execution,
                    );
                }
                let (command, native) = self.pane_close_command(scope, session, window, pane);
                closed_native_panes = native;
                Ok((scope, (command, None)))
            }
            _ => {
                return self.reject_command(CommandOutcome::StaleTarget {
                    message: "The session command's target is no longer live".to_owned(),
                });
            }
        };
        let (scope, (command, membership)) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.reject_command(session_request_outcome(error)),
        };
        let Some(submitted) = executor::submit_authoritative_command_for_scope(
            &mut self.workspace,
            &self.repaint,
            scope,
            command,
            membership.map(Box::new),
            execution,
            action.command_selection(),
        ) else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "The target Space was closed".to_owned(),
            });
        };
        // A backend that runs commands on this thread has already answered. Finish now: a frame in
        // between could show a native session's new pane and start it with a shell, not its argv.
        if let Ok(result) = submitted.result.try_recv() {
            // A native pane's process goes only once its topology close has succeeded.
            if result.is_ok() {
                for (session, window, pane) in &closed_native_panes {
                    self.workspace
                        .discard_space_pane(submitted.scope, session, window, pane);
                }
            }
            let outcome = self.command_outcome_for_mux_result(
                submitted.scope,
                &submitted.command,
                submitted.membership.as_deref(),
                result,
                submitted.layout.as_ref(),
            );
            return self.hold_until_session_starts(submitted.scope, &submitted.command, outcome);
        }
        CommandDispatch::Pending(PendingCommandResult::Mux {
            scope: submitted.scope,
            command: submitted.command,
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        })
    }

    /// Hold an explicit create's success until the first pane Bootty started for it runs.
    fn hold_until_session_starts(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        outcome: CommandOutcome,
    ) -> CommandDispatch {
        let MuxCommand::CreateProjectSession { session_id, .. } = command else {
            return CommandDispatch::Complete(outcome);
        };
        if !matches!(outcome, CommandOutcome::Success { .. }) {
            return CommandDispatch::Complete(outcome);
        }
        let starting = match self.workspace.starting_session(scope, command) {
            Ok(Some(starting)) => starting,
            Ok(None) => return CommandDispatch::Complete(outcome),
            Err(error) => return CommandDispatch::Complete(command_outcome_for_mux_error(error)),
        };
        match self.poll_session_start(&starting, session_id, &outcome) {
            Poll::Ready(outcome) => CommandDispatch::Complete(outcome),
            Poll::Pending => CommandDispatch::Pending(PendingCommandResult::SessionStart {
                starting,
                name: session_id.clone(),
                outcome,
            }),
        }
    }

    /// Poll the pane an explicit create started. A pane whose program could not start fails the
    /// command, and the session it would have shown is closed through `session.close`, so the
    /// name is free for a retry. Only that session is closed: one recreated under the same name
    /// meanwhile is left alone.
    pub(super) fn poll_session_start(
        &mut self,
        starting: &StartingSession,
        name: &str,
        outcome: &CommandOutcome,
    ) -> Poll<CommandOutcome> {
        let error = match self.workspace.session_startup(starting) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(())) => return Poll::Ready(outcome.clone()),
            Poll::Ready(Err(error)) => error,
        };
        let cleanup = if self.workspace.holds_starting_session(starting) {
            let closed = self.dispatch_session_command(
                SessionAction::Close,
                &[],
                Some(ExactMuxTarget::Session(
                    starting.scope(),
                    starting.session_id().to_owned(),
                )),
                None,
            );
            match closed {
                CommandDispatch::Complete(CommandOutcome::Success { .. }) => {
                    "; the session was closed".to_owned()
                }
                CommandDispatch::Complete(failed) => format!(
                    "; closing the session failed: {}",
                    command_outcome_message(&failed).unwrap_or_default()
                ),
                CommandDispatch::Pending(_) => "; the session is closing".to_owned(),
            }
        } else {
            String::new()
        };
        Poll::Ready(CommandOutcome::Failed {
            code: "session_start_failed".to_owned(),
            message: format!("session {name} could not start: {error}{cleanup}"),
        })
    }

    /// Native panes close in the local topology and then end their process; attach backends kill
    /// the backend pane, which ends it.
    fn pane_close_command(
        &self,
        scope: SpaceId,
        session: String,
        window: String,
        pane: String,
    ) -> (MuxCommand, Vec<(String, String, String)>) {
        let native = self
            .workspace
            .binding(scope)
            .is_some_and(bootty_mux::workspace::BindingRuntime::uses_native_terminal_layout);
        if native {
            let closing = MuxCommand::ClosePane {
                session_id: session.clone(),
                pane_id: Some(pane.clone()),
            };
            (closing, vec![(session, window, pane)])
        } else {
            let killing = MuxCommand::KillPane {
                session_id: session,
                pane_id: Some(pane),
            };
            (killing, Vec::new())
        }
    }

    /// Every pane of a native session as (session, window, pane); empty for attach backends,
    /// whose backend ends the processes itself.
    /// Whether `pane` is the only pane of a session whose processes Bootty runs.
    fn is_last_native_pane(&self, scope: SpaceId, session: &str, pane: &str) -> bool {
        let process_local = self.workspace.binding(scope).is_some_and(|binding| {
            binding.backend_policy().panes.topology == PaneTopology::ProcessLocal
        });
        // A window lists its anchor pane besides its panes.
        let panes = self
            .native_session_panes(scope, session)
            .into_iter()
            .map(|(_, _, pane)| pane)
            .collect::<std::collections::BTreeSet<_>>();
        process_local && panes.len() == 1 && panes.contains(pane)
    }

    fn native_session_panes(&self, scope: SpaceId, session: &str) -> Vec<(String, String, String)> {
        let Some(binding) = self
            .workspace
            .binding(scope)
            .filter(|binding| binding.uses_native_terminal_layout())
        else {
            return Vec::new();
        };
        binding
            .mux()
            .all_sessions()
            .iter()
            .filter(|candidate| candidate.id == session || candidate.name == session)
            .flat_map(|candidate| {
                candidate.windows.iter().flat_map(move |window| {
                    std::iter::once(&window.anchor)
                        .chain(&window.panes)
                        .filter_map(move |anchor| {
                            let pane = anchor.pane_id.clone()?;
                            Some((candidate.id.clone(), window.id.clone(), pane))
                        })
                })
            })
            .collect()
    }

    /// Every Space in order, with the targets a script needs to address it and its sessions.
    fn spaces_listing(&self) -> serde_json::Value {
        let mut spaces = self.workspace.spaces().collect::<Vec<_>>();
        spaces.sort_by_key(|space| space.position);
        spaces
            .into_iter()
            .map(|space| {
                let binding = &space.binding;
                let scope = binding.scope();
                let mux = binding.mux();
                let handle = self.binding_target_handle(scope, mux.binding_generation());
                let sessions = binding
                    .member_sessions()
                    .into_iter()
                    .map(|session| {
                        let opaque=binding.multiplexer().backend==bootty_config::config::MultiplexerBackendConfig::Herdr;
                        let windows=if opaque {Vec::new()} else {session.windows.iter().map(|window|{
                            let mut seen=std::collections::BTreeSet::new();
                            let panes=std::iter::once(&window.anchor).chain(&window.panes).filter_map(|pane|{
                                let id=pane.pane_id.as_deref()?;
                                if !seen.insert(id) {return None;}
                                Some(serde_json::json!({"target":self.mux_pane_target(scope,ResourceKind::Pane,&session.id,&window.id,id),"terminal_target":self.mux_pane_target(scope,ResourceKind::Terminal,&session.id,&window.id,id),"cwd":pane.cwd}))
                            }).collect::<Vec<_>>();
                            serde_json::json!({"name":window.name,"target":self.mux_resource_target(scope,ResourceKind::MuxWindow,&session.id,Some(&window.id)),"panes":panes})
                        }).collect::<Vec<_>>()};
                        let pane_target=session.windows.iter().find(|window|Some(window.id.as_str())==session.active_window_id.as_deref()).or_else(||session.windows.first()).and_then(|window|window.anchor.pane_id.as_deref().and_then(|pane|self.mux_pane_target(scope,ResourceKind::Pane,&session.id,&window.id,pane)));
                        serde_json::json!({
                            "name": session.name,
                            "pane_target": pane_target,
                            "topology_supported": !opaque,
                            "windows": windows,
                            "terminal_target": self.session_terminal_target(scope, &session.id),
                            "cwd": session.anchor.cwd,
                            "target": self.mux_resource_target(
                                scope,
                                ResourceKind::Session,
                                &session.id,
                                None,
                            ),
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({
                    "scope": scope.persistence_value().to_string(),
                    "name": space.name,
                    "active": space.id == self.workspace.active.id,
                    "backend": binding.multiplexer().backend,
                    "host": binding
                        .multiplexer()
                        .remote
                        .as_ref()
                        .map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label),
                    "target": ExactMuxTarget::Binding(scope).command_target(
                        ResourceKind::Binding,
                        mux,
                        &handle,
                    ),
                    "sessions": sessions,
                })
            })
            .collect()
    }
}

/// `name`, `cwd`, and the optional argv decoded from its JSON array.
fn session_create_arguments(
    arguments: &[String],
) -> Result<(&str, &str, Vec<String>), CommandOutcome> {
    let invalid = |message: String| CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message,
    };
    let (name, cwd, argv) = match arguments {
        [name, cwd] => (name.as_str(), cwd.as_str(), None),
        [name, cwd, argv] => (name.as_str(), cwd.as_str(), Some(argv)),
        _ => {
            return Err(invalid(
                "session.create expects a name, a cwd and an optional argv".to_owned(),
            ));
        }
    };
    let argv = argv
        .map(|argv| serde_json::from_str::<Vec<String>>(argv))
        .transpose()
        .map_err(|error| invalid(format!("argv must be a JSON array of strings: {error}")))?
        .unwrap_or_default();
    Ok((name, cwd, argv))
}

fn session_request_outcome(error: SessionRequestError) -> CommandOutcome {
    match error {
        SessionRequestError::Invalid(message) => CommandOutcome::Failed {
            code: "invalid_arguments".to_owned(),
            message,
        },
        SessionRequestError::NameTaken(message) => CommandOutcome::Failed {
            code: "session_exists".to_owned(),
            message,
        },
        SessionRequestError::Unsupported(message) => CommandOutcome::Unsupported { message },
        SessionRequestError::Unavailable(message) => CommandOutcome::Unavailable { message },
        SessionRequestError::Persistence(error) => CommandOutcome::Failed {
            code: "persistence_failed".to_owned(),
            message: error.to_string(),
        },
    }
}
