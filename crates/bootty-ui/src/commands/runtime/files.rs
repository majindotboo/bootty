use super::CommandDispatch;
use crate::{AppState, commands::FileAction};
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_host::{SystemCommandRunner, remote::RemoteHost};
use bootty_mux::{controller::SpaceId, executor};
use std::time::Instant;

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
        if matches!(action, FileAction::Reveal | FileAction::OpenIn) {
            return self.hand_file_to_desktop(scope, action, arguments, effects, execution);
        }
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

    /// Shows a file in this computer's file manager, or opens it in an installed editor. A
    /// remote host's file has no place in either.
    fn hand_file_to_desktop(
        &mut self,
        scope: SpaceId,
        action: FileAction,
        arguments: &[String],
        effects: &mut Vec<crate::state::AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return self.reject_command(super::command_outcome_for_mux_error(error));
        }
        let Some(binding) = self.workspace.binding(scope) else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "Binding is no longer available".to_owned(),
            });
        };
        if binding.multiplexer().remote.is_some() {
            return self.reject_command(CommandOutcome::Unsupported {
                message: if action == FileAction::Reveal {
                    "A remote host's files can't be shown in this computer's file manager"
                } else {
                    "A remote host's files can't be opened in this computer's editors"
                }
                .to_owned(),
            });
        }
        let Some(path) = arguments
            .first()
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
        else {
            return self.reject_command(CommandOutcome::Failed {
                code: "invalid_arguments".to_owned(),
                message: "An absolute path is required".to_owned(),
            });
        };
        if !path.exists() {
            return self.reject_command(CommandOutcome::Failed {
                code: "file_failed".to_owned(),
                message: format!("{} no longer exists", path.display()),
            });
        }
        if action == FileAction::Reveal {
            effects.push(crate::state::AppEffect::RevealPath(path));
            return CommandDispatch::Complete(CommandOutcome::success());
        }
        let requested = arguments.get(1).map_or("", String::as_str);
        let Some(editor) = crate::editors::find(requested) else {
            return self.reject_command(CommandOutcome::Unavailable {
                message: format!("No editor called “{requested}” is installed"),
            });
        };
        effects.push(crate::state::AppEffect::OpenInEditor {
            editor: editor.id,
            path,
        });
        CommandDispatch::Complete(CommandOutcome::success())
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
        self.dispatch_committed_command(execution, move || {
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
        })
    }
}
