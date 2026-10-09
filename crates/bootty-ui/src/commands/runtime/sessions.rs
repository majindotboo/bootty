//! Explicit session create and close in any Space, and the Space listing scripts address them by.

use std::{task::Poll, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome, ResourceKind};
use bootty_mux::{
    command::MuxCommand,
    controller::{CommandSelection, SpaceId},
    executor,
    provider::PaneTopology,
    session_membership::{SessionLifecycle, SessionStateChange},
    workspace::{PreparedSessionRequest, SessionRequestError, StartingSession},
};

use super::{
    CommandDispatch, PendingCommandResult, command_outcome_for_mux_error, command_outcome_message,
    serialized_command_outcome,
};
use crate::{
    commands::{ExactMuxTarget, SessionAction},
    state::AppState,
};

impl AppState {
    fn dispatch_project_command(
        &mut self,
        action: SessionAction,
        scope: SpaceId,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return self.reject_command(command_outcome_for_mux_error(error));
        }
        if action == SessionAction::ConfigureProject {
            let result = match arguments {
                [cwd, encoded] => serde_json::from_str(encoded)
                    .map_err(|error| error.to_string())
                    .and_then(|settings| {
                        self.workspace
                            .configure_project(scope, cwd, settings)
                            .map_err(|error| error.to_string())
                    }),
                _ => Err("project.configure expects a directory and settings JSON".to_owned()),
            };
            if let Err(message) = result {
                return self.reject_command(CommandOutcome::Failed {
                    code: "project_failed".into(),
                    message,
                });
            }
        } else if action != SessionAction::ListProjects {
            let [cwd] = arguments else {
                return self.reject_command(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message: "Choose one project path".to_owned(),
                });
            };
            let result = if action == SessionAction::RegisterProject {
                self.workspace.register_project(scope, cwd)
            } else {
                self.workspace.toggle_project_collapsed(scope, cwd)
            };
            if let Err(error) = result {
                return self.reject_command(CommandOutcome::Failed {
                    code: "project_failed".to_owned(),
                    message: error.to_string(),
                });
            }
        }
        CommandDispatch::Complete(serialized_command_outcome(
            self.workspace
                .registered_projects(scope)
                .cloned()
                .collect::<Vec<_>>(),
        ))
    }

    /// Create or close a session in the target's Space, leaving selection, focus and the active
    /// Space alone, or list the Spaces.
    pub(super) fn dispatch_session_command(
        &mut self,
        action: SessionAction,
        arguments: &[String],
        exact_target: Option<ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if action == SessionAction::InspectSpace
            && let Some(ExactMuxTarget::Binding(scope)) = exact_target
        {
            return self.inspect_space_command(scope, execution);
        }
        if action == SessionAction::ListTerminals
            && let Some(ExactMuxTarget::Binding(scope)) = exact_target
        {
            return self.list_terminals_command(scope, execution);
        }
        if matches!(
            action,
            SessionAction::ListProjects
                | SessionAction::RegisterProject
                | SessionAction::ConfigureProject
                | SessionAction::ToggleProjectCollapsed
        ) {
            let Some(ExactMuxTarget::Binding(scope)) = exact_target else {
                return self.reject_command(CommandOutcome::StaleTarget {
                    message: "The project host is no longer available".to_owned(),
                });
            };
            return self.dispatch_project_command(action, scope, arguments, execution);
        }
        if action == SessionAction::Close
            && let Some(ExactMuxTarget::Session(scope, session)) = &exact_target
            && let Some(identity) = self.workspace.session_identity(*scope, session)
            && self.workspace.binding(*scope).is_some_and(|binding| {
                binding.sessions().get(&identity).is_some()
                    && binding
                        .session_attachment(&identity)
                        .is_some_and(|session| !session.windows.is_empty())
            })
        {
            let native_panes = self
                .workspace
                .binding(*scope)
                .filter(|binding| {
                    binding.backend_policy().panes.topology == PaneTopology::ProcessLocal
                })
                .and_then(|binding| binding.session_attachment(&identity))
                .map(|session| {
                    session
                        .windows
                        .iter()
                        .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes))
                        .filter_map(|pane| pane.pane_id.clone())
                        .collect::<Vec<_>>()
                });
            if native_panes.is_some_and(|panes| {
                !panes.iter().any(|pane| {
                    self.workspace
                        .space_terminal_runtime(*scope, pane)
                        .is_some_and(|runtime| matches!(runtime.started(), Ok(true)))
                })
            }) {
                // A failed or unstarted native process has no output state to checkpoint.
                return self.dispatch_session_command_after_checkpoint(
                    action,
                    arguments,
                    exact_target,
                    execution,
                );
            }
            let Some(target) =
                self.mux_resource_target(*scope, ResourceKind::Session, session, None)
            else {
                return self.reject_command(CommandOutcome::StaleTarget {
                    message: "The session was replaced before checkpoint capture".to_owned(),
                });
            };
            if self.mux_command_pending(
                *scope,
                &MuxCommand::DitchSession {
                    session_id: session.clone(),
                },
            ) {
                return self.reject_command(CommandOutcome::Unavailable {
                    message: "This session is already being closed".to_owned(),
                });
            }
            let captured = self.checkpoint_session(*scope, &identity);
            return CommandDispatch::Pending(PendingCommandResult::SessionCheckpointClose {
                target,
                identity,
                captured,
            });
        }
        self.dispatch_session_command_after_checkpoint(action, arguments, exact_target, execution)
    }

    fn dispatch_session_command_after_checkpoint(
        &mut self,
        action: SessionAction,
        arguments: &[String],
        exact_target: Option<ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if action == SessionAction::SettleCurrent {
            return self.settle_captured_session(exact_target, execution);
        }
        // Native panes whose processes end once their topology close succeeds.
        let mut closed_native_panes = Vec::new();
        let prepared = match (action, exact_target) {
            (
                SessionAction::ListSaved
                | SessionAction::SetTitle
                | SessionAction::Pin
                | SessionAction::Unpin
                | SessionAction::Activity
                | SessionAction::AcceptedInput
                | SessionAction::Settle
                | SessionAction::Activate
                | SessionAction::Archive
                | SessionAction::Unarchive
                | SessionAction::Snooze
                | SessionAction::Unsnooze
                | SessionAction::Hide
                | SessionAction::Show
                | SessionAction::Delete
                | SessionAction::Restore,
                Some(ExactMuxTarget::Binding(scope)),
            ) => {
                return self.saved_session_metadata(action, scope, arguments, execution);
            }
            (SessionAction::Reopen, Some(ExactMuxTarget::Binding(scope))) => {
                let [identity] = arguments else {
                    return self.reject_command(CommandOutcome::Failed {
                        code: "invalid_arguments".to_owned(),
                        message: "session.reopen expects a saved identity".to_owned(),
                    });
                };
                self.workspace
                    .begin_session_reopen(scope, identity)
                    .map(|prepared| (scope, prepared))
            }
            (SessionAction::ListSpaces, _) => {
                return self.list_spaces_command(execution);
            }
            (SessionAction::Create, Some(ExactMuxTarget::Binding(scope))) => {
                return self.dispatch_session_create(scope, arguments, execution);
            }
            (SessionAction::CreateTab, Some(ExactMuxTarget::Session(scope, session))) => {
                return self.dispatch_tab_create(scope, &session, arguments, execution);
            }
            (SessionAction::CreatePane, Some(ExactMuxTarget::Pane(scope, session, _, pane))) => {
                return self.dispatch_pane_create(scope, &session, &pane, arguments, execution);
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
                // Capture the whole task before its final backend pane disappears.
                if self.is_last_session_pane(scope, &session, &pane) {
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
        self.submit_prepared_session_command(
            scope,
            (command, membership),
            &closed_native_panes,
            execution,
            if action == SessionAction::Reopen {
                CommandSelection::Follow
            } else {
                CommandSelection::Preserve
            },
        )
    }

    fn inspect_space_command(
        &self,
        scope: SpaceId,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        CommandDispatch::Complete(
            executor::begin_synchronous_command(execution).map_or_else(
                command_outcome_for_mux_error,
                |()| {
                    let Some(space) = self.workspace.spaces().find(|space| space.binding.scope() == scope) else {
                        return CommandOutcome::StaleTarget { message: "The captured Space is no longer available".into() };
                    };
                    let mux = space.binding.multiplexer();
                    serialized_command_outcome(serde_json::json!({
                        "name":space.name,"backend":mux.backend,
                        "host":mux.remote.as_ref().map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label),
                    }))
                },
            ),
        )
    }

    fn list_terminals_command(
        &self,
        scope: SpaceId,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(command_outcome_for_mux_error(error));
        }
        let Some(binding) = self.workspace.binding(scope) else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The captured Space is no longer available".into(),
            });
        };
        // Bound metadata without walking processes or starting detached sessions. Add cursor
        // pagination if callers need more than 128 observed panes in one Space.
        let handle = self.binding_target_handle(scope, binding.mux().binding_generation());
        let mut terminals = Vec::new();
        for session in binding.member_sessions() {
            for window in &session.windows {
                let panes = if window.panes.is_empty() {
                    std::slice::from_ref(&window.anchor)
                } else {
                    &window.panes
                };
                for pane in panes.iter().filter(|pane| pane.native_agent.is_none()) {
                    let exact = pane.pane_id.as_ref().map_or_else(
                        || ExactMuxTarget::Session(scope, session.id.clone()),
                        |pane| {
                            ExactMuxTarget::Pane(
                                scope,
                                session.id.clone(),
                                window.id.clone(),
                                pane.clone(),
                            )
                        },
                    );
                    let Some(target) =
                        exact.command_target(ResourceKind::Terminal, binding.mux(), &handle)
                    else {
                        continue;
                    };
                    if terminals.len() == 128 {
                        return CommandDispatch::Complete(serialized_command_outcome(
                            serde_json::json!({"terminals":terminals,"truncated":true}),
                        ));
                    }
                    terminals.push(serde_json::json!({
                        "name":window.name,"session":session.name,"target":target,
                    }));
                }
            }
        }
        CommandDispatch::Complete(serialized_command_outcome(
            serde_json::json!({"terminals":terminals,"truncated":false}),
        ))
    }

    pub(super) fn poll_session_checkpoint_close(
        &mut self,
        target: &bootty_control::CommandTarget,
        identity: &str,
        captured: &mut Option<crate::state::SessionCheckpointTicket>,
        execution: (Instant, CommandCancellation),
    ) -> Poll<CommandDispatch> {
        let (scope, session) = match self.resolve_command_target(
            "session.close",
            Some(ResourceKind::Session),
            Some(target),
        ) {
            Ok((_, Some(ExactMuxTarget::Session(scope, session)))) => (scope, session),
            Ok(_) => {
                return Poll::Ready(self.reject_command(CommandOutcome::StaleTarget {
                    message: "The closing session is no longer available".to_owned(),
                }));
            }
            Err(outcome) => return Poll::Ready(self.reject_command(outcome)),
        };
        if self.workspace.session_identity(scope, &session).as_deref() != Some(identity) {
            return Poll::Ready(self.reject_command(CommandOutcome::StaleTarget {
                message: "The closing session changed saved identity".to_owned(),
            }));
        }
        self.poll_session_checkpoints();
        if captured.is_none() {
            *captured = self.checkpoint_session(scope, identity);
            return Poll::Pending;
        }
        let Some(accepted) = captured
            .as_ref()
            .and_then(crate::state::SessionCheckpointTicket::accepted)
        else {
            return Poll::Pending;
        };
        if !accepted {
            return Poll::Ready(
                self.reject_command(CommandOutcome::Failed {
                    code: "session_checkpoint_failed".to_owned(),
                    message: captured.as_ref().and_then(crate::state::SessionCheckpointTicket::failure).map_or_else(
                        || "The session remains open because its current state could not be saved".to_owned(),
                        |reason| format!("The session remains open because its current state could not be saved: {reason}"),
                    ),
                }),
            );
        }
        Poll::Ready(self.dispatch_session_command_after_checkpoint(
            SessionAction::Close,
            &[],
            Some(ExactMuxTarget::Session(scope, session)),
            Some(execution),
        ))
    }

    fn dispatch_pane_create(
        &mut self,
        scope: SpaceId,
        session: &str,
        pane: &str,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let parsed = arguments.split_first().and_then(|(direction, arguments)| {
            let direction = match direction.as_str() {
                "right" => bootty_mux::command::MuxSplitDirection::Right,
                "down" => bootty_mux::command::MuxSplitDirection::Down,
                _ => return None,
            };
            Some((direction, arguments))
        });
        let Some((direction, arguments)) = parsed else {
            return self.reject_command(CommandOutcome::Failed {
                code: "invalid_arguments".to_owned(),
                message: "Pane direction must be right or down".to_owned(),
            });
        };
        let (argv, cwd) = match tab_create_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(outcome) => return self.reject_command(outcome),
        };
        let prepared = match self
            .workspace
            .begin_pane_create(scope, session, pane, direction, cwd, argv)
        {
            Ok(prepared) => prepared,
            Err(error) => return self.reject_command(session_request_outcome(error)),
        };
        self.submit_prepared_session_command(
            scope,
            prepared,
            &[],
            execution,
            CommandSelection::Preserve,
        )
    }

    fn dispatch_tab_create(
        &mut self,
        scope: SpaceId,
        session: &str,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (argv, cwd) = match tab_create_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(outcome) => return self.reject_command(outcome),
        };
        let prepared = match self.workspace.begin_tab_create(scope, session, cwd, argv) {
            Ok(prepared) => prepared,
            Err(error) => return self.reject_command(session_request_outcome(error)),
        };
        self.submit_prepared_session_command(
            scope,
            prepared,
            &[],
            execution,
            CommandSelection::Preserve,
        )
    }

    fn settle_captured_session(
        &mut self,
        exact_target: Option<ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(ExactMuxTarget::Session(scope, session)) = exact_target else {
            return self.reject_command(CommandOutcome::Denied {
                message: "Settle requires an issued Session target".to_owned(),
            });
        };
        let Some(identity) = self
            .workspace
            .session_identity(scope, &session)
            .filter(|identity| {
                self.workspace
                    .binding(scope)
                    .is_some_and(|binding| binding.sessions().get(identity).is_some())
            })
        else {
            return self.reject_command(CommandOutcome::Denied {
                message: "This session has no saved work to settle".to_owned(),
            });
        };
        self.saved_session_metadata(SessionAction::Settle, scope, &[identity], execution)
    }

    fn dispatch_session_create(
        &mut self,
        scope: SpaceId,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (name, cwd, argv, saved) = match session_create_arguments(arguments) {
            Ok(arguments) => arguments,
            Err(outcome) => return self.reject_command(outcome),
        };
        let prepared = match saved {
            Some((identity, title)) => self
                .workspace
                .begin_session_create_saved(scope, name, cwd, argv, identity, title),
            None => self.workspace.begin_session_create(scope, name, cwd, argv),
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.reject_command(session_request_outcome(error)),
        };
        self.submit_prepared_session_command(
            scope,
            prepared,
            &[],
            execution,
            CommandSelection::Preserve,
        )
    }

    fn submit_prepared_session_command(
        &mut self,
        scope: SpaceId,
        prepared: PreparedSessionRequest,
        closed_native_panes: &[(String, String, String)],
        execution: Option<(Instant, CommandCancellation)>,
        selection: CommandSelection,
    ) -> CommandDispatch {
        let (command, membership) = prepared;
        let native = self.native_targets_for_close(scope, &command);
        let Some(submitted) = executor::submit_authoritative_command_for_scope(
            &mut self.workspace,
            &self.repaint,
            scope,
            command,
            membership.map(Box::new),
            execution.clone(),
            selection,
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
                for (session, window, pane) in closed_native_panes {
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
            let dispatch =
                self.hold_until_session_starts(submitted.scope, &submitted.command, outcome);
            return Self::retire_native_after_close(dispatch, native, execution);
        }
        let dispatch = CommandDispatch::Pending(PendingCommandResult::Mux {
            scope: submitted.scope,
            command: Box::new(submitted.command),
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        });
        Self::retire_native_after_close(dispatch, native, execution)
    }

    /// Pending backend work disables saved-state menu actions without reading SQLite on a frame.
    pub(crate) fn saved_session_state_pending(&self, scope: SpaceId, identity: &str) -> bool {
        self.commands
            .pending
            .iter()
            .any(|pending| match &pending.result {
                PendingCommandResult::Mux {
                    scope: pending_scope,
                    membership: Some(membership),
                    ..
                }
                | PendingCommandResult::DitchCleanup {
                    scope: pending_scope,
                    membership: Some(membership),
                    ..
                } => *pending_scope == scope && membership.identity() == identity,
                _ => false,
            })
    }

    pub(crate) fn saved_session_invocation(
        &self,
        scope: SpaceId,
        command: &str,
        arguments: Vec<String>,
    ) -> Option<bootty_control::CommandInvocation> {
        let mux = self.workspace.binding(scope)?.mux();
        let handle = self.binding_target_handle(scope, mux.binding_generation());
        let mut invocation = bootty_control::CommandInvocation::new(
            command,
            arguments,
            bootty_control::Caller::Internal,
        );
        invocation.target =
            ExactMuxTarget::Binding(scope).command_target(ResourceKind::Binding, mux, &handle);
        invocation.target.as_ref()?;
        Some(invocation)
    }

    /// Capture the exact saved destination before input can complete on a worker.
    pub(crate) fn session_activity_invocation(
        &self,
        scope: SpaceId,
        identity: &str,
        at: i64,
    ) -> Option<bootty_control::CommandInvocation> {
        if self
            .workspace
            .binding(scope)?
            .sessions()
            .get(identity)
            .is_some_and(|saved| {
                saved.state.lifecycle != SessionLifecycle::Settled
                    && saved.state.last_activity_at.is_some_and(|last| last >= at)
            })
        {
            return None;
        }
        self.saved_session_invocation(
            scope,
            "session.input_accepted",
            vec![identity.to_owned(), at.to_string()],
        )
    }

    pub(crate) fn record_session_activity(&self, scope: SpaceId, identity: &str, at: i64) {
        if let Some(receipt) = self.session_activity_invocation(scope, identity, at) {
            let Some(target) = receipt.target.clone() else {
                return;
            };
            let key = (target, identity.to_owned(), at);
            let settled = self
                .workspace
                .binding(scope)
                .and_then(|binding| binding.sessions().get(identity))
                .is_some_and(|saved| saved.state.lifecycle == SessionLifecycle::Settled);
            if !settled && self.session_activity_receipt.borrow().as_ref() == Some(&key) {
                return;
            }
            if submit_activity_receipt(&self.commands.sender, receipt, at) {
                *self.session_activity_receipt.borrow_mut() = Some(key);
            }
        }
    }

    fn saved_session_metadata(
        &mut self,
        action: SessionAction,
        scope: SpaceId,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return self.reject_command(command_outcome_for_mux_error(error));
        }
        let outcome = if action == SessionAction::ListSaved {
            let Some(binding) = self.workspace.binding(scope) else {
                return self.reject_command(CommandOutcome::StaleTarget {
                    message: "the Space was closed".to_owned(),
                });
            };
            serialized_command_outcome(binding.sessions().sessions().iter().map(|saved| {
                serde_json::json!({ "identity": saved.identity, "title": saved.label(), "cwd": saved.cwd,
                    "state": saved.state,
                    "attachment": binding.session_attachment(&saved.identity).map(|session| &session.id) })
            }).collect::<Vec<_>>())
        } else if action == SessionAction::SetTitle {
            let [identity, title] = arguments else {
                return self.reject_command(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message: "session.set_title expects identity and title".to_owned(),
                });
            };
            match self.workspace.set_session_title(scope, identity, title) {
                Ok(changed) => serialized_command_outcome(
                    serde_json::json!({"identity": identity, "title": title.trim(), "changed": changed}),
                ),
                Err(error) => CommandOutcome::Failed {
                    code: "session_title_failed".to_owned(),
                    message: error.to_string(),
                },
            }
        } else {
            let (identity, change) = match session_state_arguments(
                action,
                arguments,
                crate::clock::ClockSnapshot::now().epoch,
            ) {
                Ok(request) => request,
                Err(outcome) => return self.reject_command(outcome),
            };
            match self.workspace.set_session_state(scope, identity, change) {
                Ok(changed) => {
                    let saved = self
                        .workspace
                        .binding(scope)
                        .and_then(|binding| binding.sessions().get(identity));
                    serialized_command_outcome(serde_json::json!({
                        "identity": identity, "changed": changed, "state": saved.map(|saved| saved.state)
                    }))
                }
                Err(error) => CommandOutcome::Failed {
                    code: "session_state_failed".to_owned(),
                    message: error.to_string(),
                },
            }
        };
        CommandDispatch::Complete(outcome)
    }

    /// Hold a topology mutation's success until startup and its exact session checkpoint commit.
    pub(super) fn hold_until_session_starts(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        outcome: CommandOutcome,
    ) -> CommandDispatch {
        let (MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. }
        | MuxCommand::RestoreSession { session_id, .. }
        | MuxCommand::NewWindow { session_id, .. }
        | MuxCommand::SplitPane { session_id, .. }
        | MuxCommand::CreatePane { session_id, .. }) = command
        else {
            return CommandDispatch::Complete(outcome);
        };
        if !matches!(outcome, CommandOutcome::Success { .. }) {
            return CommandDispatch::Complete(outcome);
        }
        let starting = match self.workspace.starting_session(scope, command) {
            Ok(Some(starting)) => starting,
            Ok(None) => return self.hold_until_session_checkpoint(scope, session_id, outcome),
            Err(error) => return CommandDispatch::Complete(command_outcome_for_mux_error(error)),
        };
        match self.poll_session_start(&starting, session_id, &outcome) {
            Poll::Ready(outcome) => {
                self.hold_until_session_checkpoint(scope, starting.session_id(), outcome)
            }
            Poll::Pending => CommandDispatch::Pending(PendingCommandResult::SessionStart {
                starting,
                name: session_id.clone(),
                outcome,
            }),
        }
    }

    pub(super) fn hold_until_session_checkpoint(
        &mut self,
        scope: SpaceId,
        session: &str,
        outcome: CommandOutcome,
    ) -> CommandDispatch {
        if !matches!(outcome, CommandOutcome::Success { .. }) {
            return CommandDispatch::Complete(outcome);
        }
        let Some(identity) = self.workspace.session_identity(scope, session) else {
            return CommandDispatch::Complete(session_creation_checkpoint_failed(Some(
                "The created session has no saved logical identity".to_owned(),
            )));
        };
        if let Some(dispatch) =
            self.hold_until_terminal_agent_association(scope, &identity, session, &outcome)
        {
            return dispatch;
        }
        let Some(target) = self.mux_resource_target(scope, ResourceKind::Session, session, None)
        else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The created session was replaced before its checkpoint".to_owned(),
            });
        };
        let captured = self.checkpoint_session(scope, &identity);
        CommandDispatch::Pending(PendingCommandResult::SessionCheckpointCompletion {
            target,
            identity,
            captured,
            outcome: Box::new(outcome),
        })
    }

    pub(super) fn poll_session_checkpoint_completion(
        &mut self,
        target: &bootty_control::CommandTarget,
        identity: &str,
        captured: &mut Option<crate::state::SessionCheckpointTicket>,
        outcome: &CommandOutcome,
    ) -> Poll<CommandOutcome> {
        let (scope, session) = match self.resolve_command_target(
            "session.close",
            Some(ResourceKind::Session),
            Some(target),
        ) {
            Ok((_, Some(ExactMuxTarget::Session(scope, session)))) => (scope, session),
            Ok(_) => {
                return Poll::Ready(CommandOutcome::StaleTarget {
                    message: "The created session is no longer available".to_owned(),
                });
            }
            Err(outcome) => return Poll::Ready(outcome),
        };
        if self.workspace.session_identity(scope, &session).as_deref() != Some(identity) {
            return Poll::Ready(CommandOutcome::StaleTarget {
                message: "The created session changed saved identity".to_owned(),
            });
        }
        self.poll_session_checkpoints();
        if captured.is_none() {
            *captured = self.checkpoint_session(scope, identity);
            return Poll::Pending;
        }
        match captured
            .as_ref()
            .and_then(crate::state::SessionCheckpointTicket::accepted)
        {
            None => Poll::Pending,
            Some(true) => Poll::Ready(outcome.clone()),
            Some(false) => Poll::Ready(session_creation_checkpoint_failed(
                captured
                    .as_ref()
                    .and_then(crate::state::SessionCheckpointTicket::failure),
            )),
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
            let (action, target) = if starting.created_session() {
                (
                    SessionAction::Close,
                    ExactMuxTarget::Session(starting.scope(), starting.session_id().to_owned()),
                )
            } else {
                (
                    SessionAction::ClosePane,
                    ExactMuxTarget::Pane(
                        starting.scope(),
                        starting.session_id().to_owned(),
                        starting.window_id().to_owned(),
                        starting.pane_id().to_owned(),
                    ),
                )
            };
            // Startup failure removes only its exact fresh topology. Capturing a partially
            // started restore would replace the last complete saved session with incomplete state.
            let closed =
                self.dispatch_session_command_after_checkpoint(action, &[], Some(target), None);
            match closed {
                CommandDispatch::Complete(CommandOutcome::Success { .. }) => {
                    "; the new terminal was closed".to_owned()
                }
                CommandDispatch::Complete(failed) => format!(
                    "; closing the new terminal failed: {}",
                    command_outcome_message(&failed).unwrap_or_default()
                ),
                CommandDispatch::Pending(_) => "; the new terminal is closing".to_owned(),
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
    pub(super) fn dispatch_planned_pane_close(
        &mut self,
        session: &str,
        pane: &str,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let binding = &self.workspace.active.binding;
        let window = binding
            .mux()
            .session_by_id_or_name(session)
            .and_then(|session| {
                session.windows.iter().find(|window| {
                    std::iter::once(&window.anchor)
                        .chain(&window.panes)
                        .any(|anchor| anchor.pane_id.as_deref() == Some(pane))
                })
            });
        let Some(window) = window else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "The pane to close is no longer open".to_owned(),
            });
        };
        self.dispatch_session_command(
            SessionAction::ClosePane,
            &[],
            Some(ExactMuxTarget::Pane(
                binding.scope(),
                session.to_owned(),
                window.id.clone(),
                pane.to_owned(),
            )),
            execution,
        )
    }

    fn is_last_session_pane(&self, scope: SpaceId, session: &str, pane: &str) -> bool {
        let Some(session) = self
            .workspace
            .binding(scope)
            .and_then(|binding| binding.mux().session_by_id_or_name(session))
        else {
            return false;
        };
        // A window lists its anchor pane besides its panes.
        let panes = session
            .windows
            .iter()
            .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes))
            .filter_map(|anchor| anchor.pane_id.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        panes.len() == 1 && panes.contains(pane)
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

    fn list_spaces_command(
        &self,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        CommandDispatch::Complete(
            executor::begin_synchronous_command(execution)
                .map_or_else(command_outcome_for_mux_error, |()| {
                    serialized_command_outcome(self.spaces_listing())
                }),
        )
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
                        serde_json::json!({
                            "name": session.name,
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

/// Called only after input acceptance. The worker keeps the captured destination, not selection.
pub(super) fn submit_activity_receipt(
    sender: &bootty_control::AppCommandSender,
    mut receipt: bootty_control::CommandInvocation,
    at: i64,
) -> bool {
    let Some(timestamp) = receipt.arguments.get_mut(1) else {
        return false;
    };
    *timestamp = at.to_string();
    let now = Instant::now();
    now.checked_add(std::time::Duration::from_secs(5))
        .is_some_and(|deadline| {
            sender
                .for_caller(bootty_control::Caller::Internal)
                .submit(receipt, deadline, CommandCancellation::new())
                .is_ok()
        })
}

/// `name`, `cwd`, literal argv, and an optional retained saved identity/purpose.
type SessionCreateArguments<'a> = (&'a str, &'a str, Vec<String>, Option<(&'a str, &'a str)>);

fn session_create_arguments(
    arguments: &[String],
) -> Result<SessionCreateArguments<'_>, CommandOutcome> {
    let invalid = |message: String| CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message,
    };
    let (name, cwd, argv, saved) = match arguments {
        [name, cwd] => (name.as_str(), cwd.as_str(), None, None),
        [name, cwd, argv] => (name.as_str(), cwd.as_str(), Some(argv), None),
        [name, cwd, argv, identity] => (
            name.as_str(),
            cwd.as_str(),
            Some(argv),
            Some((identity.as_str(), name.as_str())),
        ),
        [name, cwd, argv, identity, title] => (
            name.as_str(),
            cwd.as_str(),
            Some(argv),
            Some((identity.as_str(), title.as_str())),
        ),
        _ => {
            return Err(invalid(
                "session.create expects name, cwd, optional argv, identity and title".to_owned(),
            ));
        }
    };
    let argv = argv
        .map(|argv| serde_json::from_str::<Vec<String>>(argv))
        .transpose()
        .map_err(|error| invalid(format!("argv must be a JSON array of strings: {error}")))?
        .unwrap_or_default();
    Ok((name, cwd, argv, saved))
}

pub(super) fn tab_create_arguments(
    arguments: &[String],
) -> Result<(Vec<String>, Option<&str>), CommandOutcome> {
    let argv = arguments
        .first()
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
        .ok_or_else(|| CommandOutcome::Failed {
            code: "invalid_arguments".to_owned(),
            message: "argv must be a JSON array of strings".to_owned(),
        })?;
    let cwd = arguments
        .get(1)
        .filter(|cwd| !cwd.is_empty())
        .map(String::as_str);
    Ok((argv, cwd))
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

fn session_state_arguments(
    action: SessionAction,
    arguments: &[String],
    now: i64,
) -> Result<(&str, SessionStateChange), CommandOutcome> {
    let invalid = || CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: format!(
            "{} expects a saved identity{}",
            action.descriptor().id,
            if matches!(
                action,
                SessionAction::Snooze | SessionAction::Activity | SessionAction::AcceptedInput
            ) {
                " and non-negative absolute UTC seconds"
            } else {
                ""
            }
        ),
    };
    if matches!(
        action,
        SessionAction::Snooze | SessionAction::Activity | SessionAction::AcceptedInput
    ) {
        let [identity, until] = arguments else {
            return Err(invalid());
        };
        let until = until
            .parse::<i64>()
            .ok()
            .filter(|until| *until >= 0)
            .ok_or_else(invalid)?;
        let change = match action {
            SessionAction::Activity => SessionStateChange::RecordActivity { at: until, now },
            SessionAction::AcceptedInput => SessionStateChange::AcceptInput { at: until, now },
            _ => SessionStateChange::SnoozeUntil(until),
        };
        return Ok((identity, change));
    }
    let [identity] = arguments else {
        return Err(invalid());
    };
    let change = match action {
        SessionAction::Pin => SessionStateChange::SetPinned(true),
        SessionAction::Unpin => SessionStateChange::SetPinned(false),
        SessionAction::Settle => SessionStateChange::SetLifecycle(SessionLifecycle::Settled),
        SessionAction::Activate => SessionStateChange::SetLifecycle(SessionLifecycle::Active),
        SessionAction::Archive => SessionStateChange::Archive,
        SessionAction::Unarchive => SessionStateChange::RestoreArchive,
        SessionAction::Unsnooze => SessionStateChange::ClearSnooze,
        SessionAction::Hide => SessionStateChange::SetHidden(true),
        SessionAction::Show => SessionStateChange::SetHidden(false),
        SessionAction::Delete => SessionStateChange::Delete,
        SessionAction::Restore => SessionStateChange::RestoreDeleted,
        _ => return Err(invalid()),
    };
    Ok((identity, change))
}

fn session_creation_checkpoint_failed(reason: Option<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "session_checkpoint_failed".to_owned(),
        message: reason.map_or_else(
            || "The terminal was created, but its current session state could not be saved; it remains open".to_owned(),
            |reason| format!("The terminal was created, but its current session state could not be saved; it remains open: {reason}"),
        ),
    }
}
