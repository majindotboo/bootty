//! Authoritative mux commands and membership persistence.

use super::{
    CommandDispatch, PendingCommandResult, command_outcome_for_mux_error, command_outcome_message,
    serialized_command_outcome,
};
use crate::{commands::ExactMuxTarget, error_catalog::ErrorNotice, state::AppState};
use bootty_control::{CommandCancellation, CommandOutcome, CommandTarget, ResourceKind};
use bootty_mux::{
    command::MuxCommand,
    controller::{
        CommandSelection, MuxCommandCompletion, MuxCommandError, MuxCommandResult, SpaceId,
    },
    executor,
    repository::BindingMembershipMutation,
};
use std::{collections::BTreeMap, time::Instant};

impl AppState {
    pub(super) fn dispatch_pane_command(
        &mut self,
        action: crate::commands::PaneAction,
        arguments: &[String],
        exact_target: Option<ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(ExactMuxTarget::Session(scope, session)) = exact_target else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "Pane arrangement requires a live session target".to_owned(),
            });
        };
        let command = match action.command(session, arguments) {
            Ok(command) => command,
            Err(message) => {
                return self.reject_command(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message,
                });
            }
        };
        let Some(submitted) = executor::submit_authoritative_command_for_scope(
            &mut self.workspace,
            &self.repaint,
            scope,
            command,
            None,
            execution,
            CommandSelection::Follow,
        ) else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "Pane binding was closed".to_owned(),
            });
        };
        CommandDispatch::Pending(PendingCommandResult::Mux {
            scope: submitted.scope,
            command: Box::new(submitted.command),
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        })
    }

    pub(super) fn preflight_mux_command(&self, command: &MuxCommand) -> Option<CommandOutcome> {
        match executor::preflight_command(&self.workspace, command) {
            Err(MuxCommandError::Failed(message)) => Some(CommandOutcome::Unavailable { message }),
            Err(MuxCommandError::Unsupported) => Some(CommandOutcome::Unsupported {
                message: ErrorNotice::MuxOperationUnsupported.to_string(),
            }),
            Err(MuxCommandError::Unavailable) => Some(CommandOutcome::Unavailable {
                message: ErrorNotice::MuxOperationUnavailable.to_string(),
            }),
            Err(MuxCommandError::Stale) => Some(CommandOutcome::StaleTarget {
                message: ErrorNotice::MuxOperationCapabilityStale.to_string(),
            }),
            Ok(()) | Err(MuxCommandError::Cancelled | MuxCommandError::DeadlineExceeded) => None,
        }
    }

    pub(super) fn submit_authoritative_mux_command(
        &mut self,
        command: MuxCommand,
        membership: Option<Box<BindingMembershipMutation>>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> PendingCommandResult {
        let submitted = executor::submit_authoritative_command(
            &mut self.workspace,
            &self.repaint,
            command,
            membership,
            execution,
        );
        PendingCommandResult::Mux {
            scope: submitted.scope,
            command: Box::new(submitted.command),
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        }
    }

    pub(super) fn begin_authoritative_membership(
        &mut self,
        command: &MuxCommand,
    ) -> Result<Option<Box<BindingMembershipMutation>>, CommandOutcome> {
        executor::begin_authoritative_membership(&mut self.workspace, command).map_err(|error| {
            let outcome = CommandOutcome::Failed {
                code: "persistence_failed".to_owned(),
                message: error.to_string(),
            };
            if let Some(message) = command_outcome_message(&outcome) {
                self.record_command_failure(message);
            }
            outcome
        })
    }

    /// Whether `command` is already in flight against the binding at `scope`.
    pub(super) fn mux_command_pending(&self, scope: SpaceId, command: &MuxCommand) -> bool {
        self.commands
            .pending
            .iter()
            .any(|pending| self.pending_mux_command_matches(&pending.result, scope, command))
    }

    fn pending_mux_command_matches(
        &self,
        pending: &PendingCommandResult,
        scope: SpaceId,
        command: &MuxCommand,
    ) -> bool {
        match pending {
            PendingCommandResult::Mux {
                scope: pending_scope,
                command: pending_command,
                ..
            } => *pending_scope == scope && pending_command.as_ref() == command,
            PendingCommandResult::SessionCheckpointClose { target, .. } => {
                let MuxCommand::DitchSession { session_id } = command else {
                    return false;
                };
                self.mux_resource_target(scope, ResourceKind::Session, session_id, None)
                    .as_ref()
                    == Some(target)
            }
            PendingCommandResult::NativePanelFocus {
                command: pending, ..
            }
            | PendingCommandResult::SessionDelete {
                command: pending, ..
            }
            | PendingCommandResult::NativePanelClose {
                command: pending, ..
            }
            | PendingCommandResult::SurfaceCreation {
                command: pending, ..
            }
            | PendingCommandResult::SurfaceRestore {
                command: pending, ..
            } => self.pending_mux_command_matches(&pending.result, scope, command),
            // These states have no direct mux command to match.
            PendingCommandResult::SessionStart { .. }
            | PendingCommandResult::SessionReopen { .. }
            | PendingCommandResult::SessionAgentAssociation { .. }
            | PendingCommandResult::SessionCheckpointCompletion { .. }
            | PendingCommandResult::TerminalAgentRestore { .. }
            | PendingCommandResult::TerminalAgent { .. }
            | PendingCommandResult::Outcome(_)
            | PendingCommandResult::WorktreeCleanup { .. }
            | PendingCommandResult::NativeResume { .. }
            | PendingCommandResult::Forward { .. }
            | PendingCommandResult::Clipboard { .. }
            | PendingCommandResult::Link { .. } => false,
        }
    }

    pub(super) fn command_outcome_for_mux_result(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        membership: Option<&BindingMembershipMutation>,
        result: MuxCommandResult,
        layout: Option<&bootty_mux::workspace::PreparedPaneArrangement>,
    ) -> CommandOutcome {
        let completion = match executor::complete_authoritative_command(
            &mut self.workspace,
            scope,
            command,
            membership,
            result,
            layout,
        ) {
            Ok(completion) => completion,
            Err(error) => {
                let message = error.to_string();
                self.record_command_failure(message.clone());
                return CommandOutcome::Failed {
                    code: "persistence_failed".to_owned(),
                    message,
                };
            }
        };
        let (completion, sync_error) = completion;
        if let Some(error) = sync_error {
            self.record_error("Could not update the panes", error);
        }
        match completion {
            Ok(completion) => {
                let Some(value) = self.mux_command_completion_value(scope, command, &completion)
                else {
                    let outcome = CommandOutcome::StaleTarget {
                        message: ErrorNotice::MuxOperationCapabilityStale.to_string(),
                    };
                    if let Some(message) = command_outcome_message(&outcome) {
                        self.record_command_failure(message);
                    }
                    return outcome;
                };
                if scope == self.mux_scope()
                    && matches!(
                        command,
                        MuxCommand::ActivateWindow { .. }
                            | MuxCommand::ActivateWindowIndex { .. }
                            | MuxCommand::ActivateLastWindow { .. }
                            | MuxCommand::ActivatePane { .. }
                            | MuxCommand::RestoreSession { .. }
                    )
                {
                    self.dismiss_session_creation();
                }
                if matches!(
                    command,
                    MuxCommand::ActivateWindow { .. }
                        | MuxCommand::ActivatePane { .. }
                        | MuxCommand::RestoreSession { .. }
                ) && let Some(target) = value.get("terminal")
                {
                    match self.resolve_command_target(
                        "terminal.capture",
                        Some(ResourceKind::Terminal),
                        Some(target),
                    ) {
                        Ok((_, Some(exact))) => {
                            if let Err(outcome) = self.activate_terminal_target(&exact) {
                                return outcome;
                            }
                        }
                        Ok(_) => {
                            return CommandOutcome::StaleTarget {
                                message: "The reopened session has no admitted terminal target"
                                    .to_owned(),
                            };
                        }
                        Err(outcome) => return outcome,
                    }
                }
                serialized_command_outcome(value)
            }
            Err(error) => {
                let outcome = command_outcome_for_mux_error(error);
                if let Some(message) = command_outcome_message(&outcome) {
                    self.record_command_failure(message);
                }
                outcome
            }
        }
    }

    fn mux_command_completion_value(
        &self,
        scope: SpaceId,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
    ) -> Option<BTreeMap<String, CommandTarget>> {
        let mut value = BTreeMap::new();
        if let MuxCommand::CreatePane {
            session_id,
            pane_id,
            ..
        } = command
        {
            let target = self.created_pane_target(scope, session_id, pane_id.as_deref())?;
            value.insert("created".to_owned(), target);
        }
        if let MuxCommand::ActivateWindow {
            session_id,
            window_id,
        }
        | MuxCommand::ActivatePane {
            session_id,
            window_id,
            ..
        } = command
        {
            value.insert(
                "terminal".to_owned(),
                if let MuxCommand::ActivatePane { pane_id, .. } = command {
                    let binding = self.workspace.binding(scope)?;
                    let handle =
                        self.binding_target_handle(scope, binding.mux().binding_generation());
                    ExactMuxTarget::Pane(
                        scope,
                        session_id.clone(),
                        window_id.clone(),
                        pane_id.clone(),
                    )
                    .command_target(
                        ResourceKind::Terminal,
                        binding.mux(),
                        &handle,
                    )?
                } else {
                    self.mux_terminal_target(scope, session_id, window_id)?
                },
            );
        }
        // Background tab creation preserves selection, so its result carries no focused window.
        // The backend still marks the window it just created active within the target session.
        if let MuxCommand::NewWindow { session_id, .. } = command {
            let binding = self.workspace.binding(scope)?;
            let window = binding
                .mux()
                .backend_session_by_id_or_name(session_id)?
                .active_window_id
                .as_deref()?;
            value.insert(
                "created".to_owned(),
                self.mux_terminal_target(scope, session_id, window)?,
            );
        }
        if let Some(session_id) = match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. }
            | MuxCommand::RestoreSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        } {
            value.insert(
                "created".to_owned(),
                self.mux_resource_target(scope, ResourceKind::Session, session_id, None)?,
            );
            if let Some(terminal) = self.session_terminal_target(scope, session_id) {
                value.insert("terminal".to_owned(), terminal);
            }
        }
        if let (Some(session_id), Some(window_id)) = (
            completion.selected_session.as_deref(),
            completion.selected_window.as_deref(),
        ) {
            value.insert(
                "focused".to_owned(),
                self.mux_resource_target(
                    scope,
                    ResourceKind::MuxWindow,
                    session_id,
                    Some(window_id),
                )?,
            );
        }
        if !value.contains_key("focused")
            && let Some(session_id) = completion.selected_session.as_deref()
        {
            value.insert(
                "focused".to_owned(),
                self.mux_resource_target(scope, ResourceKind::Session, session_id, None)?,
            );
        }
        Some(value)
    }

    fn created_pane_target(
        &self,
        scope: SpaceId,
        session_id: &str,
        parent: Option<&str>,
    ) -> Option<CommandTarget> {
        let runtime = self.workspace.binding(scope)?;
        let session = runtime.mux().backend_session_by_id_or_name(session_id)?;
        let window = session.windows.iter().find(|window| {
            parent.map_or_else(
                || Some(window.id.as_str()) == session.active_window_id.as_deref(),
                |parent| {
                    std::iter::once(&window.anchor)
                        .chain(&window.panes)
                        .any(|anchor| anchor.pane_id.as_deref() == Some(parent))
                },
            )
        })?;
        if window.anchor.pane_id.as_deref() == parent {
            return None;
        }
        let handle = self.binding_target_handle(scope, runtime.mux().binding_generation());
        crate::commands::ExactMuxTarget::Pane(
            scope,
            session.id.clone(),
            window.id.clone(),
            window.anchor.pane_id.clone()?,
        )
        .command_target(ResourceKind::Terminal, runtime.mux(), &handle)
    }
}
