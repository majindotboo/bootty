use super::{CommandDispatch, PendingCommandResult};
use crate::{AppState, commands::FileAction};
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_host::{SystemCommandRunner, remote::RemoteHost};
use bootty_mux::{controller::SpaceId, executor};
use std::{sync::mpsc, time::Instant};

impl AppState {
    pub(super) fn dispatch_file_action(
        &mut self,
        scope: SpaceId,
        action: FileAction,
        arguments: &[String],
        target: Option<bootty_control::CommandTarget>,
        effects: &mut Vec<crate::state::AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if matches!(action, FileAction::Open | FileAction::Browse) {
            if let Err(error) = executor::begin_synchronous_command(execution) {
                return self.reject_command(super::command_outcome_for_mux_error(error));
            }
            let (Some(binding), Some(target)) = (self.workspace.binding(scope), target) else {
                return self.reject_command(CommandOutcome::StaleTarget {
                    message: "Binding is no longer available".to_owned(),
                });
            };
            let Some(path) = arguments.first() else {
                return self.reject_command(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message: "A path is required".to_owned(),
                });
            };
            let host = binding
                .multiplexer()
                .remote
                .as_ref()
                .map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label);
            effects.push(crate::state::AppEffect::OpenFiles(
                crate::state::OpenFilesRequest {
                    scope,
                    target,
                    host,
                    path: path.clone(),
                    document: action == FileAction::Open,
                    line: arguments
                        .get(1)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1),
                    column: arguments
                        .get(2)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1),
                },
            ));
            CommandDispatch::Complete(CommandOutcome::success())
        } else {
            self.dispatch_file_command(scope, action, arguments, execution)
        }
    }

    pub(super) fn dispatch_file_command(
        &self,
        scope: SpaceId,
        action: FileAction,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(binding) = self.workspace.binding(scope) else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "File host binding was closed".to_owned(),
            });
        };
        let remote = binding.multiplexer().remote.clone();
        let request = match action.request(arguments) {
            Ok(request) => request,
            Err(message) => {
                return CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message,
                });
            }
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            // A started atomic save must report its observed result, never a fictitious rollback.
            let outcome = if let Err(error) =
                executor::begin_synchronous_command(Some((deadline, cancellation)))
            {
                super::command_outcome_for_mux_error(error)
            } else {
                let result = remote.map_or_else(
                    || request.execute(),
                    |remote| request.execute_remote(&RemoteHost::new(remote), SystemCommandRunner),
                );
                match result.and_then(|response| Ok(serde_json::to_value(response)?)) {
                    Ok(value) => CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    },
                    Err(error) => CommandOutcome::Failed {
                        code: "file_failed".to_owned(),
                        message: format!("{error:#}"),
                    },
                }
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(result))
    }
}
