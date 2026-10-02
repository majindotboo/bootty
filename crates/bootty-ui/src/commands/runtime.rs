mod agents;
mod targets;
mod terminal_agents;

use agents::{AgentScopeIndex, AppCommandAgentExecutor};

mod capture;
mod clipboard;
mod files;
mod forwards;
mod git;
mod jobs;
mod links;
mod mux;
mod pane_input;
mod recovery;
mod sessions;
mod shell;
mod wsl;

use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    task::{Poll, ready},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    app_actions::{KeybindAction, MuxKeyAction},
    commands::{
        CommandCatalog, CommandExecutor, CoreCommandExecutor, ExactMuxTarget, SynchronousCommand,
    },
    error_catalog::ErrorNotice,
    state::{AppEffect, AppState, ViewportSnapshot},
};
use bootty_agents::{AgentCommandExecutor, AgentService, TerminalAgentService};
use bootty_control::{
    AppCommandReceiver, AppCommandSender, BoundAppCommandSender, Caller, CommandCancellation,
    CommandInvocation, CommandOutcome, CommandTarget, ControlEventSender, MutationClass,
    ResourceKind, app_command_channel,
};
use bootty_mux::repository::BindingMembershipMutation;
use bootty_mux::{
    RepaintHandle,
    command::MuxCommand,
    controller::{MuxCommandError, MuxCommandResult, SpaceId},
    executor,
    provider::PaneTopology,
    workspace::{StartingSession, WorkspaceRuntime},
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
    /// An explicit create that succeeded, held until the first pane Bootty started for it runs.
    SessionStart {
        starting: StartingSession,
        name: String,
        outcome: CommandOutcome,
    },
}

pub enum CommandDispatch {
    Complete(CommandOutcome),
    Pending(PendingCommandResult),
}

pub struct PendingAppCommand {
    pub(crate) label: String,
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
    terminal_agents: Option<Arc<TerminalAgentService>>,
    terminal_agent_error: Option<String>,
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
        if let Some(agents) = self.terminal_agents.take() {
            std::thread::spawn(move || agents.shutdown());
        }
    }
}

impl CommandRuntime {
    pub(crate) fn new(repaint: RepaintHandle) -> Self {
        let (sender, receiver) = app_command_channel(64, repaint);
        Self::from_channel(sender, receiver, None, None, None)
    }

    pub(crate) fn new_with_agents(
        repaint: RepaintHandle,
        events: ControlEventSender,
        agent_state: &std::path::Path,
    ) -> Self {
        let (sender, receiver) = app_command_channel(64, repaint.clone());
        let scope_index = Arc::new(AgentScopeIndex::default());
        let nested_commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            sender: sender.clone(),
        });
        let agents = Arc::new(
            AgentService::with_control_and_resolver(
                nested_commands,
                events.clone(),
                scope_index.clone(),
            )
            .persisted_at(agent_state),
        );
        let (terminals, terminal_error) =
            match TerminalAgentService::open(agent_state.with_extension("terminals.json")) {
                Ok(service) => (Some(Arc::new(service)), None),
                Err(mut error) => {
                    error.truncate(error.floor_char_boundary(4096));
                    (None, Some(error))
                }
            };
        if let Some(terminals) = &terminals {
            terminals.set_change_handler(repaint);
        }
        let mut runtime = Self::from_channel(
            sender,
            receiver,
            Some(agents),
            Some(scope_index),
            Some(events),
        );
        runtime.terminal_agents.clone_from(&terminals);
        runtime.terminal_agent_error = terminal_error;
        runtime.catalog = Arc::new(CommandCatalog::with_terminal_services(
            runtime.agent_service.clone(),
            terminals,
            Arc::downgrade(&runtime.jobs),
        ));
        runtime
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
            terminal_agents: None,
            terminal_agent_error: None,
            agent_scope_index,
            pending: Vec::new(),
            forwards: Vec::new(),
            jobs,
        }
    }

    pub(crate) fn refresh_agent_scopes(&self, workspace: &WorkspaceRuntime) {
        let owners_changed = self
            .agent_scope_index
            .as_ref()
            .is_some_and(|index| index.refresh(workspace));
        if let Some(agents) = &self.agent_service {
            // Records follow their panes to their owners before anything unowned is pruned.
            if owners_changed {
                agents.reattribute();
            }
            let (spaces, live) = agents::live_panes(workspace);
            agents.retain_live_panes(&spaces, &live);
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

    /// Cancel before work starts; once committed, publish the observed result. Atomic file writes,
    /// Git hooks, and job-registry mutations cannot report a fictitious rollback on cancellation.
    fn dispatch_committed_command(
        &self,
        execution: Option<(Instant, CommandCancellation)>,
        run: impl FnOnce() -> CommandOutcome + Send + 'static,
    ) -> CommandDispatch {
        let execution = executor::command_execution(execution);
        let (sender, receiver) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = match executor::begin_synchronous_command(Some(execution)) {
                Ok(()) => run(),
                Err(error) => command_outcome_for_mux_error(error),
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
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

    pub fn terminal_agent_service(&self) -> Option<Arc<TerminalAgentService>> {
        self.commands.terminal_agents.clone()
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
            let label = self
                .commands
                .catalog
                .describe(&request.invocation.command)
                .map_or_else(
                    || request.invocation.command.clone(),
                    |descriptor| descriptor.title,
                );
            let now = Instant::now();
            let dispatch = if request.cancellation.is_cancelled() {
                CommandDispatch::Complete(CommandOutcome::cancelled())
            } else if now >= request.deadline {
                let _ = request.cancellation.cancel();
                CommandDispatch::Complete(CommandOutcome::deadline_exceeded())
            } else {
                // Like a pending result with a response channel, a mailbox caller receives its
                // failure and reports it; it must not also raise a window notification the person
                // at the window never asked for.
                let previous_error = self.last_error.clone();
                let dispatch = self.dispatch_command_with_execution(
                    request.invocation,
                    viewport,
                    effects,
                    Some((request.deadline, request.cancellation.clone())),
                );
                self.last_error = previous_error;
                dispatch
            };
            match dispatch {
                CommandDispatch::Complete(outcome) => {
                    let _ = request.response.send(outcome);
                }
                CommandDispatch::Pending(result) => {
                    self.commands.pending.push(PendingAppCommand {
                        label,
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
            // A caller with a response channel reports its own failure, including one that
            // completes later; converting its result must not leave a window notification.
            let previous_error = pending.response.is_some().then(|| self.last_error.clone());
            let polled = self.poll_pending_app_command(&mut pending, now, effects);
            if let Some(previous_error) = previous_error {
                self.last_error = previous_error;
            }
            match polled {
                Poll::Pending => self.commands.pending.push(pending),
                Poll::Ready(None) => {}
                Poll::Ready(Some(outcome)) => {
                    if let Some(response) = pending.response {
                        let _ = response.send(outcome);
                    } else if matches!(&outcome, CommandOutcome::Failed { code, .. } if code == "deadline_exceeded")
                    {
                        self.record_error(format!("{} took too long. Try again.", pending.label));
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
            } => ready!(self.poll_pending_mux(
                *scope,
                command,
                membership.as_deref(),
                layout.as_ref(),
                result
            )),
            PendingCommandResult::Outcome(result) => match ready!(poll_command_result(result)) {
                Ok(outcome) => outcome,
                Err(mpsc::RecvError) => CommandOutcome::Failed {
                    code: "command_worker_stopped".to_owned(),
                    message: ErrorNotice::CommandWorkerStopped.to_string(),
                },
            },
            PendingCommandResult::SessionStart {
                starting,
                name,
                outcome,
            } => ready!(self.poll_session_start(starting, name, outcome)),
        };
        Poll::Ready(Some(outcome))
    }

    /// The outcome of a mux command still running on its backend's worker.
    fn poll_pending_mux(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        membership: Option<&BindingMembershipMutation>,
        layout: Option<&bootty_mux::workspace::PreparedPaneArrangement>,
        result: &mpsc::Receiver<MuxCommandResult>,
    ) -> Poll<CommandOutcome> {
        let Ok(result) = ready!(poll_command_result(result)) else {
            if membership.is_some() {
                self.workspace
                    .defer_binding_membership_reconciliation(scope);
            }
            return Poll::Ready(CommandOutcome::Failed {
                code: "backend_worker_stopped".to_owned(),
                message: ErrorNotice::MuxCommandWorkerStopped.to_string(),
            });
        };
        Poll::Ready(self.command_outcome_for_mux_result(scope, command, membership, result, layout))
    }

    pub(crate) fn dispatch_command(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        let label = self
            .commands
            .catalog
            .describe(&invocation.command)
            .map_or_else(|| invocation.command.clone(), |descriptor| descriptor.title);
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
            || invocation.command.starts_with("session.")
            || invocation.command == "link.open";
        let (deadline, cancellation) = executor::command_execution(None);
        let execution = asynchronous.then(|| (deadline, cancellation.clone()));
        match self.dispatch_command_with_execution(invocation, viewport, effects, execution) {
            CommandDispatch::Complete(outcome) => outcome,
            CommandDispatch::Pending(result) => {
                self.commands.pending.push(PendingAppCommand {
                    label,
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
        // A terminal command naming its own target writes to that pane through the mux, in any
        // Space, and leaves selection and focus alone.
        if target_supplied
            && let Some(input) = pane_input::targeted_pane_input(&resolved.executor)
            && let Some(exact) = exact_target.as_ref()
        {
            return self.dispatch_pane_input(exact, input, execution);
        }
        let planned_mux_command =
            match self.preflight_resolved_invocation(&resolved, exact_target.as_ref(), effects) {
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
            CommandExecutor::TerminalAgent => self.dispatch_terminal_agent(
                context.invocation,
                context.exact_target.as_ref(),
                execution,
            ),
            CommandExecutor::UncomposedAgent => {
                if let Some(error) = &self.commands.terminal_agent_error {
                    return CommandDispatch::Complete(CommandOutcome::Unavailable {
                        message: format!("Terminal agent owner could not start: {error}"),
                    });
                }
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
        let target_scope = exact_target.as_ref().map(ExactMuxTarget::scope);
        let scope = target_scope.unwrap_or_else(|| self.mux_scope());
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
            CoreCommandExecutor::Forward(action, arguments) => {
                self.dispatch_forward(action, &arguments, target_scope, execution)
            }
            CoreCommandExecutor::Job(action, arguments) => {
                self.dispatch_job_command(action, arguments, target_scope, execution)
            }
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
            CoreCommandExecutor::Session(action, arguments) => {
                self.dispatch_session_command(action, &arguments, exact_target, execution)
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

    fn preflight_resolved_invocation(
        &mut self,
        resolved: &crate::commands::ResolvedCommandInvocation,
        exact_target: Option<&ExactMuxTarget>,
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
        Ok(planned_mux_command)
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
}
