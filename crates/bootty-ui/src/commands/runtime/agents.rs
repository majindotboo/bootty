//! Native agent execution and unambiguous backend-pane scope resolution.

use super::{
    CommandDispatch, PendingCommandResult, command_outcome_for_mux_error,
    serialized_command_outcome,
};
use crate::{
    commands::{AgentWorkspaceAction, ExactMuxTarget},
    state::{AppEffect, AppState},
};
use bootty_agents::{AgentCommandExecutor, AgentInvocation, AgentPaneResolver, AgentService};
use bootty_control::{
    AppCommandSendError, AppCommandSender, Caller, CommandCancellation, CommandInvocation,
    CommandOutcome, ResourceKind,
};
use bootty_mux::{executor, workspace::WorkspaceRuntime};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

/// Runs the service's nested terminal commands through the app mailbox. Each nested invocation
/// gets its own cancellation token because the outer agent request is already marked started.
#[derive(Clone)]
pub(super) struct AppCommandAgentExecutor {
    pub sender: AppCommandSender,
}

impl AgentCommandExecutor for AppCommandAgentExecutor {
    fn execute(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandOutcome {
        let nested_cancellation = CommandCancellation::new();
        let receiver = match self.sender.for_caller(Caller::Internal).submit(
            invocation,
            deadline,
            nested_cancellation.clone(),
        ) {
            Ok(receiver) => receiver,
            Err(AppCommandSendError::Overloaded) => {
                return CommandOutcome::Failed {
                    code: "overloaded".to_owned(),
                    message: "application command queue is overloaded".to_owned(),
                };
            }
            Err(AppCommandSendError::Shutdown) => {
                return CommandOutcome::Failed {
                    code: "shutdown".to_owned(),
                    message: "application command channel shut down".to_owned(),
                };
            }
        };
        loop {
            if cancellation.is_cancelled() {
                let _ = nested_cancellation.cancel();
                return CommandOutcome::cancelled();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = nested_cancellation.cancel();
                return CommandOutcome::deadline_exceeded();
            }
            match receiver.recv_timeout(remaining.min(Duration::from_millis(5))) {
                Ok(outcome) => return outcome,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return CommandOutcome::Failed {
                        code: "shutdown".to_owned(),
                        message: "application command response channel closed".to_owned(),
                    };
                }
            }
        }
    }
}

/// Snapshot of backend pane labels to their unique Space binding. Hooks only carry the backend
/// pane label, so an ambiguous label deliberately resolves to no scope instead of the active one.
#[derive(Clone, Default)]
pub(super) struct AgentScopeIndex {
    scopes: Arc<Mutex<BTreeMap<String, Option<String>>>>,
}

impl AgentPaneResolver for AgentScopeIndex {
    fn scope_for_pane(&self, pane: &str) -> Option<String> {
        self.scopes.lock().ok()?.get(pane).cloned().flatten()
    }
}

impl AgentScopeIndex {
    pub(super) fn refresh(&self, workspace: &WorkspaceRuntime) {
        let mut candidates: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for binding in workspace.all_bindings() {
            let scope = binding.scope().persistence_value().to_string();
            let panes = binding
                .mux()
                .all_sessions()
                .iter()
                .flat_map(|session| &session.windows)
                .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes));
            for pane_id in panes.filter_map(|pane| pane.pane_id.as_ref()) {
                candidates
                    .entry(pane_id.clone())
                    .or_default()
                    .insert(scope.clone());
            }
        }

        let scopes = candidates
            .into_iter()
            .map(|(pane, candidates)| {
                let scope = if candidates.len() == 1 {
                    candidates.into_iter().next()
                } else {
                    None
                };
                (pane, scope)
            })
            .collect();
        if let Ok(mut current) = self.scopes.lock() {
            *current = scopes;
        }
    }
}

impl AppState {
    pub(super) fn dispatch_agent_invocation(
        &self,
        agents: Arc<AgentService>,
        invocation: CommandInvocation,
        target_supplied: bool,
        exact_target: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        if let Err(error) =
            executor::begin_synchronous_command(Some((deadline, cancellation.clone())))
        {
            return CommandDispatch::Complete(command_outcome_for_mux_error(error));
        }
        let scope = if resolves_backend_pane(&invocation) {
            // Hook and pane-specific state commands resolve their backend pane through
            // AgentScopeIndex. Never stamp them with whichever Space is active now.
            None
        } else {
            Some(
                exact_target
                    .map_or_else(
                        || self.workspace.active.binding.scope(),
                        ExactMuxTarget::scope,
                    )
                    .persistence_value()
                    .to_string(),
            )
        };
        let mut request =
            AgentInvocation::new(invocation, target_supplied, scope, deadline, cancellation);
        if let Some(exact) = exact_target {
            request.launch_context = self.agent_launch_context(exact);
        }
        let (result_sender, result_receiver) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = agents.invoke(&request);
            let _ = result_sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(result_receiver))
    }

    fn agent_launch_context(&self, exact: &ExactMuxTarget) -> bootty_agents::AgentLaunchContext {
        let scope = exact.scope();
        let (session, window, pane) = exact.ids();
        let mut context = bootty_agents::AgentLaunchContext {
            new_tab: session
                .and_then(|session| {
                    self.mux_resource_target(scope, ResourceKind::Session, session, None)
                })
                .or_else(|| self.current_command_target_for("new_tab", ResourceKind::Session)),
            pane: pane.map(str::to_owned),
            ..Default::default()
        };
        let Some(binding) = self.workspace.binding(scope) else {
            return context;
        };
        context.shell = if cfg!(windows) && binding.multiplexer().remote.is_none() {
            bootty_agents::LaunchShell::Windows
        } else {
            bootty_agents::LaunchShell::Posix
        };
        context.cwd = agent_working_directory(binding.mux().all_sessions(), session, window, pane);
        context
    }

    pub(super) fn dispatch_agent_workspace_command(
        &mut self,
        action: AgentWorkspaceAction,
        exact_target: Option<&ExactMuxTarget>,
        effects: &mut Vec<AppEffect>,
    ) -> CommandDispatch {
        let outcome = match action {
            AgentWorkspaceAction::List => serialized_command_outcome(self.agent_overview()),
            AgentWorkspaceAction::Focus => exact_target.map_or_else(
                || CommandOutcome::Unavailable {
                    message: "No agent pane is available".to_owned(),
                },
                |target| {
                    self.activate_terminal_target(target)
                        .map_or_else(|outcome| outcome, |()| CommandOutcome::success())
                },
            ),
            AgentWorkspaceAction::Next => self.focus_next_agent(),
        };
        if action != AgentWorkspaceAction::List && matches!(outcome, CommandOutcome::Success { .. })
        {
            self.apply_sidebar_action(crate::app_actions::SidebarAction::FocusTerminal);
            effects.push(AppEffect::FocusTerminal);
        }
        CommandDispatch::Complete(outcome)
    }
    fn focus_next_agent(&mut self) -> CommandOutcome {
        let current = self.current_command_target_for("agents.focus", ResourceKind::Terminal);
        let entries = self.agent_overview();
        let next = entries
            .iter()
            .filter(|entry| entry.unread)
            .find(|entry| current.as_ref() != Some(&entry.target))
            .or_else(|| entries.iter().find(|entry| entry.unread));
        let Some(entry) = next else {
            return CommandOutcome::Unavailable {
                message: "No unread agents".to_owned(),
            };
        };
        match self.resolve_command_target(
            "agents.focus",
            Some(ResourceKind::Terminal),
            Some(&entry.target),
        ) {
            Ok((_, Some(target))) => self
                .activate_terminal_target(&target)
                .map_or_else(|outcome| outcome, |()| CommandOutcome::success()),
            Err(outcome) => outcome,
            _ => CommandOutcome::Unavailable {
                message: "Agent pane is unavailable".to_owned(),
            },
        }
    }
}

fn resolves_backend_pane(invocation: &CommandInvocation) -> bool {
    let pane_state = invocation.command.rsplit('.').next() == Some("state")
        && invocation
            .arguments
            .first()
            .is_some_and(|pane| !pane.is_empty());
    invocation.command.ends_with(".ingest") || pane_state
}

fn agent_working_directory(
    sessions: &[bootty_mux::snapshot::MuxSession],
    session: Option<&str>,
    window: Option<&str>,
    pane: Option<&str>,
) -> Option<String> {
    let session = sessions
        .iter()
        .find(|candidate| Some(candidate.id.as_str()) == session)?;
    let window = session
        .windows
        .iter()
        .find(|candidate| Some(candidate.id.as_str()) == window)?;
    let anchor = window
        .panes
        .iter()
        .find(|candidate| candidate.pane_id.as_deref() == pane)
        .unwrap_or(&window.anchor);
    anchor.cwd.clone()
}
