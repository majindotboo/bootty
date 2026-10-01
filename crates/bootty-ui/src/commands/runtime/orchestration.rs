use super::{CommandDispatch, PendingCommandResult};
use crate::state::AppState;
use bootty_agents::AgentInvocation;
use bootty_control::{CommandCancellation, CommandInvocation, CommandOutcome, ResourceKind};
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
        if let Some(target) = invocation.target.as_ref()
            && target.kind == ResourceKind::Terminal
        {
            let exact = match self.resolve_command_target(
                &invocation.command,
                Some(ResourceKind::Terminal),
                Some(target),
            ) {
                Ok((_, exact)) => exact,
                Err(outcome) => return CommandDispatch::Complete(outcome),
            };
            if invocation.command == "orchestration.worker.attach" {
                // Reports invoke this owner's executable. A remote worker needs an explicit
                // remote control transport before it can participate in this local run.
                if exact
                    .and_then(|exact| self.workspace.binding(exact.scope()))
                    .is_some_and(|binding| binding.multiplexer().remote.is_some())
                {
                    return CommandDispatch::Complete(CommandOutcome::Unsupported {
                        message:
                            "Coordination workers currently require an agent terminal on this host"
                                .to_owned(),
                    });
                }
                let registered = self
                    .terminal_agent_service()
                    .and_then(|agents| agents.record(target));
                if !registered.is_some_and(|record| {
                    invocation
                        .arguments
                        .get(2)
                        .is_some_and(|provider| *provider == record.provider.to_string())
                }) {
                    return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                        message: "Choose a registered terminal for this agent provider".to_owned(),
                    });
                }
            }
        }
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
