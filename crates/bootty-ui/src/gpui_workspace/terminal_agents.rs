//! Coordination workers come from registered agents and live backend terminal facts.

use bootty_agents::TerminalAgentRecord;
use gpui_kit::{Context, Window};

use super::GpuiWorkspace;
use crate::gpui_orchestration::OrchestrationAgentSession;

#[derive(Default)]
pub(super) struct TerminalAgents {
    records: Vec<TerminalAgentRecord>,
    refreshing: bool,
    revision: Option<u64>,
}

impl GpuiWorkspace {
    pub(super) fn refresh_terminal_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Mux liveness and the active binding can change independently of retained launch metadata.
        self.sync_terminal_agent_workers(window, cx);
        let Some(service) = self.state.terminal_agent_service() else {
            return;
        };
        let revision = service.revision();
        if self.terminal_agents.refreshing || self.terminal_agents.revision == Some(revision) {
            return;
        }
        self.terminal_agents.refreshing = true;
        cx.spawn_in(window, async move |owner, cx| {
            let records = cx
                .background_executor()
                .spawn(async move { service.records() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.terminal_agents.refreshing = false;
                this.terminal_agents.revision = Some(revision);
                this.terminal_agents.records = records;
                this.refresh_terminal_agents(window, cx);
            });
        })
        .detach();
    }

    fn sync_terminal_agent_workers(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tools) = &self.tools else {
            return;
        };
        let binding = &self.state.workspace.active.binding;
        let scope = binding.scope();
        let mux = binding.mux();
        let binding_id = scope.persistence_value().to_string();
        let handle = self
            .state
            .binding_target_handle(scope, mux.binding_generation());
        let workers = self
            .terminal_agents
            .records
            .iter()
            .filter_map(|record| {
                if record.binding_id != binding_id {
                    return None;
                }
                let exact =
                    bootty_mux::target::exact_mux_target(scope, mux, &record.target, &handle)?;
                let (session, window, _) = exact.ids();
                let session = mux
                    .sessions()
                    .iter()
                    .find(|candidate| Some(candidate.id.as_str()) == session)?;
                let name = window
                    .and_then(|id| session.windows.iter().find(|window| window.id == id))
                    .map_or_else(
                        || session.name.clone(),
                        |window| format!("{} · {}", session.name, window.name),
                    );
                Some(OrchestrationAgentSession {
                    name,
                    provider: record.provider,
                    target: record.target.clone(),
                })
            })
            .collect();
        tools.update(cx, |tools, cx| {
            tools.sync_agent_terminals(workers, window, cx);
        });
    }
}
