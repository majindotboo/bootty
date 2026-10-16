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
            self.record_message("Session has not reported a Git directory");
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
        let writer = if matches!(
            action,
            GitAction::WriteMessage | GitAction::WritePullRequest
        ) {
            match self.quick_writer() {
                Ok(writer) => Some(writer),
                Err(outcome) => return CommandDispatch::Complete(outcome),
            }
        } else {
            None
        };
        let (deadline, cancellation) = executor::command_execution(execution.clone());
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
            let result = match (writer, result) {
                (Some(writer), Ok(context)) => writer.write(
                    action,
                    remote,
                    arguments.first().map_or("", String::as_str),
                    context.as_str().unwrap_or_default(),
                    &|| cancellation.is_cancelled() || Instant::now() >= deadline,
                ),
                (_, result) => result,
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

/// The configured Codex account and quick model, captured to write commit messages and pull
/// request text.
struct QuickWriter {
    preferences: bootty_config::config::AgentProviderConfig,
    model: String,
}

impl AppState {
    fn quick_writer(&self) -> Result<QuickWriter, CommandOutcome> {
        let agents = &self.config().agents;
        let preferences = agents
            .provider("codex")
            .filter(|preferences| preferences.enabled)
            .cloned()
            .ok_or_else(|| CommandOutcome::Unavailable {
                message: "Enable Codex to write with its quick model".into(),
            })?;
        Ok(QuickWriter {
            preferences,
            model: agents.quick_model.clone(),
        })
    }
}

impl QuickWriter {
    /// Asks Codex, on the repository's host, to write what `action` wants from Git's `context`.
    fn write(
        &self,
        action: GitAction,
        remote: Option<bootty_config::config::RemoteConfig>,
        repository: &str,
        context: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<serde_json::Value, String> {
        let launch = super::terminal_agents::prepare_launch(
            &bootty_control::CommandInvocation::new(
                "agents.codex.start",
                vec![repository.to_owned()],
                bootty_control::Caller::Internal,
            ),
            bootty_agents::AgentKind::Codex,
            "start",
            None,
            &self.preferences,
        )?;
        let config = match remote {
            Some(remote) => bootty_agents::NativeSessionConfig::capture_remote(
                bootty_agents::AgentKind::Codex,
                launch,
                remote,
            ),
            None => bootty_agents::NativeSessionConfig::from_launch(
                bootty_agents::AgentKind::Codex,
                launch,
            ),
        }?;
        if action == GitAction::WriteMessage {
            let message =
                bootty_agents::generate_commit_message(&config, &self.model, context, cancelled)?;
            Ok(serde_json::json!({"message": message.message()}))
        } else {
            serde_json::to_value(bootty_agents::generate_pull_request(
                &config,
                &self.model,
                context,
                cancelled,
            )?)
            .map_err(|error| error.to_string())
        }
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
                bootty_mux::remote_space::create_remote_worktree_request_with_runner(
                    &remote,
                    repository,
                    &request,
                    &SystemCommandRunner,
                )
                .map_err(|error| error.to_string())
            },
        )
        .map(serde_json::Value::String)
}
