mod capture;
mod clipboard;
mod files;
mod forwards;
mod git;
mod jobs;
mod links;
mod recovery;
mod shell;
mod wsl;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    task::{Poll, ready},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    app_actions::{KeybindAction, MuxKeyAction},
    commands::{
        AgentWorkspaceAction, CommandCatalog, CommandExecutor, CoreCommandExecutor, ExactMuxTarget,
        SynchronousCommand,
    },
    error_catalog::ErrorNotice,
    state::{AppEffect, AppState, ViewportSnapshot},
};
use bootty_agents::{AgentCommandExecutor, AgentInvocation, AgentPaneResolver, AgentService};
use bootty_control::{
    AppCommandReceiver, AppCommandSendError, AppCommandSender, BoundAppCommandSender, Caller,
    CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ControlEventSender,
    MutationClass, ResourceKind, app_command_channel,
};
use bootty_mux::repository::BindingMembershipMutation;
use bootty_mux::{
    RepaintHandle,
    command::MuxCommand,
    controller::{MuxCommandCompletion, MuxCommandError, MuxCommandResult, SpaceId},
    executor,
    provider::PaneTopology,
    target,
    terminal::decode_scoped_pane_id,
    workspace::WorkspaceRuntime,
};
use bootty_terminal::terminal::{KeyInput, KeyMods, TerminalKey};
use bootty_terminal::terminal_input::TerminalInputCommand;

pub fn command_outcome_message(outcome: &CommandOutcome) -> Option<String> {
    match outcome {
        CommandOutcome::Success { .. } => None,
        CommandOutcome::Unsupported { message }
        | CommandOutcome::Unavailable { message }
        | CommandOutcome::Denied { message }
        | CommandOutcome::StaleTarget { message }
        | CommandOutcome::Failed { message, .. } => Some(message.clone()),
        CommandOutcome::ConfirmationRequired { .. } => {
            Some(ErrorNotice::CommandRequiresConfirmation.to_string())
        }
    }
}

pub fn command_outcome_for_mux_error(error: MuxCommandError) -> CommandOutcome {
    match error {
        MuxCommandError::Cancelled => CommandOutcome::cancelled(),
        MuxCommandError::DeadlineExceeded => CommandOutcome::deadline_exceeded(),
        MuxCommandError::Unsupported => CommandOutcome::Unsupported {
            message: ErrorNotice::MuxOperationUnsupported.to_string(),
        },
        MuxCommandError::Unavailable => CommandOutcome::Unavailable {
            message: ErrorNotice::MuxOperationUnavailable.to_string(),
        },
        MuxCommandError::Stale => CommandOutcome::StaleTarget {
            message: ErrorNotice::MuxOperationCapabilityStale.to_string(),
        },
        MuxCommandError::Failed(message) => CommandOutcome::Failed {
            code: "execution_failed".to_owned(),
            message,
        },
    }
}

fn serialized_command_outcome(value: impl serde::Serialize) -> CommandOutcome {
    match serde_json::to_value(value) {
        Ok(value) => CommandOutcome::Success {
            value,
            warnings: Vec::new(),
        },
        Err(error) => CommandOutcome::Failed {
            code: "serialization_failed".to_owned(),
            message: error.to_string(),
        },
    }
}

static NEXT_WINDOW_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Runs the service's nested terminal commands through the app mailbox. Each nested invocation
/// gets its own cancellation token because the outer agent request is already marked started.
#[derive(Clone)]
struct AppCommandAgentExecutor {
    sender: AppCommandSender,
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
struct AgentScopeIndex {
    scopes: Arc<Mutex<BTreeMap<String, Option<String>>>>,
}

impl AgentPaneResolver for AgentScopeIndex {
    fn scope_for_pane(&self, pane: &str) -> Option<String> {
        self.scopes.lock().ok()?.get(pane).cloned().flatten()
    }
}

impl AgentScopeIndex {
    fn refresh(&self, workspace: &WorkspaceRuntime) {
        let mut candidates: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for binding in workspace.all_bindings() {
            let scope = binding.scope().persistence_value().to_string();
            for session in binding.mux().all_sessions() {
                for window in &session.windows {
                    for pane in std::iter::once(&window.anchor).chain(&window.panes) {
                        if let Some(pane_id) = pane.pane_id.as_deref() {
                            candidates
                                .entry(pane_id.to_owned())
                                .or_default()
                                .insert(scope.clone());
                        }
                    }
                }
            }
        }
        let scopes = candidates
            .into_iter()
            .map(|(pane, candidates)| {
                let scope = (candidates.len() == 1)
                    .then(|| candidates.into_iter().next())
                    .flatten();
                (pane, scope)
            })
            .collect();
        if let Ok(mut current) = self.scopes.lock() {
            *current = scopes;
        }
    }
}

fn process_handle() -> String {
    static HANDLE: OnceLock<String> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("{}:{nanos:032x}", std::process::id())
        })
        .clone()
}

struct ResolvedCommandContext {
    invocation: CommandInvocation,
    exact_target: Option<ExactMuxTarget>,
    planned_mux_command: Option<MuxCommand>,
    target_supplied: bool,
}

pub enum PendingCommandResult {
    Forward {
        result: mpsc::Receiver<Result<forwards::ForwardResult, String>>,
    },
    Link {
        scope: SpaceId,
        target: CommandTarget,
        result: mpsc::Receiver<Result<links::ResolvedLink, String>>,
    },
    Clipboard {
        scope: SpaceId,
        target: CommandTarget,
        result: mpsc::Receiver<Result<String, String>>,
    },
    DitchCleanup {
        scope: SpaceId,
        command: MuxCommand,
        membership: Option<Box<BindingMembershipMutation>>,
        result: mpsc::Receiver<bootty_mux::workflow::DitchCleanupOutcome>,
    },
    Mux {
        layout: Option<bootty_mux::workspace::PreparedPaneArrangement>,
        scope: SpaceId,
        command: MuxCommand,
        membership: Option<Box<BindingMembershipMutation>>,
        result: mpsc::Receiver<MuxCommandResult>,
    },
    Outcome(mpsc::Receiver<CommandOutcome>),
}

pub enum CommandDispatch {
    Complete(CommandOutcome),
    Pending(PendingCommandResult),
}

pub struct PendingAppCommand {
    pub(crate) deadline: Instant,
    pub(crate) cancellation: CommandCancellation,
    pub(crate) response: Option<mpsc::Sender<CommandOutcome>>,
    pub(crate) result: PendingCommandResult,
}

impl PendingAppCommand {
    fn cancellation_outcome(&self, now: Instant) -> Option<CommandOutcome> {
        // Reconcile a cleanup result before honoring cancellation of its backend command.
        if matches!(self.result, PendingCommandResult::DitchCleanup { .. }) {
            return None;
        }
        if self.cancellation.is_cancelled() {
            Some(CommandOutcome::cancelled())
        } else if now >= self.deadline && self.cancellation.cancel() {
            Some(CommandOutcome::deadline_exceeded())
        } else {
            None
        }
    }
}

fn poll_command_result<T>(result: &mpsc::Receiver<T>) -> Poll<Result<T, mpsc::RecvError>> {
    match result.try_recv() {
        Ok(value) => Poll::Ready(Ok(value)),
        Err(mpsc::TryRecvError::Empty) => Poll::Pending,
        Err(mpsc::TryRecvError::Disconnected) => Poll::Ready(Err(mpsc::RecvError)),
    }
}

pub struct CommandRuntime {
    instance_handle: String,
    instance_generation: u64,
    window_generation: u64,
    queued: Option<CommandInvocation>,
    sender: AppCommandSender,
    receiver: AppCommandReceiver,
    catalog: Arc<CommandCatalog>,
    agent_service: Option<Arc<AgentService>>,
    agent_scope_index: Option<Arc<AgentScopeIndex>>,
    pending: Vec<PendingAppCommand>,
    jobs: Arc<bootty_host::jobs::JobRegistry>,
    forwards: Vec<(SpaceId, u64, Arc<bootty_host::ssh_forward::ForwardLease>)>,
}

impl Drop for CommandRuntime {
    fn drop(&mut self) {
        self.jobs.retire();
        for pending in &self.pending {
            let _ = pending.cancellation.cancel();
        }
        if let Some(agents) = &self.agent_service {
            agents.retire();
        }
    }
}

impl CommandRuntime {
    pub(crate) fn new(repaint: RepaintHandle) -> Self {
        let (sender, receiver) = app_command_channel(64, repaint);
        Self::from_channel(sender, receiver, None, None, None)
    }

    pub(crate) fn new_with_agents(repaint: RepaintHandle, events: ControlEventSender) -> Self {
        let (sender, receiver) = app_command_channel(64, repaint);
        let scope_index = Arc::new(AgentScopeIndex::default());
        let nested_commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            sender: sender.clone(),
        });
        let agents = Arc::new(AgentService::with_control_and_resolver(
            nested_commands,
            events.clone(),
            scope_index.clone(),
        ));
        Self::from_channel(
            sender,
            receiver,
            Some(agents),
            Some(scope_index),
            Some(events),
        )
    }

    fn from_channel(
        sender: AppCommandSender,
        receiver: AppCommandReceiver,
        agents: Option<Arc<AgentService>>,
        agent_scope_index: Option<Arc<AgentScopeIndex>>,
        events: Option<ControlEventSender>,
    ) -> Self {
        let publishes_jobs = events.is_some();
        let jobs = jobs::registry(events);
        Self {
            instance_handle: process_handle(),
            instance_generation: 1,
            window_generation: NEXT_WINDOW_GENERATION.fetch_add(1, Ordering::Relaxed),
            queued: None,
            sender,
            receiver,
            catalog: Arc::new(CommandCatalog::with_services(
                agents.clone(),
                if publishes_jobs {
                    Arc::downgrade(&jobs)
                } else {
                    std::sync::Weak::new()
                },
            )),
            agent_service: agents,
            agent_scope_index,
            pending: Vec::new(),
            forwards: Vec::new(),
            jobs,
        }
    }

    pub(crate) fn refresh_agent_scopes(&self, workspace: &WorkspaceRuntime) {
        if let Some(index) = &self.agent_scope_index {
            index.refresh(workspace);
        }
    }

    pub(crate) fn catalog(&self) -> Arc<CommandCatalog> {
        Arc::clone(&self.catalog)
    }

    pub(crate) fn queue(&mut self, invocation: CommandInvocation) {
        self.queued = Some(invocation);
    }

    pub(crate) fn clear_queue(&mut self) {
        self.queued = None;
    }

    pub(crate) fn target_kind(&self, command: &str) -> Option<ResourceKind> {
        self.catalog.describe(command)?.target
    }

    pub(crate) const fn has_queued(&self) -> bool {
        self.queued.is_some()
    }

    pub(crate) const fn take_queued(&mut self) -> Option<CommandInvocation> {
        self.queued.take()
    }

    pub(crate) fn target_identity(&self) -> (&str, u64, u64) {
        (
            &self.instance_handle,
            self.instance_generation,
            self.window_generation,
        )
    }
}

impl AppState {
    pub(crate) fn pane_arrangement_pending(&self) -> bool {
        self.commands.pending.iter().any(|pending| {
            matches!(&pending.result,
            PendingCommandResult::Mux { scope, layout: Some(_), .. } if *scope == self.mux_scope())
        })
    }

    fn reject_command(&mut self, outcome: CommandOutcome) -> CommandDispatch {
        if let Some(message) = command_outcome_message(&outcome) {
            self.record_error(message);
        }
        CommandDispatch::Complete(outcome)
    }

    /// Returns a non-blocking sender for producers outside the UI-owner call stack.
    ///
    /// UI code dispatches directly and must not synchronously wait on this channel's response.
    pub fn app_command_sender(&self, caller: Caller) -> BoundAppCommandSender {
        self.commands.sender.for_caller(caller)
    }

    pub fn command_catalog(&self) -> Arc<CommandCatalog> {
        Arc::clone(&self.commands.catalog)
    }

    /// Returns the composed native agent owner for provider pane snapshots. Headless app states
    /// return `None` and retain the static catalog's explicit unsupported behavior.
    pub fn agent_service(&self) -> Option<Arc<AgentService>> {
        self.commands.catalog.agents()
    }

    pub(crate) fn drain_app_commands(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        self.drain_pending_app_commands(Instant::now(), effects);
        let mut drained = 0_usize;
        for _ in 0..32 {
            let request = match self.commands.receiver.try_recv() {
                Ok(request) => request,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            drained = drained.saturating_add(1);
            let now = Instant::now();
            let dispatch = if request.cancellation.is_cancelled() {
                CommandDispatch::Complete(CommandOutcome::cancelled())
            } else if now >= request.deadline {
                let _ = request.cancellation.cancel();
                CommandDispatch::Complete(CommandOutcome::deadline_exceeded())
            } else {
                self.dispatch_command_with_execution(
                    request.invocation,
                    viewport,
                    effects,
                    Some((request.deadline, request.cancellation.clone())),
                )
            };
            match dispatch {
                CommandDispatch::Complete(outcome) => {
                    let _ = request.response.send(outcome);
                }
                CommandDispatch::Pending(result) => {
                    self.commands.pending.push(PendingAppCommand {
                        deadline: request.deadline,
                        cancellation: request.cancellation,
                        response: Some(request.response),
                        result,
                    });
                }
            }
        }
        if drained == 32 {
            effects.push(AppEffect::RequestRepaint);
        }
    }

    fn drain_pending_app_commands(&mut self, now: Instant, effects: &mut Vec<AppEffect>) {
        self.commands.forwards.retain(|(scope, generation, _)| {
            self.workspace
                .binding(*scope)
                .is_some_and(|binding| binding.mux().binding_generation() == *generation)
        });
        for mut pending in std::mem::take(&mut self.commands.pending) {
            match self.poll_pending_app_command(&mut pending, now, effects) {
                Poll::Pending => self.commands.pending.push(pending),
                Poll::Ready(None) => {}
                Poll::Ready(Some(outcome)) => {
                    if let Some(response) = pending.response {
                        let _ = response.send(outcome);
                    } else if let Some(message) = command_outcome_message(&outcome) {
                        self.record_error(message);
                    }
                }
            }
        }
    }

    fn poll_pending_app_command(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        if let Some(outcome) = pending.cancellation_outcome(now) {
            if let PendingCommandResult::Mux {
                scope,
                membership: Some(_),
                ..
            } = &pending.result
            {
                self.workspace
                    .defer_binding_membership_reconciliation(*scope);
            }
            return Poll::Ready(Some(outcome));
        }
        let outcome = match &mut pending.result {
            PendingCommandResult::DitchCleanup {
                scope,
                command,
                membership,
                result,
            } => {
                match self.poll_ditch_cleanup(
                    *scope,
                    command,
                    membership,
                    result,
                    (pending.deadline, pending.cancellation.clone()),
                ) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Ready(Some(result)) => {
                        pending.result = result;
                    }
                }
                return self.poll_pending_app_command(pending, now, effects);
            }
            PendingCommandResult::Forward { result } => match ready!(poll_command_result(result)) {
                Ok(Ok(result)) => self.finish_forward(result),
                Ok(Err(message)) => links::failure(message),
                Err(mpsc::RecvError) => links::failure("Forward worker stopped".to_owned()),
            },
            PendingCommandResult::Link {
                scope,
                target,
                result,
            } => match ready!(poll_command_result(result)) {
                Ok(Ok(link)) if pending.cancellation.try_start() => {
                    self.finish_link_open(*scope, target, link, effects)
                }
                Ok(Ok(_)) => CommandOutcome::cancelled(),
                Ok(Err(message)) => links::failure(message),
                Err(mpsc::RecvError) => links::failure("link worker stopped".to_owned()),
            },
            PendingCommandResult::Clipboard {
                scope,
                target,
                result,
            } => match ready!(poll_command_result(result)) {
                Ok(Ok(text)) if pending.cancellation.try_start() => {
                    self.finish_clipboard_paste(*scope, target, &text)
                }
                Ok(Ok(_)) => CommandOutcome::cancelled(),
                Ok(Err(message)) => clipboard::upload_failure(message),
                Err(mpsc::RecvError) => {
                    clipboard::upload_failure("clipboard worker stopped".to_owned())
                }
            },
            PendingCommandResult::Mux {
                scope,
                command,
                membership,
                result,
                layout,
            } => {
                if let Ok(result) = ready!(poll_command_result(result)) {
                    self.command_outcome_for_mux_result(
                        *scope,
                        command,
                        membership.as_deref(),
                        result,
                        layout.as_ref(),
                    )
                } else {
                    if membership.is_some() {
                        self.workspace
                            .defer_binding_membership_reconciliation(*scope);
                    }
                    CommandOutcome::Failed {
                        code: "backend_worker_stopped".to_owned(),
                        message: ErrorNotice::MuxCommandWorkerStopped.to_string(),
                    }
                }
            }
            PendingCommandResult::Outcome(result) => match ready!(poll_command_result(result)) {
                Ok(outcome) => outcome,
                Err(mpsc::RecvError) => CommandOutcome::Failed {
                    code: "command_worker_stopped".to_owned(),
                    message: ErrorNotice::CommandWorkerStopped.to_string(),
                },
            },
        };
        Poll::Ready(Some(outcome))
    }

    fn poll_ditch_cleanup(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        membership: &mut Option<Box<BindingMembershipMutation>>,
        result: &mpsc::Receiver<bootty_mux::workflow::DitchCleanupOutcome>,
        execution: (Instant, CommandCancellation),
    ) -> Poll<Option<PendingCommandResult>> {
        use bootty_mux::workflow::DitchCleanupOutcome;
        let cleanup = match ready!(poll_command_result(result)) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvError) => {
                DitchCleanupOutcome::NoAction("Git cleanup worker stopped".to_owned())
            }
        };
        match cleanup {
            DitchCleanupOutcome::NoAction(error) => {
                self.workspace
                    .defer_binding_membership_reconciliation(scope);
                self.record_notice(ErrorNotice::Ditch(error));
                return Poll::Ready(None);
            }
            DitchCleanupOutcome::Partial { branch, error } => {
                self.record_notice(ErrorNotice::DitchPartial(format!(
                    "worktree removed; branch '{branch}' remains: {error}"
                )));
            }
            DitchCleanupOutcome::Complete => {}
        }
        // Commit against the original binding with the original deadline and cancellation token.
        let Some(submitted) = executor::submit_authoritative_command_for_scope(
            &mut self.workspace,
            &self.repaint,
            scope,
            command.clone(),
            membership.take(),
            Some(execution),
        ) else {
            self.workspace
                .defer_binding_membership_reconciliation(scope);
            self.record_notice(ErrorNotice::Ditch(
                "Session binding disappeared after cleanup".to_owned(),
            ));
            return Poll::Ready(None);
        };
        Poll::Ready(Some(PendingCommandResult::Mux {
            scope: submitted.scope,
            command: submitted.command,
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        }))
    }

    pub(crate) fn dispatch_command(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        let asynchronous = crate::action_catalog::Command::from_action(&invocation.command)
            .and_then(crate::commands::DockAction::from_command)
            .is_some()
            || invocation.command.starts_with("agents.")
            || invocation.command == "paste_from_clipboard"
            || invocation.command.starts_with("git.")
            || invocation.command.starts_with("files.")
            || invocation.command.starts_with("jobs.")
            || invocation.command.starts_with("transfers.")
            || invocation.command.starts_with("forwards.")
            || invocation.command.starts_with("recovery.")
            || invocation.command.starts_with("shell.")
            || invocation.command.starts_with("history.")
            || matches!(
                invocation.command.as_str(),
                "terminal.capture" | "terminal.export"
            )
            || invocation.command.starts_with("pane.")
            || invocation.command == "link.open";
        let (deadline, cancellation) = executor::command_execution(None);
        let execution = asynchronous.then(|| (deadline, cancellation.clone()));
        match self.dispatch_command_with_execution(invocation, viewport, effects, execution) {
            CommandDispatch::Complete(outcome) => outcome,
            CommandDispatch::Pending(result) => {
                self.commands.pending.push(PendingAppCommand {
                    deadline,
                    cancellation,
                    response: None,
                    result,
                });
                CommandOutcome::Success {
                    value: serde_json::json!({"queued": true}),
                    warnings: Vec::new(),
                }
            }
        }
    }

    fn dispatch_command_with_execution(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        crate::switch_benchmark::requested(&invocation.command);
        let target_supplied = invocation.target.is_some();
        let mut resolved = match self.commands.catalog.resolve(invocation) {
            Ok(resolved) => resolved,
            Err(outcome) => return self.reject_command(outcome),
        };
        let (target, exact_target) = match self.resolve_command_target(
            &resolved.invocation.command,
            resolved.descriptor.target,
            resolved.invocation.target.as_ref(),
        ) {
            Ok(target) => target,
            Err(outcome) => return self.reject_command(outcome),
        };
        resolved.invocation.target = target;
        let planned_mux_command = match self.preflight_resolved_invocation(
            &resolved,
            exact_target.as_ref(),
            target_supplied,
            effects,
        ) {
            Ok(command) => command,
            Err(outcome) => return self.reject_command(outcome),
        };
        let context = ResolvedCommandContext {
            invocation: resolved.invocation,
            exact_target,
            planned_mux_command,
            target_supplied,
        };
        match resolved.executor {
            CommandExecutor::Core(executor) => {
                self.dispatch_core_command(executor, context, viewport, effects, execution)
            }
            CommandExecutor::Agent(agents) => self.dispatch_agent_invocation(
                agents,
                context.invocation,
                context.target_supplied,
                context.exact_target.as_ref(),
                execution,
            ),
            CommandExecutor::UncomposedAgent => {
                CommandDispatch::Complete(CommandOutcome::Unsupported {
                    message: "native agent service is not composed for this app instance"
                        .to_owned(),
                })
            }
        }
    }

    fn dispatch_core_command(
        &mut self,
        executor: CoreCommandExecutor,
        context: ResolvedCommandContext,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let ResolvedCommandContext {
            invocation,
            exact_target,
            planned_mux_command,
            ..
        } = context;
        let scope = exact_target
            .as_ref()
            .map_or_else(|| self.mux_scope(), ExactMuxTarget::scope);
        let caller = invocation.caller;
        match executor {
            CoreCommandExecutor::Recovery(action, arguments) => {
                self.dispatch_recovery(action, &arguments, execution)
            }
            CoreCommandExecutor::ShellPrompt(action, arguments) => exact_target.map_or_else(
                || {
                    CommandDispatch::Complete(CommandOutcome::Unavailable {
                        message: "No terminal prompt is available".to_owned(),
                    })
                },
                |exact| self.dispatch_shell_prompt(&exact, action, &arguments, execution),
            ),
            CoreCommandExecutor::Forward(action, arguments) => self.dispatch_forward(
                action,
                &arguments,
                exact_target.as_ref().map(ExactMuxTarget::scope),
                execution,
            ),
            CoreCommandExecutor::Job(action, arguments) => self.dispatch_job_command(
                action,
                arguments,
                exact_target.as_ref().map(ExactMuxTarget::scope),
                execution,
            ),
            CoreCommandExecutor::AgentWorkspace(action) => {
                self.dispatch_agent_workspace_command(action, exact_target.as_ref(), effects)
            }
            CoreCommandExecutor::Theme(action, arguments) => {
                self.dispatch_theme_command(action, &arguments, execution, effects)
            }
            CoreCommandExecutor::CaptureTerminal(arguments, export) => {
                let (Some(exact), Some(target)) = (exact_target, invocation.target) else {
                    return self.reject_command(CommandOutcome::Unavailable {
                        message: "Capture needs an attached terminal".to_owned(),
                    });
                };
                self.dispatch_terminal_capture(&exact, target, &arguments, export, execution)
            }
            CoreCommandExecutor::WslList => self.dispatch_wsl_list(execution),
            CoreCommandExecutor::OpenLink(arguments) => {
                let (Some(exact), Some(target)) = (exact_target, invocation.target) else {
                    return self.reject_command(links::failure(
                        "Link needs an attached terminal".to_owned(),
                    ));
                };
                self.dispatch_link_open(target, &exact, &arguments, execution)
            }
            CoreCommandExecutor::Keybind(KeybindAction::PasteFromClipboard) => {
                let Some(target) = invocation.target else {
                    return self.reject_command(CommandOutcome::Unavailable {
                        message: "clipboard paste requires a terminal".to_owned(),
                    });
                };
                self.dispatch_clipboard_paste(scope, target, execution)
            }
            CoreCommandExecutor::Pane(action, arguments) => {
                self.dispatch_pane_command(action, &arguments, exact_target, execution)
            }
            CoreCommandExecutor::File(action, arguments) => self.dispatch_file_action(
                scope,
                action,
                &arguments,
                invocation.target,
                effects,
                execution,
            ),
            CoreCommandExecutor::Git(action, arguments) => self.dispatch_git_action(
                scope,
                action,
                arguments,
                invocation.target,
                effects,
                execution,
            ),
            CoreCommandExecutor::Synchronous(executor) => {
                self.dispatch_synchronous_command(executor, effects, execution)
            }
            CoreCommandExecutor::Keybind(action) => self.dispatch_resolved_keybind_command(
                action,
                planned_mux_command,
                caller,
                viewport,
                effects,
                execution,
            ),
            CoreCommandExecutor::Dock(action, group) => {
                Self::dispatch_dock_command(action, group, effects, execution)
            }
        }
    }

    fn dispatch_dock_command(
        action: super::DockAction,
        group: Option<u64>,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (response, result) = mpsc::channel();
        effects.push(AppEffect::Dock(crate::commands::DockRequest::new(
            action,
            group,
            execution,
            Some(response),
        )));
        CommandDispatch::Pending(PendingCommandResult::Outcome(result))
    }

    fn dispatch_pane_command(
        &mut self,
        action: super::PaneAction,
        arguments: &[String],
        exact_target: Option<ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(ExactMuxTarget::Session(scope, session)) = exact_target else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "Pane arrangement requires a live session target".to_owned(),
            });
        };
        let command = match action.command(session, arguments) {
            Ok(command) => command,
            Err(message) => {
                return self.reject_command(CommandOutcome::Failed {
                    code: "invalid_arguments".to_owned(),
                    message,
                });
            }
        };
        let Some(submitted) = executor::submit_authoritative_command_for_scope(
            &mut self.workspace,
            &self.repaint,
            scope,
            command,
            None,
            execution,
        ) else {
            return self.reject_command(CommandOutcome::StaleTarget {
                message: "Pane binding was closed".to_owned(),
            });
        };
        CommandDispatch::Pending(PendingCommandResult::Mux {
            scope: submitted.scope,
            command: submitted.command,
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        })
    }

    fn preflight_resolved_invocation(
        &mut self,
        resolved: &crate::commands::ResolvedCommandInvocation,
        exact_target: Option<&ExactMuxTarget>,
        target_supplied: bool,
        effects: &mut Vec<AppEffect>,
    ) -> Result<Option<MuxCommand>, CommandOutcome> {
        if resolved.descriptor.mutation == MutationClass::Destructive
            && matches!(
                resolved.invocation.caller,
                Caller::Cli | Caller::Socket | Caller::Luau
            )
            && resolved.invocation.confirmation.as_ref()
                != Some(&resolved.invocation.confirmation())
        {
            return Err(CommandOutcome::ConfirmationRequired {
                confirmation: Box::new(resolved.invocation.confirmation()),
            });
        }
        if resolved.descriptor.mutation != MutationClass::Read {
            effects.push(AppEffect::RequestRepaint);
        }
        let planned_mux_command = match &resolved.executor {
            CommandExecutor::Core(CoreCommandExecutor::Keybind(KeybindAction::Mux(action))) => {
                self.plan_mux_key_action(*action, exact_target)
            }
            _ => None,
        };
        let arranging = matches!(
            &resolved.executor,
            CommandExecutor::Core(CoreCommandExecutor::Pane(..))
        );
        let scope = exact_target.map_or_else(|| self.mux_scope(), ExactMuxTarget::scope);
        if (arranging || planned_mux_command.is_some()) && self.commands.pending.iter().any(|pending| {
            matches!(&pending.result, PendingCommandResult::Mux { scope: pending_scope, layout, .. }
                if *pending_scope == scope && (arranging || layout.is_some()))
        }) {
            return Err(CommandOutcome::Failed {
                code: "pane_arrangement_busy".to_owned(),
                message: "Wait for the current pane operation to complete".to_owned(),
            });
        }
        if let Some(command) = planned_mux_command.as_ref()
            && let Some(outcome) = self.preflight_mux_command(command)
        {
            return Err(outcome);
        }
        if target_supplied
            && matches!(
                &resolved.executor,
                CommandExecutor::Core(
                    CoreCommandExecutor::Keybind(KeybindAction::Write(_))
                        | CoreCommandExecutor::Synchronous(
                            SynchronousCommand::PasteTerminal(_)
                                | SynchronousCommand::SubmitTerminal
                        )
                )
            )
            && let Some(exact_target) = exact_target
            && let Err(outcome) = self.activate_terminal_target(exact_target)
        {
            return Err(outcome);
        }
        Ok(planned_mux_command)
    }

    fn dispatch_agent_invocation(
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
        let scope = if invocation.command.ends_with(".ingest")
            || (invocation.command.rsplit('.').next() == Some("state")
                && invocation
                    .arguments
                    .first()
                    .is_some_and(|pane| !pane.is_empty()))
        {
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
            let scope = exact.scope();
            let (session, window, pane) = exact.ids();
            request.launch_context.new_tab = session
                .and_then(|session| {
                    self.mux_resource_target(scope, ResourceKind::Session, session, None)
                })
                .or_else(|| self.current_command_target_for("new_tab", ResourceKind::Session));
            request.launch_context.pane = pane.map(str::to_owned);
            if let Some(binding) = self.workspace.binding(scope) {
                request.launch_context.shell =
                    if cfg!(windows) && binding.multiplexer().remote.is_none() {
                        bootty_agents::LaunchShell::Windows
                    } else {
                        bootty_agents::LaunchShell::Posix
                    };
                request.launch_context.cwd = binding
                    .mux()
                    .all_sessions()
                    .iter()
                    .find(|candidate| Some(candidate.id.as_str()) == session)
                    .and_then(|session| {
                        session
                            .windows
                            .iter()
                            .find(|candidate| Some(candidate.id.as_str()) == window)
                    })
                    .and_then(|window| {
                        window
                            .panes
                            .iter()
                            .find(|candidate| candidate.pane_id.as_deref() == pane)
                            .or(Some(&window.anchor))
                    })
                    .and_then(|anchor| anchor.cwd.clone());
            }
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

    fn dispatch_agent_workspace_command(
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
            AgentWorkspaceAction::Next => {
                let current =
                    self.current_command_target_for("agents.focus", ResourceKind::Terminal);
                let entries = self.agent_overview();
                let next = entries
                    .iter()
                    .filter(|entry| entry.unread)
                    .find(|entry| current.as_ref() != Some(&entry.target))
                    .or_else(|| entries.iter().find(|entry| entry.unread));
                next.map_or_else(
                    || CommandOutcome::Unavailable {
                        message: "No unread agents".to_owned(),
                    },
                    |entry| match self.resolve_command_target(
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
                    },
                )
            }
        };
        if action != AgentWorkspaceAction::List && matches!(outcome, CommandOutcome::Success { .. })
        {
            self.apply_sidebar_action(crate::app_actions::SidebarAction::FocusTerminal);
            effects.push(AppEffect::FocusTerminal);
        }
        CommandDispatch::Complete(outcome)
    }

    fn preflight_mux_command(&self, command: &MuxCommand) -> Option<CommandOutcome> {
        match executor::preflight_command(&self.workspace, command) {
            Err(MuxCommandError::Failed(message)) => Some(CommandOutcome::Unavailable { message }),
            Err(MuxCommandError::Unsupported) => Some(CommandOutcome::Unsupported {
                message: ErrorNotice::MuxOperationUnsupported.to_string(),
            }),
            Err(MuxCommandError::Unavailable) => Some(CommandOutcome::Unavailable {
                message: ErrorNotice::MuxOperationUnavailable.to_string(),
            }),
            Err(MuxCommandError::Stale) => Some(CommandOutcome::StaleTarget {
                message: ErrorNotice::MuxOperationCapabilityStale.to_string(),
            }),
            Ok(()) | Err(MuxCommandError::Cancelled | MuxCommandError::DeadlineExceeded) => None,
        }
    }

    fn resolve_command_target(
        &self,
        command: &str,
        expected: Option<ResourceKind>,
        supplied: Option<&CommandTarget>,
    ) -> Result<(Option<CommandTarget>, Option<ExactMuxTarget>), CommandOutcome> {
        let Some(expected) = expected else {
            return if supplied.is_none() {
                Ok((None, None))
            } else {
                Err(CommandOutcome::Denied {
                    message: ErrorNotice::CommandDoesNotAcceptTarget.to_string(),
                })
            };
        };
        if supplied.is_some_and(|target| {
            target.kind != expected
                && !(command == "new_tab" && target.kind == ResourceKind::Binding)
        }) {
            return Err(CommandOutcome::Denied {
                message: ErrorNotice::CommandRequiresTarget(format!(
                    "command requires a {expected:?} target"
                ))
                .raw_message(),
            });
        }
        if let Some(supplied) = supplied {
            if self
                .current_command_target_for(command, expected)
                .is_some_and(|current| current == *supplied)
            {
                return Ok((
                    Some(supplied.clone()),
                    self.current_exact_mux_target_for(command, expected),
                ));
            }
            if let Some(exact) = target::exact_mux_target(
                self.workspace.active.binding.scope(),
                self.workspace.active.binding.mux(),
                supplied,
                &self.binding_target_handle(
                    self.workspace.active.binding.scope(),
                    self.workspace.active.binding.mux().binding_generation(),
                ),
            ) {
                return Ok((Some(supplied.clone()), Some(exact)));
            }
            if ((command.starts_with("git.")
                || command.starts_with("files.")
                || matches!(
                    command,
                    "jobs.start" | "transfers.start" | "forwards.open" | "history.search"
                ))
                && expected == ResourceKind::Binding)
                || (command.starts_with("pane.") && expected == ResourceKind::Session)
                || ((command == "link.open"
                    || command == "agents.focus"
                    || command.ends_with(".acknowledge"))
                    && expected == ResourceKind::Terminal)
            {
                for binding in self.workspace.all_bindings() {
                    let handle = self
                        .binding_target_handle(binding.scope(), binding.mux().binding_generation());
                    if let Some(exact) =
                        target::exact_mux_target(binding.scope(), binding.mux(), supplied, &handle)
                    {
                        return Ok((Some(supplied.clone()), Some(exact)));
                    }
                }
            }
            return Err(CommandOutcome::StaleTarget {
                message: ErrorNotice::StaleCommandTarget(format!(
                    "the {expected:?} target is stale"
                ))
                .raw_message(),
            });
        }
        let Some(current) = self.current_command_target_for(command, expected) else {
            return Err(CommandOutcome::Unavailable {
                message: ErrorNotice::NoCurrentTarget(format!(
                    "no current {expected:?} target is available"
                ))
                .raw_message(),
            });
        };
        // The opaque handle is only an equality token. Build the typed target from current mux
        // state after the complete wire target (kind, handle, and generation) has matched.
        let exact = self.current_exact_mux_target_for(command, expected);
        Ok((Some(current), exact))
    }

    fn activate_terminal_target(&mut self, target: &ExactMuxTarget) -> Result<(), CommandOutcome> {
        let scope = target.scope();
        let (session, window, pane) = target.ids();
        let Some(session) = session.map(str::to_owned) else {
            return Ok(());
        };
        let window = window.map(str::to_owned);
        let pane = pane.map(str::to_owned);
        self.workspace
            .activate_target(scope, &session, window.as_deref(), &self.repaint)
            .map_err(|error| CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message: error.to_string(),
            })?;
        if let Some(pane) = pane {
            self.workspace.active.binding.focus_pane(&pane);
        }
        self.sync_terminal_panes_now();
        (self.repaint)();
        Ok(())
    }

    pub(crate) fn current_exact_mux_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<ExactMuxTarget> {
        let scope = self.workspace.active.binding.scope();
        let (session_id, window_id, pane_id) = self.selected_mux_resource_path();
        match kind {
            ResourceKind::Binding => Some(ExactMuxTarget::Binding(scope)),
            ResourceKind::Session => session_id
                .map(|session_id| ExactMuxTarget::Session(scope, session_id))
                .or_else(|| (command == "new_tab").then_some(ExactMuxTarget::Binding(scope))),
            ResourceKind::MuxWindow => Some(ExactMuxTarget::Window(scope, session_id?, window_id?)),
            ResourceKind::Pane => Some(ExactMuxTarget::Pane(
                scope,
                session_id?,
                window_id?,
                pane_id?,
            )),
            ResourceKind::Terminal => match (session_id, window_id, pane_id) {
                (Some(session), Some(window), Some(pane)) => {
                    Some(ExactMuxTarget::Pane(scope, session, window, pane))
                }
                (Some(session), Some(window), None) => {
                    Some(ExactMuxTarget::Window(scope, session, window))
                }
                (Some(session), None, _) => Some(ExactMuxTarget::Session(scope, session)),
                (None, _, _) => Some(ExactMuxTarget::Binding(scope)),
            },
            ResourceKind::Instance | ResourceKind::ApplicationWindow => None,
        }
    }

    pub(crate) fn current_command_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<CommandTarget> {
        let target = self.current_command_target(kind);
        if target.is_some() || command != "new_tab" || kind != ResourceKind::Session {
            return target;
        }
        self.current_command_target(ResourceKind::Binding)
            .map(|binding| CommandTarget {
                kind,
                handle: serde_json::Value::from(vec!["no-session", binding.handle.as_str()])
                    .to_string(),
                generation: binding.generation,
            })
    }

    pub(crate) fn current_command_target(&self, kind: ResourceKind) -> Option<CommandTarget> {
        let (process, instance_generation, window_generation) = self.commands.target_identity();
        let process = process.to_owned();
        let window = &self.window_state_key;
        let scope = self.workspace.active.binding.scope();
        let binding_generation = self.workspace.active.binding.mux().binding_generation();
        let binding_handle = self.binding_target_handle(scope, binding_generation);
        let (session, mux_window, pane) = self.selected_mux_resource_path();
        let target = match kind {
            ResourceKind::Instance => CommandTarget {
                kind,
                handle: process,
                generation: instance_generation,
            },
            ResourceKind::ApplicationWindow => CommandTarget {
                kind,
                handle: serde_json::Value::from(vec![process.as_str(), window.as_str()])
                    .to_string(),
                generation: window_generation,
            },
            ResourceKind::Binding => CommandTarget {
                kind,
                handle: binding_handle,
                generation: binding_generation,
            },
            ResourceKind::Session => {
                let session = session?;
                CommandTarget {
                    kind,
                    handle: serde_json::Value::from(vec![
                        binding_handle.as_str(),
                        session.as_str(),
                    ])
                    .to_string(),
                    generation: self
                        .workspace
                        .active
                        .binding
                        .mux()
                        .session_generation(&session)?,
                }
            }
            ResourceKind::MuxWindow => {
                let (session, mux_window) = (session?, mux_window?);
                CommandTarget {
                    kind,
                    handle: serde_json::Value::from(vec![
                        binding_handle.as_str(),
                        session.as_str(),
                        mux_window.as_str(),
                    ])
                    .to_string(),
                    generation: self
                        .workspace
                        .active
                        .binding
                        .mux()
                        .window_generation(&session, &mux_window)?,
                }
            }
            ResourceKind::Pane => {
                let (session, mux_window, pane) = (session?, mux_window?, pane?);
                CommandTarget {
                    kind,
                    handle: serde_json::Value::from(vec![
                        binding_handle.as_str(),
                        session.as_str(),
                        mux_window.as_str(),
                        pane.as_str(),
                    ])
                    .to_string(),
                    generation: self.workspace.active.binding.mux().pane_generation(
                        &session,
                        &mux_window,
                        &pane,
                    )?,
                }
            }
            ResourceKind::Terminal => self.current_terminal_target(
                &binding_handle,
                binding_generation,
                (session, mux_window, pane),
            )?,
        };
        Some(target)
    }

    fn current_terminal_target(
        &self,
        binding_handle: &str,
        binding_generation: u64,
        path: (Option<String>, Option<String>, Option<String>),
    ) -> Option<CommandTarget> {
        let (handle, generation) = match path {
            (Some(session), Some(mux_window), Some(pane)) => (
                serde_json::Value::from(vec![
                    binding_handle,
                    session.as_str(),
                    mux_window.as_str(),
                    pane.as_str(),
                ])
                .to_string(),
                self.workspace.active.binding.mux().terminal_generation(
                    &session,
                    &mux_window,
                    &pane,
                )?,
            ),
            (Some(session), _, _) => (
                serde_json::Value::from(vec![binding_handle, session.as_str()]).to_string(),
                self.workspace
                    .active
                    .binding
                    .mux()
                    .session_generation(&session)?,
            ),
            (None, _, _) => (
                serde_json::Value::from(vec![binding_handle, "active_terminal"]).to_string(),
                binding_generation,
            ),
        };
        Some(CommandTarget {
            kind: ResourceKind::Terminal,
            handle,
            generation,
        })
    }

    pub(crate) fn selected_mux_resource_path(
        &self,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let Some(anchor) = self
            .workspace
            .active
            .binding
            .mux()
            .selected_session_anchor()
        else {
            return (None, None, None);
        };
        let session = anchor.session_id.clone();
        let mux_window = self
            .workspace
            .active
            .binding
            .mux()
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.workspace
                    .active
                    .binding
                    .mux()
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            });
        let pane = if self.uses_native_terminal_layout() {
            self.workspace
                .active
                .binding
                .terminal()
                .focused_pane_id()
                .map(|pane_id| {
                    decode_scoped_pane_id(pane_id).map_or_else(
                        || pane_id.to_owned(),
                        |(scope, pane_id)| {
                            debug_assert_eq!(scope, self.workspace.active.binding.scope());
                            pane_id
                        },
                    )
                })
        } else {
            anchor.pane_id.clone()
        };
        (Some(session), mux_window, pane)
    }

    fn read_active_terminal(&mut self) -> CommandOutcome {
        match self.workspace.active.binding.terminal_mut().extract_frame() {
            Ok(frame) => CommandOutcome::Success {
                value: serde_json::json!({
                    "cols": frame.cols,
                    "rows": frame.rows,
                    "text": frame.text_rows().join("\n"),
                    "cursor": frame.cursor.map(|cursor| serde_json::json!({
                        "x": cursor.x,
                        "y": cursor.y,
                    })),
                }),
                warnings: Vec::new(),
            },
            Err(error) => CommandOutcome::Failed {
                code: "terminal_read_failed".to_owned(),
                message: error.to_string(),
            },
        }
    }

    fn reload_config_command(&mut self, effects: &mut Vec<AppEffect>) -> CommandOutcome {
        let reloaded = self.reload_config(effects);
        if reloaded {
            self.last_error
                .clone()
                .map_or_else(CommandOutcome::success, |warning| {
                    CommandOutcome::success_with_warning(
                        "configuration_warning",
                        warning.raw_message(),
                    )
                })
        } else {
            CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message: self.last_error.clone().map_or_else(
                    || ErrorNotice::ConfigurationReloadFailed.to_string(),
                    |error| error.raw_message(),
                ),
            }
        }
    }

    fn terminal_input_command(
        &mut self,
        input: TerminalInputCommand,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        let previous_error = self.last_error.take();
        self.apply_terminal_input(input, effects);
        self.last_error.clone().map_or_else(
            || {
                self.last_error = previous_error;
                CommandOutcome::success()
            },
            |notice| CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message: notice.raw_message(),
            },
        )
    }

    fn dispatch_synchronous_command(
        &mut self,
        executor: SynchronousCommand,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(command_outcome_for_mux_error(error));
        }
        match executor {
            SynchronousCommand::ReloadConfig => {
                CommandDispatch::Complete(self.reload_config_command(effects))
            }
            SynchronousCommand::Sidebar(action) => {
                if self.apply_sidebar_action(action)
                    && matches!(
                        action,
                        crate::app_actions::SidebarAction::FocusTerminal
                            | crate::app_actions::SidebarAction::ActivateSession
                    )
                {
                    effects.push(AppEffect::FocusTerminal);
                }
                CommandDispatch::Complete(CommandOutcome::success())
            }
            SynchronousCommand::Command(action) => {
                if self.keymap_focus() != crate::keymap_runtime::KeymapFocus::Command {
                    return CommandDispatch::Complete(CommandOutcome::Unavailable {
                        message: "No command palette or picker is open.".to_owned(),
                    });
                }
                effects.push(AppEffect::CommandAction(action));
                CommandDispatch::Complete(CommandOutcome::success())
            }
            SynchronousCommand::CurrentResource(kind) => {
                let outcome = self.current_command_target(kind).map_or_else(
                    || CommandOutcome::Unavailable {
                        message: ErrorNotice::NoCurrentTarget(format!(
                            "no current {kind:?} target is available"
                        ))
                        .raw_message(),
                    },
                    |target| CommandOutcome::Success {
                        value: serde_json::json!({"target": target}),
                        warnings: Vec::new(),
                    },
                );
                CommandDispatch::Complete(outcome)
            }
            SynchronousCommand::Doctor => CommandDispatch::Complete(CommandOutcome::Success {
                value: self.doctor(),
                warnings: Vec::new(),
            }),
            SynchronousCommand::ShellIntegration(shell) => {
                let outcome = bootty_terminal::shell_integration::ShellIntegration::script(&shell)
                    .map_or_else(
                        || CommandOutcome::Unsupported {
                            message: "Shell integration supports bash, zsh and fish".to_owned(),
                        },
                        |script| CommandOutcome::Success {
                            value: serde_json::json!({"shell":shell,"script":script}),
                            warnings: Vec::new(),
                        },
                    );
                CommandDispatch::Complete(outcome)
            }
            SynchronousCommand::WslSpace(arguments) => {
                CommandDispatch::Complete(self.create_wsl_space(&arguments))
            }
            SynchronousCommand::ReadTerminal => {
                CommandDispatch::Complete(self.read_active_terminal())
            }
            SynchronousCommand::PasteTerminal(text) => CommandDispatch::Complete(
                self.terminal_input_command(TerminalInputCommand::Paste(text), effects),
            ),
            SynchronousCommand::SubmitTerminal => {
                CommandDispatch::Complete(self.terminal_input_command(
                    TerminalInputCommand::Key(KeyInput {
                        key: TerminalKey::Enter,
                        mods: KeyMods::default(),
                        repeat: false,
                        utf8: None,
                        unshifted: None,
                    }),
                    effects,
                ))
            }
        }
    }

    fn submit_authoritative_mux_command(
        &mut self,
        command: MuxCommand,
        membership: Option<Box<BindingMembershipMutation>>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> PendingCommandResult {
        let submitted = executor::submit_authoritative_command(
            &mut self.workspace,
            &self.repaint,
            command,
            membership,
            execution,
        );
        PendingCommandResult::Mux {
            scope: submitted.scope,
            command: submitted.command,
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        }
    }

    fn begin_authoritative_membership(
        &mut self,
        command: &MuxCommand,
    ) -> Result<Option<Box<BindingMembershipMutation>>, CommandOutcome> {
        executor::begin_authoritative_membership(&mut self.workspace, command).map_err(|error| {
            let outcome = CommandOutcome::Failed {
                code: "persistence_failed".to_owned(),
                message: error.to_string(),
            };
            if let Some(message) = command_outcome_message(&outcome) {
                self.record_error(message);
            }
            outcome
        })
    }

    pub(crate) fn prepare_ditch_session_command(
        &mut self,
        session_id: String,
    ) -> Result<(SpaceId, MuxCommand, Option<Box<BindingMembershipMutation>>), CommandOutcome> {
        let command = MuxCommand::DitchSession { session_id };
        let scope = self.workspace.active.binding.scope();
        if self
            .commands
            .pending
            .iter()
            .any(|pending| match &pending.result {
                PendingCommandResult::DitchCleanup {
                    scope: pending_scope,
                    command: pending_command,
                    ..
                }
                | PendingCommandResult::Mux {
                    scope: pending_scope,
                    command: pending_command,
                    ..
                } => *pending_scope == scope && *pending_command == command,
                PendingCommandResult::Outcome(_)
                | PendingCommandResult::Forward { .. }
                | PendingCommandResult::Clipboard { .. }
                | PendingCommandResult::Link { .. } => false,
            })
        {
            let outcome = CommandOutcome::Unavailable {
                message: "This session is already being closed".to_owned(),
            };
            self.record_error("This session is already being closed".to_owned());
            return Err(outcome);
        }
        if let Some(outcome) = self.preflight_mux_command(&command) {
            if let Some(message) = command_outcome_message(&outcome) {
                self.record_error(message);
            }
            return Err(outcome);
        }
        let membership = self.begin_authoritative_membership(&command)?;
        Ok((scope, command, membership))
    }

    pub(crate) fn submit_prepared_ditch_session_command(
        &mut self,
        (scope, command, membership): (SpaceId, MuxCommand, Option<Box<BindingMembershipMutation>>),
    ) {
        debug_assert_eq!(scope, self.workspace.active.binding.scope());
        let (deadline, cancellation) = executor::command_execution(None);
        let result = self.submit_authoritative_mux_command(
            command,
            membership,
            Some((deadline, cancellation.clone())),
        );
        self.commands.pending.push(PendingAppCommand {
            deadline,
            cancellation,
            response: None,
            result,
        });
    }

    pub(crate) fn submit_ditch_cleanup(
        &mut self,
        (scope, command, membership): (SpaceId, MuxCommand, Option<Box<BindingMembershipMutation>>),
        cwd: Option<String>,
        action: &crate::presentation::dialogs::DitchAction,
    ) {
        let action = crate::state::ditch::mux_ditch_action(action);
        let (deadline, cancellation) = executor::command_execution(None);
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        let cleanup_cancellation = cancellation.clone();
        std::thread::spawn(move || {
            let outcome = bootty_mux::workflow::run_ditch_cleanup_with_deadline(
                cwd.as_deref(),
                &action,
                deadline,
                cleanup_cancellation,
            );
            let _ = sender.send(outcome);
            repaint();
        });
        self.commands.pending.push(PendingAppCommand {
            deadline,
            cancellation,
            response: None,
            result: PendingCommandResult::DitchCleanup {
                scope,
                command,
                membership,
                result,
            },
        });
    }

    fn dispatch_resolved_keybind_command(
        &mut self,
        action: KeybindAction,
        planned_mux_command: Option<MuxCommand>,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let KeybindAction::OpenSetting(id) = &action
            && !self
                .settings_schema()
                .allows_write_path(&id.split('.').collect::<Vec<_>>())
        {
            return CommandDispatch::Complete(CommandOutcome::Failed {
                code: "unknown_setting".to_owned(),
                message: format!("Unknown setting: {id}"),
            });
        }
        if matches!(action, KeybindAction::Mux(MuxKeyAction::ClosePane))
            && let Some(policy) = self.last_surface_close_policy()
        {
            if let Err(error) = executor::begin_synchronous_command(execution) {
                return CommandDispatch::Complete(command_outcome_for_mux_error(error));
            }
            // This is a window policy, shared by every caller and backend. Other sessions
            // and Spaces must remain reachable when only the selected session becomes empty.
            let close_window = match policy {
                bootty_config::config::WhenClosingWithNoTabs::CloseWindow => true,
                bootty_config::config::WhenClosingWithNoTabs::KeepWindowOpen => false,
                bootty_config::config::WhenClosingWithNoTabs::PlatformDefault => {
                    !cfg!(target_os = "macos")
                }
            };
            if close_window {
                effects.push(AppEffect::CloseWindow);
            }
            effects.push(AppEffect::RequestRepaint);
            return CommandDispatch::Complete(CommandOutcome::success());
        }
        let mut return_native_mux_focus = false;
        // Every mailbox caller needs the authoritative result, including nested native-agent commands.
        if (execution.is_some() || matches!(caller, Caller::Cli | Caller::Socket | Caller::Luau))
            && let KeybindAction::Mux(mux_action) = action
        {
            let process_local_action = self
                .workspace
                .active
                .binding
                .backend_policy()
                .panes
                .topology
                == PaneTopology::ProcessLocal
                && Self::process_local_mux_action_uses_local_layout(mux_action);
            if process_local_action {
                return_native_mux_focus = true;
            } else if let Some(command) = planned_mux_command.clone() {
                let membership = match self.begin_authoritative_membership(&command) {
                    Ok(membership) => membership,
                    Err(outcome) => return CommandDispatch::Complete(outcome),
                };
                return CommandDispatch::Pending(
                    self.submit_authoritative_mux_command(command, membership, execution),
                );
            }
        }
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(command_outcome_for_mux_error(error));
        }
        let previous_error = self.last_error.take();
        self.apply_resolved_keybind_action(action, planned_mux_command, viewport, effects);
        let outcome = if let Some(notice) = self.last_error.clone() {
            CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message: notice.raw_message(),
            }
        } else {
            self.last_error = previous_error;
            if return_native_mux_focus {
                CommandOutcome::Success {
                    value: self.current_mux_focus_value(),
                    warnings: Vec::new(),
                }
            } else {
                CommandOutcome::success()
            }
        };
        CommandDispatch::Complete(outcome)
    }
    const fn process_local_mux_action_uses_local_layout(action: MuxKeyAction) -> bool {
        matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
                | MuxKeyAction::SplitPane(_)
                | MuxKeyAction::SelectPane(_)
                | MuxKeyAction::NextPane
                | MuxKeyAction::PreviousPane
                | MuxKeyAction::KillPane
                | MuxKeyAction::ClosePane
        )
    }

    fn current_mux_focus_value(&self) -> serde_json::Value {
        let focused = self
            .current_command_target(ResourceKind::Pane)
            .or_else(|| self.current_command_target(ResourceKind::MuxWindow))
            .or_else(|| self.current_command_target(ResourceKind::Session));
        focused.map_or_else(
            || serde_json::json!({}),
            |focused| serde_json::json!({ "focused": focused }),
        )
    }

    fn command_outcome_for_mux_result(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        membership: Option<&BindingMembershipMutation>,
        result: MuxCommandResult,
        layout: Option<&bootty_mux::workspace::PreparedPaneArrangement>,
    ) -> CommandOutcome {
        let completion = match executor::complete_authoritative_command(
            &mut self.workspace,
            scope,
            membership,
            result,
            layout,
        ) {
            Ok(completion) => completion,
            Err(error) => {
                let message = error.to_string();
                self.record_error(message.clone());
                return CommandOutcome::Failed {
                    code: "persistence_failed".to_owned(),
                    message,
                };
            }
        };
        let (completion, sync_error) = completion;
        if let Some(error) = sync_error {
            self.record_error(error);
        }
        match completion {
            Ok(completion) => {
                let Some(value) = self.mux_command_completion_value(scope, command, &completion)
                else {
                    let outcome = CommandOutcome::StaleTarget {
                        message: ErrorNotice::MuxOperationCapabilityStale.to_string(),
                    };
                    if let Some(message) = command_outcome_message(&outcome) {
                        self.record_error(message);
                    }
                    return outcome;
                };
                serialized_command_outcome(value)
            }
            Err(error) => {
                let message = error.to_string();
                let outcome = match error {
                    MuxCommandError::Cancelled => CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message,
                    },
                    MuxCommandError::DeadlineExceeded => CommandOutcome::Failed {
                        code: "deadline_exceeded".to_owned(),
                        message,
                    },
                    MuxCommandError::Unsupported => CommandOutcome::Unsupported {
                        message: ErrorNotice::MuxOperationUnsupported.to_string(),
                    },
                    MuxCommandError::Unavailable => CommandOutcome::Unavailable {
                        message: ErrorNotice::MuxOperationUnavailable.to_string(),
                    },
                    MuxCommandError::Stale => CommandOutcome::StaleTarget {
                        message: ErrorNotice::MuxOperationCapabilityStale.to_string(),
                    },
                    MuxCommandError::Failed(_) => CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message,
                    },
                };
                if let Some(message) = command_outcome_message(&outcome) {
                    self.record_error(message);
                }
                outcome
            }
        }
    }

    fn mux_command_completion_value(
        &self,
        scope: SpaceId,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
    ) -> Option<BTreeMap<String, CommandTarget>> {
        let mut value = BTreeMap::new();
        if let Some(session_id) = match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        } {
            value.insert(
                "created".to_owned(),
                self.mux_resource_target(scope, ResourceKind::Session, session_id, None)?,
            );
        }
        if let (Some(session_id), Some(window_id)) = (
            completion.selected_session.as_deref(),
            completion.selected_window.as_deref(),
        ) {
            value.insert(
                "focused".to_owned(),
                self.mux_resource_target(
                    scope,
                    ResourceKind::MuxWindow,
                    session_id,
                    Some(window_id),
                )?,
            );
            if matches!(command, MuxCommand::NewWindow { .. })
                && let Some(created) = self.mux_terminal_target(scope, session_id, window_id)
            {
                value.insert("created".to_owned(), created);
            }
        }
        if !value.contains_key("focused")
            && let Some(session_id) = completion.selected_session.as_deref()
        {
            value.insert(
                "focused".to_owned(),
                self.mux_resource_target(scope, ResourceKind::Session, session_id, None)?,
            );
        }
        Some(value)
    }

    pub(crate) fn mux_resource_target(
        &self,
        scope: SpaceId,
        kind: ResourceKind,
        session_id: &str,
        window_id: Option<&str>,
    ) -> Option<CommandTarget> {
        let binding_runtime = self.workspace.binding(scope)?;
        let binding = self.binding_target_handle(scope, binding_runtime.mux().binding_generation());
        let (handle, generation) = match kind {
            ResourceKind::Session => (
                serde_json::Value::from(vec![binding.as_str(), session_id]).to_string(),
                binding_runtime
                    .mux()
                    .session_generation(session_id)
                    .unwrap_or(1),
            ),
            ResourceKind::MuxWindow => {
                let window_id = window_id?;
                (
                    serde_json::Value::from(vec![binding.as_str(), session_id, window_id])
                        .to_string(),
                    binding_runtime
                        .mux()
                        .window_generation(session_id, window_id)
                        .unwrap_or(1),
                )
            }
            _ => return None,
        };
        Some(CommandTarget {
            kind,
            handle,
            generation,
        })
    }

    fn mux_terminal_target(
        &self,
        scope: SpaceId,
        session_id: &str,
        window_id: &str,
    ) -> Option<CommandTarget> {
        let binding_runtime = self.workspace.binding(scope)?;
        let pane_id = binding_runtime
            .mux()
            .sessions()
            .iter()
            .find(|session| session.id == session_id)?
            .windows
            .iter()
            .find(|window| window.id == window_id)?
            .anchor
            .pane_id
            .as_deref()?;
        let binding = self.binding_target_handle(scope, binding_runtime.mux().binding_generation());
        Some(CommandTarget {
            kind: ResourceKind::Terminal,
            handle: serde_json::Value::from(vec![binding.as_str(), session_id, window_id, pane_id])
                .to_string(),
            generation: binding_runtime
                .mux()
                .pane_generation(session_id, window_id, pane_id)?,
        })
    }

    pub(crate) fn binding_target_handle(&self, scope: SpaceId, generation: u64) -> String {
        let (process, _, window_generation) = self.commands.target_identity();
        serde_json::Value::Array(vec![
            process.into(),
            self.window_state_key.clone().into(),
            window_generation.into(),
            scope.persistence_value().to_string().into(),
            generation.into(),
        ])
        .to_string()
    }
}
