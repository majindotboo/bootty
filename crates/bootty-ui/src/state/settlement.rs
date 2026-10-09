//! The window supplies time and observed activity; saved session state owns eligibility.
use super::{AppEffect, AppState};
use bootty_agents::{NativeSessionStatus, TerminalAgentStatus};
use bootty_control::{Caller, CommandInvocation, ResourceKind};
use std::time::{Duration, Instant};

impl AppState {
    pub(super) fn poll_session_settlement(&mut self, now: Instant, effects: &mut Vec<AppEffect>) {
        let interval = Duration::from_secs(60);
        if self
            .last_settlement_check
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return;
        }
        self.last_settlement_check = Some(now);
        let days = self.config().sidebar.auto_settle_after_days;
        if days == 0 {
            return;
        }
        effects.push(AppEffect::RepaintAfter(interval));
        let epoch = chrono::Utc::now().timestamp();
        let native = self
            .native_agent_service()
            .map_or_else(Vec::new, |service| service.activities());
        let mut invocations = Vec::new();
        for binding in self.workspace.all_bindings() {
            let scope = binding.scope();
            for saved in binding.sessions().sessions() {
                if saved.state.view(epoch) != bootty_mux::session_membership::SessionView::Active
                    || saved.state.pinned
                    || saved.state.auto_settle_disabled
                {
                    continue;
                }
                let busy = native.iter().any(|agent| {
                    agent.binding_id == scope.persistence_value().to_string()
                        && agent.task_identity.as_deref() == Some(&saved.identity)
                        && matches!(
                            agent.status,
                            NativeSessionStatus::Starting
                                | NativeSessionStatus::Working
                                | NativeSessionStatus::Waiting
                        )
                }) || crate::chrome_frame::terminal_activities_in_scope(
                    self,
                    scope,
                    &saved.backend_name,
                    None,
                )
                .iter()
                .any(|agent| {
                    matches!(
                        agent.status,
                        TerminalAgentStatus::Starting
                            | TerminalAgentStatus::Working
                            | TerminalAgentStatus::Approval
                            | TerminalAgentStatus::Input
                            | TerminalAgentStatus::Waiting
                    )
                });
                // Older sessions without observed activity wait for accepted input;
                // startup is not evidence of when they last worked.
                let command = if busy {
                    "session.activity"
                } else if saved
                    .state
                    .should_auto_settle(epoch, i64::from(days).saturating_mul(86_400))
                {
                    "session.settle"
                } else {
                    continue;
                };
                let mut invocation =
                    CommandInvocation::new(command, vec![saved.identity.clone()], Caller::Internal);
                if command == "session.activity" {
                    invocation.arguments.push(epoch.to_string());
                }
                invocation.target = Some(bootty_control::CommandTarget {
                    kind: ResourceKind::Binding,
                    handle: self.binding_target_handle(scope, binding.mux().binding_generation()),
                    generation: binding.mux().binding_generation(),
                });
                invocations.push(invocation);
            }
        }
        for invocation in invocations {
            _ = self.dispatch_command(invocation, super::ViewportSnapshot::default(), effects);
        }
    }
}
