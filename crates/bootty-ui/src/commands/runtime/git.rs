use std::time::Instant;

use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_host::{
    SystemCommandRunner,
    remote::{RemoteCommandRunner, RemoteHost},
};
use bootty_mux::{controller::SpaceId, executor};

use super::CommandDispatch;
use crate::{AppState, commands::GitAction};

impl AppState {
    pub(super) fn dispatch_git_action(
        &self,
        scope: SpaceId,
        action: GitAction,
        arguments: Vec<String>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        self.dispatch_git_command(scope, action, arguments, execution)
    }

    pub(super) fn dispatch_worktree_cleanup(
        &mut self,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return self.reject_command(super::command_outcome_for_mux_error(error));
        }
        match self.start_worktree_cleanup() {
            Ok((result, warnings)) => {
                CommandDispatch::Pending(super::PendingCommandResult::WorktreeCleanup {
                    result,
                    warnings,
                })
            }
            Err(outcome) => self.reject_command(outcome),
        }
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
        let worktree_directory = self
            .config()
            .session
            .worktree_directory
            .clone()
            .filter(|directory| !directory.trim().is_empty());
        self.dispatch_committed_command(execution, move || {
            let result = if action == GitAction::CreateWorktree {
                create_worktree(remote.clone(), &arguments, worktree_directory)
            } else {
                remote.clone().map_or_else(
                    || action.execute(SystemCommandRunner, &arguments),
                    |remote| {
                        action.execute(
                            RemoteCommandRunner::new(RemoteHost::new(remote), SystemCommandRunner),
                            &arguments,
                        )
                    },
                )
            };
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
        })
    }
}

/// Create the checkout on the repository's host, which resolves the configured directory.
fn create_worktree(
    remote: Option<bootty_config::config::RemoteConfig>,
    arguments: &[String],
    directory: Option<String>,
) -> Result<serde_json::Value, String> {
    let repository = arguments.first().ok_or("Repository path is required")?;
    let request = GitAction::worktree_request(arguments, directory)?;
    remote
        .map_or_else(
            || bootty_git::Git::new().create_worktree(repository, &request),
            |remote| {
                bootty_git::remote::create_worktree(
                    &remote,
                    repository,
                    &request,
                    &SystemCommandRunner,
                )
            },
        )
        .map(serde_json::Value::String)
}
