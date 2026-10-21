//! The window supplies time and observed activity; saved session state owns eligibility.
use super::{AppEffect, AppState};
use bootty_agents::{NativeSessionStatus, TerminalAgentStatus};
use bootty_control::{Caller, CommandInvocation, CommandOutcome, ResourceKind};
use bootty_mux::{
    controller::CommandSelection, session_membership::SessionView, workspace::ScopedSessionTarget,
};
use std::time::{Duration, Instant};

pub struct SessionSettlementNavigation {
    current: ScopedSessionTarget,
    identity: String,
    cwd: String,
    next: Option<ScopedSessionTarget>,
}

impl AppState {
    pub(crate) fn session_settlement_navigation(&self) -> Option<SessionSettlementNavigation> {
        let binding = &self.workspace.active.binding;
        let selected = binding.mux().selected_session()?;
        let identity = self.workspace.session_identity(binding.scope(), selected)?;
        let saved = binding.sessions().get(&identity)?;
        let current = ScopedSessionTarget::new(binding.scope(), selected);
        let targets = self.sidebar_navigation_targets();
        let now = crate::clock::ClockSnapshot::now().epoch;
        let next = targets
            .iter()
            .position(|target| target == &current)
            .and_then(|index| {
                targets
                    .iter()
                    .cycle()
                    .skip(index.saturating_add(1))
                    .take(targets.len().saturating_sub(1))
                    .find(|target| {
                        self.workspace.binding(target.scope).is_some_and(|binding| {
                            let identity = self
                                .workspace
                                .session_identity(target.scope, &target.session_id)
                                .unwrap_or_else(|| target.session_id.clone());
                            binding
                                .sessions()
                                .get(&identity)
                                .is_some_and(|saved| saved.state.view(now) == SessionView::Active)
                        })
                    })
                    .cloned()
            });
        Some(SessionSettlementNavigation {
            current,
            identity,
            cwd: saved.cwd.clone(),
            next,
        })
    }

    pub(crate) fn advance_after_session_settlement(
        &mut self,
        navigation: SessionSettlementNavigation,
        outcome: &CommandOutcome,
        effects: &mut Vec<AppEffect>,
    ) {
        let CommandOutcome::Success { value, .. } = outcome else {
            return;
        };
        if value["changed"] != true
            || value["identity"].as_str() != Some(&navigation.identity)
            || self.mux_scope() != navigation.current.scope
            || self.mux().selected_session() != Some(navigation.current.session_id.as_str())
        {
            return;
        }
        if let Some(next) = navigation.next {
            self.sidebar_hovered_session = Some(next);
            if self.activate_sidebar_hovered_session() {
                effects.push(AppEffect::FocusTerminal);
            }
        } else {
            self.open_new_session_dialog_in(navigation.current.scope, navigation.cwd);
        }
    }

    pub(super) fn poll_session_settlement(&mut self, now: Instant, effects: &mut Vec<AppEffect>) {
        let interval = Duration::from_secs(60);
        if self
            .last_settlement_check
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return;
        }
        self.last_settlement_check = Some(now);
        // Busy sessions are stamped even with auto-settle off; "Recent activity" sorts by it.
        let days = self.config().sidebar.auto_settle_after_days;
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
                } else if days > 0
                    && saved
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
            _ = self.dispatch_command_with_selection(
                invocation,
                super::ViewportSnapshot::default(),
                effects,
                CommandSelection::Preserve,
            );
        }
    }
}
