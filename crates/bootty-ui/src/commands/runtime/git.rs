use std::{sync::mpsc, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_host::{
    SystemCommandRunner,
    remote::{RemoteCommandRunner, RemoteHost},
};
use bootty_mux::{controller::SpaceId, executor};

use super::{CommandDispatch, PendingCommandResult};
use crate::{AppState, commands::GitAction};

impl AppState {
    pub(super) fn dispatch_git_action(
        &mut self,
        scope: SpaceId,
        action: GitAction,
        arguments: Vec<String>,
        target: Option<bootty_control::CommandTarget>,
        effects: &mut Vec<crate::state::AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if action == GitAction::Open {
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
            effects.push(crate::state::AppEffect::OpenGitChanges {
                scope,
                target,
                directory: path.clone(),
                host,
            });
            CommandDispatch::Complete(CommandOutcome::success())
        } else {
            self.dispatch_git_command(scope, action, arguments, execution)
        }
    }

    pub(crate) fn open_session_git_changes(&mut self, scope: SpaceId, session_id: &str) {
        let Some(binding) = self.workspace.binding(scope) else {
            return;
        };
        let Some(directory) = binding
            .mux()
            .all_sessions()
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.anchor.cwd.clone())
        else {
            self.record_error("Session has not reported a Git directory");
            return;
        };
        let generation = binding.mux().binding_generation();
        let mut invocation = bootty_control::CommandInvocation::from_action(
            "git.open",
            bootty_control::Caller::Internal,
        );
        invocation.arguments = vec![directory];
        invocation.target = Some(bootty_control::CommandTarget {
            kind: bootty_control::ResourceKind::Binding,
            handle: self.binding_target_handle(scope, generation),
            generation,
        });
        self.commands.queue(invocation);
    }

    pub(super) fn dispatch_git_command(
        &self,
        scope: SpaceId,
        action: GitAction,
        arguments: Vec<String>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(binding) = self.workspace.binding(scope) else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Git host binding was closed".to_owned(),
            });
        };
        let remote = binding.multiplexer().remote.clone();
        let (deadline, cancellation) = executor::command_execution(execution);
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            // Once Git starts, wait for its result: killing commit hooks can leave an ambiguous commit.
            let outcome = if let Err(error) =
                executor::begin_synchronous_command(Some((deadline, cancellation)))
            {
                super::command_outcome_for_mux_error(error)
            } else {
                let result = remote.map_or_else(
                    || action.execute(SystemCommandRunner, &arguments),
                    |remote| {
                        if action == GitAction::CreateWorktree {
                            bootty_mux::remote_space::create_remote_worktree_request_with_runner(
                                &remote,
                                arguments.first().ok_or("Repository path is required")?,
                                &GitAction::worktree_request(&arguments)?,
                                &SystemCommandRunner,
                            )
                            .map(serde_json::Value::String)
                            .map_err(|error| error.to_string())
                        } else {
                            action.execute(
                                RemoteCommandRunner::new(
                                    RemoteHost::new(remote),
                                    SystemCommandRunner,
                                ),
                                &arguments,
                            )
                        }
                    },
                );
                match result {
                    Ok(value) => CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    },
                    Err(message) => CommandOutcome::Failed {
                        code: "git_failed".to_owned(),
                        message,
                    },
                }
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(result))
    }
}
