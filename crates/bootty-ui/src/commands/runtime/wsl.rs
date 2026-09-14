use std::{sync::mpsc, time::Instant};

use bootty_config::config::{MultiplexerBackendConfig, WslDistribution, WslRemoteConfig};
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_host::{CancellableCommandRunner, wsl::distributions};
use bootty_mux::{
    executor,
    repository::{SpaceMuxOverride, SpaceRemoteOverride},
};

use super::{CommandDispatch, PendingCommandResult};
use crate::AppState;

fn unsupported() -> CommandOutcome {
    CommandOutcome::Unsupported {
        message: "WSL workspaces require Windows with WSL installed".to_owned(),
    }
}
fn failed(message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "wsl_failed".to_owned(),
        message: message.into(),
    }
}

impl AppState {
    pub(super) fn dispatch_wsl_list(
        &self,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if !cfg!(windows) {
            return CommandDispatch::Complete(unsupported());
        }
        let (deadline, cancellation) = executor::command_execution(execution);
        let (sender, receiver) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
                bootty_host::CommandCancellation::default(),
                deadline,
                move || cancellation.is_cancelled(),
            );
            let outcome = match distributions(&runner) {
                Ok(distributions) => CommandOutcome::Success {
                    value: serde_json::json!({"distributions": distributions}),
                    warnings: Vec::new(),
                },
                Err(error) => failed(error.to_string()),
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }

    pub(super) fn create_wsl_space(&mut self, arguments: &[String]) -> CommandOutcome {
        if !cfg!(windows) {
            return unsupported();
        }
        let Some(distribution) = arguments.first() else {
            return failed("A WSL distribution is required".to_owned());
        };
        let distribution = match WslDistribution::new(distribution.clone()) {
            Ok(distribution) => distribution,
            Err(error) => return failed(error),
        };
        let name = arguments
            .get(1)
            .map_or(distribution.as_str(), String::as_str);
        let backend = match arguments.get(2).map_or("rmux", String::as_str) {
            "rmux" => MultiplexerBackendConfig::Rmux,
            "tmux" => MultiplexerBackendConfig::Tmux,
            _ => {
                return CommandOutcome::Unsupported {
                    message: "WSL supports rmux and tmux backends".to_owned(),
                };
            }
        };
        let config = self.config().clone();
        let mux = SpaceMuxOverride {
            backend: Some(backend),
            remote: SpaceRemoteOverride::Inline(
                WslRemoteConfig {
                    distribution: distribution.clone(),
                }
                .into(),
            ),
        };
        match self.workspace.create_space(
            name,
            "terminal",
            bootty_mux::repository::DEFAULT_SPACE_COLOR,
            false,
            mux,
            &config,
            self.active_appearance_variant(),
        ) {
            Ok(Some(space_id)) => {
                let activated = self.activate_space_from_ui(space_id);
                CommandOutcome::Success {
                    value: serde_json::json!({"space_id": space_id.persistence_value(), "distribution": distribution, "activated": activated}),
                    warnings: if activated {
                        vec![]
                    } else {
                        vec![bootty_control::CommandWarning {
                            code: "activation_failed".to_owned(),
                            message: "Space was saved but could not be activated".to_owned(),
                        }]
                    },
                }
            }
            Ok(None) => failed("Space name must be nonempty"),
            Err(error) => failed(error.to_string()),
        }
    }
}
