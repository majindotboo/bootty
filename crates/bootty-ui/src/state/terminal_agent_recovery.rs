use super::AppState;
use bootty_control::{Caller, CommandCancellation, CommandInvocation, ResourceKind};
use std::time::{Duration, Instant};

impl AppState {
    pub(crate) fn revoke_current_terminal_agent_recovery(&mut self) {
        if let Some(pane) = self.focused_pane() {
            self.workspace
                .active
                .binding
                .revoke_restored_agent_terminal(&pane);
        }
    }

    /// Only a cold-created shell receives a retained provider-native resume, once.
    pub(super) fn restore_terminal_agents(&mut self, now: Instant) {
        for response in std::mem::take(&mut self.terminal_agent_recoveries) {
            match response.try_recv() {
                Ok(outcome) => {
                    if let Some(error) = crate::commands::command_outcome_message(&outcome) {
                        self.record_error(format!("Terminal agent recovery failed: {error}"));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.terminal_agent_recoveries.push(response);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.record_error("Terminal agent recovery worker stopped");
                }
            }
        }
        let Some(service) = self.terminal_agent_service() else {
            return;
        };
        let Some(deadline) = now.checked_add(Duration::from_secs(30)) else {
            return;
        };
        for record in service.records() {
            if record.recovery_launch().is_err()
                || !self
                    .config()
                    .agents
                    .provider(&record.provider.to_string())
                    .is_some_and(|provider| provider.enabled)
            {
                continue;
            }
            let Some(location) = &record.location else {
                continue;
            };
            let destination = self.workspace.all_bindings().find_map(|binding| {
                // Resume data belongs to its owning local host; remote credentials never migrate.
                if binding.scope().persistence_value().to_string() != record.binding_id
                    || binding.multiplexer().remote.is_some()
                {
                    return None;
                }
                let original = binding.cold_agent_source_path(
                    &location.task_identity,
                    &location.window_id,
                    &location.pane_id,
                )?;
                let exact = binding.cold_restored_agent_terminal(&original, false)?;
                let handle =
                    self.binding_target_handle(binding.scope(), binding.mux().binding_generation());
                let target =
                    exact.command_target(ResourceKind::Terminal, binding.mux(), &handle)?;
                Some((exact, target, original))
            });
            let Some((exact, target, original)) = destination else {
                continue;
            };
            if !self
                .workspace
                .binding_mut(exact.scope())
                .is_some_and(|binding| {
                    binding.admit_restored_agent_terminal(&original, &exact, false)
                })
            {
                continue;
            }
            let mut invocation = CommandInvocation::new(
                format!("agents.{}.restore", record.provider),
                vec![record.target.handle],
                Caller::Internal,
            );
            invocation.target = Some(target);
            match self.app_command_sender(Caller::Internal).submit(
                invocation,
                deadline,
                CommandCancellation::new(),
            ) {
                Ok(response) => self.terminal_agent_recoveries.push(response),
                Err(_) => self.record_error("Terminal agent recovery command could not be queued"),
            }
        }
    }
}
