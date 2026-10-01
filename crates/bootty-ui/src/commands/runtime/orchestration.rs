use super::{CommandDispatch, PendingCommandResult};
use crate::state::AppState;
use bootty_agents::AgentInvocation;
use bootty_control::{CommandCancellation, CommandInvocation, CommandOutcome};
use std::{sync::mpsc, time::Instant};

impl AppState {
    pub(super) fn dispatch_orchestration(
        &self,
        invocation: CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(service) = self.commands.orchestration.clone() else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "Orchestration storage is unavailable".to_owned(),
            });
        };
        let (deadline, cancellation) = bootty_mux::executor::command_execution(execution);
        let supplied = invocation.target.is_some();
        let request = AgentInvocation::new(
            invocation,
            supplied,
            Some(self.mux_scope().persistence_value().to_string()),
            deadline,
            cancellation,
        );
        let (sender, receiver) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let _ = sender.send(service.invoke(&request));
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
}
