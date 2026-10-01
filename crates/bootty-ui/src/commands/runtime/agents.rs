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
use bootty_mux::{
    executor,
    workspace::{BindingRuntime, WorkspaceRuntime},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
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

/// Snapshot of backend pane labels to the Spaces that list them, each with the socket of the local
/// server running it. Current hook adapters report the pane label and that server; older ones only
/// the label. A label that still matches several Spaces deliberately resolves to no scope instead
/// of the active one.
#[derive(Clone, Default)]
pub(super) struct AgentScopeIndex {
    panes: Arc<Mutex<PaneScopes>>,
}

/// Pane label, then the scope of each Space listing it, with that Space's server socket.
type PaneScopes = BTreeMap<String, BTreeMap<String, Option<PathBuf>>>;

impl AgentPaneResolver for AgentScopeIndex {
    fn scope_for_pane(&self, pane: &str) -> Option<String> {
        self.scope_for_server_pane(None, pane)
    }

    fn scope_for_server_pane(&self, server: Option<&str>, pane: &str) -> Option<String> {
        let candidates = self.panes.lock().ok()?.get(pane).cloned()?;
        let Some(server) = server else {
            return unique_scope(candidates.keys());
        };
        // tmux and rmux name their socket through its resolved path, which may differ from the
        // path Bootty addresses it by (macOS `/tmp` is `/private/tmp`).
        // A hook reaches this app only through the local `bootty`, so its server is a local one.
        // A pane on a server no Space here names, such as a remote host's, is not ours.
        let reported = canonical(reported_socket(server));
        unique_scope(
            candidates
                .iter()
                .filter(|(_, socket)| {
                    socket
                        .as_deref()
                        .is_some_and(|socket| canonical(socket) == reported)
                })
                .map(|(scope, _)| scope),
        )
    }
}

impl AgentScopeIndex {
    /// Rebuild from the workspace. Returns whether any pane's owner changed.
    pub(super) fn refresh(&self, workspace: &WorkspaceRuntime) -> bool {
        // Spaces bound to one backend server all list every session on it, so only membership
        // decides: a Space claims the panes of the sessions it owns, found in its own snapshot.
        // A pane no loaded Space owns resolves to no scope rather than to whichever Space listed it.
        let mut panes = PaneScopes::new();
        for binding in workspace.all_bindings() {
            let scope = binding.scope().persistence_value().to_string();
            let server = binding.server_socket().map(Path::to_path_buf);
            for pane_id in owned_panes(binding) {
                panes
                    .entry(pane_id.to_owned())
                    .or_default()
                    .insert(scope.clone(), server.clone());
            }
        }
        let Ok(mut current) = self.panes.lock() else {
            return false;
        };
        let changed = *current != panes;
        *current = panes;
        changed
    }
}

/// The pane ids in the sessions `binding` owns: its members when it tracks membership, otherwise
/// everything its backend reports.
fn owned_panes(binding: &BindingRuntime) -> impl Iterator<Item = &str> {
    binding
        .member_sessions()
        .into_iter()
        .flat_map(|session| &session.windows)
        .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes))
        .filter_map(|pane| pane.pane_id.as_deref())
}

/// The socket in a pane's `$TMUX` or `$RMUX` value, `socket,pid,session`.
fn reported_socket(server: &str) -> &Path {
    let mut fields = server.rsplitn(3, ',');
    match (fields.next(), fields.next(), fields.next()) {
        (Some(_), Some(_), Some(socket)) => Path::new(socket),
        _ => Path::new(server),
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every open Space's scope, and every pane id on each Space whose backend listing is
/// authoritative. A Space without a complete listing is absent from the second, so nothing is
/// pruned on its account. The listing is the whole server's, not only the Space's members: a pane
/// whose session moved to a Space that has not listed it yet is alive, and its record waits for
/// [`AgentService::reattribute`] to move it there.
pub(super) fn live_panes(
    workspace: &WorkspaceRuntime,
) -> (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>) {
    let spaces = workspace
        .all_bindings()
        .map(|binding| binding.scope().persistence_value().to_string())
        .collect();
    let live = workspace
        .all_bindings()
        .filter(|binding| binding.mux().has_session_snapshot())
        .map(|binding| {
            let panes = binding
                .mux()
                .all_sessions()
                .iter()
                .flat_map(|session| &session.windows)
                .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes))
                .filter_map(|pane| pane.pane_id.clone())
                .collect();
            (binding.scope().persistence_value().to_string(), panes)
        })
        .collect();
    (spaces, live)
}

fn unique_scope<'a>(scopes: impl IntoIterator<Item = &'a String>) -> Option<String> {
    let mut scopes = scopes.into_iter();
    let first = scopes.next()?;
    scopes.all(|scope| scope == first).then(|| first.clone())
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
        let scope = if resolves_backend_pane(&invocation, target_supplied) {
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
            AgentWorkspaceAction::List => {
                let mut agents = self.agent_overview();
                let native = self.commands.native_agents.clone();
                let repaint = self.repaint.clone();
                let (sender, receiver) = mpsc::channel();
                std::thread::spawn(move || {
                    agents.extend(super::native_agents::native_agent_overview(
                        native.as_deref(),
                    ));
                    let _ = sender.send(serialized_command_outcome(agents));
                    repaint();
                });
                return CommandDispatch::Pending(PendingCommandResult::Outcome(receiver));
            }
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

/// Whether the command names a backend pane by id for `AgentScopeIndex` to place: a hook, or a
/// state read given a pane and no target. A state read's target may be the current terminal the
/// command resolved by default, which says nothing about the pane asked for.
fn resolves_backend_pane(invocation: &CommandInvocation, target_supplied: bool) -> bool {
    let pane_state = invocation.command.rsplit('.').next() == Some("state")
        && !target_supplied
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
