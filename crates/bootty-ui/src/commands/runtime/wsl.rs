use std::{sync::mpsc, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_host::{CancellableCommandRunner, wsl::distributions};
use bootty_mux::executor;

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
}
