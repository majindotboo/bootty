mod targets;

mod capture;
mod clipboard;
mod files;
mod forwards;
mod jobs;
mod links;
mod mux;
mod names;
mod pane_input;
mod recovery;
mod sessions;
mod shell;
mod surfaces;
mod wsl;

use std::{
    collections::HashSet,
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
    commands::{CommandCatalog, CoreCommandExecutor, ExactMuxTarget, SynchronousCommand},
    error_catalog::ErrorNotice,
    state::{AppEffect, AppState, ViewportSnapshot},
};
use bootty_control::{
    AppCommandReceiver, AppCommandSender, BoundAppCommandSender, Caller, CommandCancellation,
    CommandInvocation, CommandOutcome, CommandTarget, ControlEventSender, MutationClass,
    ResourceKind, app_command_channel,
};
use bootty_mux::repository::BindingMembershipMutation;
use bootty_mux::{
    RepaintHandle,
    command::MuxCommand,
    controller::{CommandSelection, MuxCommandError, MuxCommandResult, SpaceId},
    executor,
    workspace::StartingSession,
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
        MuxCommandError::Unavailable => CommandOutcome::Unavailable {
            message: ErrorNotice::MuxOperationUnavailable.to_string(),
        },
        MuxCommandError::Stale => CommandOutcome::StaleTarget {
            message: ErrorNotice::StaleCommandTarget("mux target is stale".to_owned()).to_string(),
        },
        MuxCommandError::LaunchFailed(message) => CommandOutcome::Failed {
            code: "session_start_failed".to_owned(),
            message,
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
}

pub enum PendingCommandResult {
    SessionCheckpointCompletion {
        target: CommandTarget,
        identity: String,
        captured: Option<crate::state::SessionCheckpointTicket>,
        outcome: Box<CommandOutcome>,
    },
    SurfaceCreation {
        request_id: u64,
        command: Box<PendingAppCommand>,
    },
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
    Mux {
        scope: SpaceId,
        command: Box<MuxCommand>,
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
    pub(crate) creation_receipt: Option<mpsc::Sender<CommandTarget>>,
    pub(crate) label: String,
    pub(crate) deadline: Instant,
    pub(crate) cancellation: CommandCancellation,
    pub(crate) response: Option<mpsc::Sender<CommandOutcome>>,
    pub(crate) result: PendingCommandResult,
}

impl PendingAppCommand {
    fn cancellation_outcome(&self, now: Instant) -> Option<CommandOutcome> {
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

fn publish_creation_receipt(
    receipt: &mut Option<mpsc::Sender<CommandTarget>>,
    outcome: &CommandOutcome,
) {
    let CommandOutcome::Success { value, .. } = outcome else {
        return;
    };
    let target = value
        .get("terminal")
        .or_else(|| value.get("created"))
        .and_then(|target| serde_json::from_value::<CommandTarget>(target.clone()).ok())
        .filter(|target| target.kind == ResourceKind::Terminal);
    if let Some(target) = target
        && let Some(receipt) = receipt.take()
    {
        let _ = receipt.send(target);
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
    pending: Vec<PendingAppCommand>,
    /// The title of the command being dispatched or polled, so a failure it records names it.
    title: Option<String>,
    jobs: Arc<bootty_host::jobs::JobRegistry>,
    forwards: Vec<(SpaceId, u64, Arc<bootty_host::ssh_forward::ForwardLease>)>,
}

impl Drop for CommandRuntime {
    fn drop(&mut self) {
        self.jobs.retire();
        for pending in &self.pending {
            let _ = pending.cancellation.cancel();
        }
    }
}

impl CommandRuntime {
    /// A runtime whose job changes publish to `events` when the host supplies them.
    pub(crate) fn new(repaint: RepaintHandle, events: Option<ControlEventSender>) -> Self {
        let (sender, receiver) = app_command_channel(64, repaint);
        let publishes_jobs = events.is_some();
        let jobs = jobs::registry(events);
        Self {
            instance_handle: process_handle(),
            instance_generation: 1,
            window_generation: NEXT_WINDOW_GENERATION.fetch_add(1, Ordering::Relaxed),
            queued: None,
            sender,
            receiver,
            catalog: Arc::new(CommandCatalog::with_jobs(if publishes_jobs {
                Arc::downgrade(&jobs)
            } else {
                std::sync::Weak::new()
            })),
            pending: Vec::new(),
            title: None,
            forwards: Vec::new(),
            jobs,
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
    pub(crate) fn pane_close_pending(&self, pane: &str) -> bool {
        self.commands.pending.iter().any(|pending| {
            matches!(&pending.result, PendingCommandResult::Mux { scope, command, .. }
                if *scope == self.mux_scope() && matches!(command.as_ref(),
                    MuxCommand::ClosePane { pane_id: Some(closing), .. }
                    | MuxCommand::KillPane { pane_id: Some(closing), .. } if closing == pane))
        })
    }

    pub(crate) fn sync_pending_session_completion_scopes(&mut self) {
        let mut pending = std::mem::take(&mut self.commands.pending);
        let mut scopes = HashSet::new();
        for command in &mut pending {
            self.collect_pending_session_completion_scopes(&mut command.result, &mut scopes);
        }
        self.commands.pending = pending;
        self.workspace.set_pending_session_completion_scopes(scopes);
    }

    fn collect_pending_session_completion_scopes(
        &self,
        pending: &mut PendingCommandResult,
        scopes: &mut HashSet<SpaceId>,
    ) {
        match pending {
            PendingCommandResult::Mux { scope, .. } => {
                scopes.insert(*scope);
            }
            PendingCommandResult::SessionStart { starting, .. } => {
                scopes.insert(starting.scope());
            }
            PendingCommandResult::SessionCheckpointCompletion { target, .. } => {
                if let Ok((_, Some(exact))) = self.resolve_command_target(
                    "session.close",
                    Some(ResourceKind::Session),
                    Some(target),
                ) {
                    scopes.insert(exact.scope());
                }
            }
            PendingCommandResult::SurfaceCreation { command, .. } => {
                self.collect_pending_session_completion_scopes(&mut command.result, scopes);
            }
            PendingCommandResult::Forward { .. }
            | PendingCommandResult::Link { .. }
            | PendingCommandResult::Clipboard { .. }
            | PendingCommandResult::Outcome(_) => {}
        }
    }

    fn reject_command(&mut self, outcome: CommandOutcome) -> CommandDispatch {
        if let Some(message) = command_outcome_message(&outcome) {
            self.record_command_failure(&message);
        }
        CommandDispatch::Complete(outcome)
    }

    /// Records a failure of the command being dispatched or polled, named by its title.
    pub(super) fn record_command_failure(&mut self, message: &dyn std::fmt::Display) {
        let action = self.commands.title.as_ref().map_or_else(
            || "The command failed".to_owned(),
            |title| format!("{title} failed"),
        );
        self.record_error(&action, message);
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

    pub(crate) fn drain_app_commands(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        self.drain_pending_app_commands(Instant::now(), effects);
        let mut drained = 0_usize;
        for _ in 0..32 {
            let mut request = match self.commands.receiver.try_recv() {
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
                    CommandSelection::Follow,
                );
                self.last_error = previous_error;
                dispatch
            };
            match dispatch {
                CommandDispatch::Complete(outcome) => {
                    publish_creation_receipt(&mut request.creation_receipt, &outcome);
                    let _ = request.response.send(outcome);
                }
                CommandDispatch::Pending(result) => {
                    self.commands.pending.push(PendingAppCommand {
                        creation_receipt: request.creation_receipt,
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
            let focus_tab = pending.response.is_none()
                && matches!(self.modal_dialog(), Some(crate::state::ModalDialog::NewSession(dialog))
                    if dialog.is_creation_form())
                && matches!(&pending.result, PendingCommandResult::Mux { scope, command, .. }
                    if *scope == self.mux_scope() && matches!(command.as_ref(),
                        MuxCommand::ActivateWindow { .. } | MuxCommand::ActivateWindowIndex { .. }
                            | MuxCommand::ActivateLastWindow { .. }));
            // A caller with a response channel reports its own failure, including one that
            // completes later; converting its result must not leave a window notification.
            let previous_error = pending.response.is_some().then(|| self.last_error.clone());
            let title = self.commands.title.replace(pending.label.clone());
            let polled = self.poll_pending_app_command(&mut pending, now, effects);
            self.commands.title = title;
            if let Some(previous_error) = previous_error {
                self.last_error = previous_error;
            }
            match polled {
                Poll::Pending => self.commands.pending.push(pending),
                Poll::Ready(None) => {}
                Poll::Ready(Some(outcome)) => {
                    if focus_tab && matches!(outcome, CommandOutcome::Success { .. }) {
                        effects.push(AppEffect::FocusTerminal);
                    }
                    if let Some(response) = pending.response {
                        let _ = response.send(outcome);
                    } else if matches!(&outcome, CommandOutcome::Failed { code, .. } if code == "deadline_exceeded")
                    {
                        self.record_message(format!("{} took too long. Try again.", pending.label));
                    } else if let Some(message) = command_outcome_message(&outcome) {
                        self.record_error(&format!("{} failed", pending.label), &message);
                    }
                }
            }
        }
    }

    /// A dispatch's outcome now, or the command it continues as, kept pending.
    fn settle(
        pending: &mut PendingAppCommand,
        dispatch: CommandDispatch,
    ) -> Poll<Option<CommandOutcome>> {
        match dispatch {
            CommandDispatch::Complete(outcome) => Poll::Ready(Some(outcome)),
            CommandDispatch::Pending(result) => {
                pending.result = result;
                Poll::Pending
            }
        }
    }

    /// Release what a cancelled command still holds, and the outcome to report for it.
    fn cancel_pending(
        &mut self,
        pending: &PendingAppCommand,
        outcome: CommandOutcome,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        if let PendingCommandResult::SurfaceCreation { request_id, .. } = &pending.result
            && let Err(error) = self.finish_surface_creation(*request_id, &outcome, effects)
        {
            return error;
        }
        outcome
    }

    fn poll_pending_app_command(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        if let Some(outcome) = pending.cancellation_outcome(now) {
            return Poll::Ready(Some(self.cancel_pending(pending, outcome, effects)));
        }
        if let PendingCommandResult::SessionStart {
            starting,
            name,
            outcome,
        } = &mut pending.result
        {
            let outcome = ready!(self.poll_session_start(starting, name, outcome));
            let dispatch = self.hold_until_session_checkpoint(
                starting.scope(),
                starting.session_id(),
                outcome,
            );
            return Self::settle(pending, dispatch);
        }
        if let PendingCommandResult::SessionCheckpointCompletion {
            target,
            identity,
            captured,
            outcome,
        } = &mut pending.result
        {
            publish_creation_receipt(&mut pending.creation_receipt, outcome);
            return self
                .poll_session_checkpoint_completion(target, identity, captured, outcome)
                .map(Some);
        }
        self.poll_started_pending_command(pending, now, effects)
    }

    /// Opens a link once it resolves, unless the caller cancelled first.
    fn finish_link(
        &mut self,
        link: Result<Result<links::ResolvedLink, String>, mpsc::RecvError>,
        (scope, target): (SpaceId, &CommandTarget),
        cancellation: &CommandCancellation,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        match link {
            Ok(Ok(link)) if cancellation.try_start() => {
                self.finish_link_open(scope, target, link, effects)
            }
            Ok(Ok(_)) => CommandOutcome::cancelled(),
            Ok(Err(message)) => links::failure(message),
            Err(mpsc::RecvError) => links::failure("link worker stopped".to_owned()),
        }
    }

    /// Pastes clipboard text once its worker read it, unless the caller cancelled first.
    fn finish_clipboard(
        &mut self,
        text: Result<Result<String, String>, mpsc::RecvError>,
        scope: SpaceId,
        target: &CommandTarget,
        cancellation: &CommandCancellation,
    ) -> CommandOutcome {
        match text {
            Ok(Ok(text)) if cancellation.try_start() => {
                self.finish_clipboard_paste(scope, target, &text)
            }
            Ok(Ok(_)) => CommandOutcome::cancelled(),
            Ok(Err(message)) => clipboard::upload_failure(message),
            Err(mpsc::RecvError) => {
                clipboard::upload_failure("clipboard worker stopped".to_owned())
            }
        }
    }

    fn poll_started_pending_command(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        let outcome = match &mut pending.result {
            PendingCommandResult::SessionStart { .. }
            | PendingCommandResult::SessionCheckpointCompletion { .. } => {
                return self.poll_pending_app_command(pending, now, effects);
            }
            PendingCommandResult::SurfaceCreation { .. } => {
                return self.poll_surface_creation(pending, now, effects);
            }
            PendingCommandResult::Forward { result } => {
                let forward = ready!(poll_command_result(result));
                self.finish_forward_result(forward)
            }
            PendingCommandResult::Link {
                scope,
                target,
                result,
            } => {
                let link = ready!(poll_command_result(result));
                self.finish_link(link, (*scope, target), &pending.cancellation, effects)
            }
            PendingCommandResult::Clipboard {
                scope,
                target,
                result,
            } => {
                let text = ready!(poll_command_result(result));
                self.finish_clipboard(text, *scope, target, &pending.cancellation)
            }
            PendingCommandResult::Mux {
                scope,
                command,
                membership,
                result,
            } => {
                let dispatch = ready!(self.poll_pending_mux(
                    *scope,
                    command,
                    membership.as_deref(),
                    result,
                    &mut pending.creation_receipt
                ));
                return Self::settle(pending, dispatch);
            }
            PendingCommandResult::Outcome(result) => ready!(poll_command_result(result))
                .unwrap_or_else(|mpsc::RecvError| CommandOutcome::Failed {
                    code: "command_worker_stopped".to_owned(),
                    message: ErrorNotice::CommandWorkerStopped.to_string(),
                }),
        };
        Poll::Ready(Some(outcome))
    }

    /// The outcome of a mux command still running on its backend's worker.
    fn poll_pending_mux(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        membership: Option<&BindingMembershipMutation>,
        result: &mpsc::Receiver<MuxCommandResult>,
        creation_receipt: &mut Option<mpsc::Sender<CommandTarget>>,
    ) -> Poll<CommandDispatch> {
        let Ok(result) = ready!(poll_command_result(result)) else {
            return Poll::Ready(CommandDispatch::Complete(CommandOutcome::Failed {
                code: "backend_worker_stopped".to_owned(),
                message: ErrorNotice::MuxCommandWorkerStopped.to_string(),
            }));
        };
        let outcome = self.command_outcome_for_mux_result(scope, command, membership, result);
        publish_creation_receipt(creation_receipt, &outcome);
        Poll::Ready(self.hold_until_session_starts(scope, command, outcome))
    }

    pub(crate) fn dispatch_command(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        self.dispatch_command_with_selection(
            invocation,
            viewport,
            effects,
            CommandSelection::Follow,
        )
    }

    pub(crate) fn dispatch_command_with_selection(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        selection: CommandSelection,
    ) -> CommandOutcome {
        let label = self
            .commands
            .catalog
            .describe(&invocation.command)
            .map_or_else(|| invocation.command.clone(), |descriptor| descriptor.title);
        let asynchronous = crate::action_catalog::Command::from_action(&invocation.command)
            .and_then(crate::commands::DockAction::from_command)
            .is_some()
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
        match self
            .dispatch_command_with_execution(invocation, viewport, effects, execution, selection)
        {
            CommandDispatch::Complete(outcome) => outcome,
            CommandDispatch::Pending(result) => {
                self.commands.pending.push(PendingAppCommand {
                    creation_receipt: None,
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

    fn dispatch_creation_close(
        &mut self,
        command: &crate::commands::ResolvedCommandInvocation,
        target_supplied: bool,
        execution: Option<&(Instant, CommandCancellation)>,
        effects: &mut Vec<AppEffect>,
    ) -> Option<CommandDispatch> {
        // Closing an unsubmitted composer must never close the pane behind it.
        if target_supplied
            || !matches!(
                command.invocation.caller,
                Caller::Keybinding | Caller::BuiltinKeybinding | Caller::CommandPalette
            )
            || !matches!(
                command.executor,
                CoreCommandExecutor::Keybind(KeybindAction::Mux(MuxKeyAction::ClosePane))
            )
            || !matches!(
                self.modal_dialog(),
                Some(crate::state::ModalDialog::NewSession(_))
            )
        {
            return None;
        }
        if let Err(error) = executor::begin_synchronous_command(execution.cloned()) {
            return Some(CommandDispatch::Complete(command_outcome_for_mux_error(
                error,
            )));
        }
        // The empty workspace keeps its creation surface; dismissing it would leave no tab.
        if self.mux().selected_session_anchor().is_some() {
            self.dismiss_session_creation();
            if self.modal_dialog().is_none() {
                effects.push(AppEffect::FocusTerminal);
            }
        }
        effects.push(AppEffect::RequestRepaint);
        Some(CommandDispatch::Complete(CommandOutcome::success()))
    }

    fn dispatch_command_with_execution(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
        _selection: CommandSelection,
    ) -> CommandDispatch {
        let title = self
            .commands
            .catalog
            .describe(&invocation.command)
            .map_or_else(|| invocation.command.clone(), |descriptor| descriptor.title);
        let outer = self.commands.title.replace(title);
        let dispatch = match self.named_target(invocation) {
            Ok(invocation) => {
                self.dispatch_titled_command(invocation, viewport, effects, execution)
            }
            Err(outcome) => self.reject_command(outcome),
        };
        self.commands.title = outer;

        dispatch
    }

    fn dispatch_titled_command(
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
        if let Some(dispatch) =
            self.dispatch_creation_close(&resolved, target_supplied, execution.as_ref(), effects)
        {
            return dispatch;
        }
        if let CoreCommandExecutor::Keybind(KeybindAction::Mux(action)) = &resolved.executor {
            let placement = match action {
                MuxKeyAction::NewTab => Some(crate::surface_creation::SurfacePlacement::Tab),
                MuxKeyAction::SplitPane(direction) => {
                    Some(crate::surface_creation::SurfacePlacement::Split(*direction))
                }
                _ => None,
            };
            if let Some(placement) = placement {
                return self.open_new_surface(
                    &resolved.invocation,
                    placement,
                    viewport,
                    effects,
                    execution,
                );
            }
        }
        let (target, exact_target) = match self.resolve_invocation_target(&resolved) {
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
            return self.dispatch_pane_input(exact, &input, execution);
        }
        if target_supplied
            && matches!(
                resolved.executor,
                CoreCommandExecutor::Synchronous(SynchronousCommand::ReadTerminal)
            )
            && let Some(exact) = exact_target.as_ref()
        {
            return self.dispatch_targeted_terminal_read(exact, execution);
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
        };
        self.dispatch_core_command(resolved.executor, context, viewport, effects, execution)
    }

    fn resolve_invocation_target(
        &self,
        resolved: &crate::commands::ResolvedCommandInvocation,
    ) -> Result<(Option<CommandTarget>, Option<ExactMuxTarget>), CommandOutcome> {
        self.resolve_command_target(
            &resolved.invocation.command,
            resolved.descriptor.target,
            resolved.invocation.target.as_ref(),
        )
    }

    fn dispatch_core_command(
        &mut self,
        executor: CoreCommandExecutor,
        context: ResolvedCommandContext,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let target_scope = context.exact_target.as_ref().map(ExactMuxTarget::scope);
        match executor {
            CoreCommandExecutor::Recovery(_, _) => {
                self.dispatch_recovery(&context.invocation, execution)
            }
            CoreCommandExecutor::ShellPrompt(action, arguments) => self
                .dispatch_attached_shell_prompt(
                    context.exact_target,
                    action,
                    &arguments,
                    execution,
                ),
            CoreCommandExecutor::Forward(action, arguments) => {
                self.dispatch_forward(action, &arguments, target_scope, execution)
            }
            CoreCommandExecutor::Job(action, arguments) => {
                self.dispatch_job_command(action, arguments, target_scope, execution)
            }
            CoreCommandExecutor::Theme(action, arguments) => {
                self.dispatch_theme_command(action, &arguments, execution, effects)
            }
            CoreCommandExecutor::CaptureTerminal(arguments, export) => {
                self.dispatch_captured_terminal(context, &arguments, export, execution)
            }
            CoreCommandExecutor::WslList => self.dispatch_wsl_list(execution),
            CoreCommandExecutor::OpenLink(arguments) => {
                self.dispatch_attached_link(context, &arguments, execution)
            }
            CoreCommandExecutor::Pane(action, arguments) => {
                self.dispatch_pane_command(action, &arguments, context.exact_target, execution)
            }
            CoreCommandExecutor::Session(action, arguments) => {
                self.dispatch_session_with_caller(action, &arguments, context, execution)
            }
            CoreCommandExecutor::File(action, arguments) => self.dispatch_file_action(
                target_scope.unwrap_or_else(|| self.mux_scope()),
                action,
                &arguments,
                effects,
                execution,
            ),
            CoreCommandExecutor::Synchronous(executor) => {
                self.dispatch_synchronous_command(executor, effects, execution)
            }
            CoreCommandExecutor::Keybind(action) => {
                self.dispatch_keybind_executor(action, context, viewport, effects, execution)
            }
            CoreCommandExecutor::Dock(action, group) => {
                Self::dispatch_dock_command(action, group, effects, execution)
            }
        }
    }

    /// Keybind actions that act on the exact target first, then every other resolved keybind.
    fn dispatch_keybind_executor(
        &mut self,
        action: KeybindAction,
        context: ResolvedCommandContext,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let exact_target = context.exact_target.as_ref();
        let invocation = context.invocation;
        match action {
            KeybindAction::PasteFromClipboard => self.dispatch_attached_clipboard(
                exact_target.map_or_else(|| self.mux_scope(), ExactMuxTarget::scope),
                invocation.target,
                execution,
            ),
            action => self.dispatch_resolved_keybind_command(
                action,
                context.planned_mux_command,
                invocation.caller,
                viewport,
                effects,
                execution,
            ),
        }
    }

    fn dispatch_attached_shell_prompt(
        &mut self,
        exact: Option<ExactMuxTarget>,
        action: &'static str,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        exact.map_or_else(
            || {
                CommandDispatch::Complete(CommandOutcome::Unavailable {
                    message: "No terminal prompt is available".to_owned(),
                })
            },
            |exact| self.dispatch_shell_prompt(&exact, action, arguments, execution),
        )
    }

    fn dispatch_session_with_caller(
        &mut self,
        action: crate::commands::SessionAction,
        arguments: &[String],
        context: ResolvedCommandContext,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (target, caller) = (context.exact_target, context.invocation.caller);
        if matches!(action, crate::commands::SessionAction::Activity) && caller != Caller::Internal
        {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "Session activity requires an accepted host input receipt".to_owned(),
            });
        }
        self.dispatch_session_command(action, arguments, target, execution)
    }

    fn dispatch_attached_clipboard(
        &mut self,
        scope: SpaceId,
        target: Option<CommandTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(target) = target else {
            return self.reject_command(CommandOutcome::Unavailable {
                message: "clipboard paste requires a terminal".to_owned(),
            });
        };
        self.dispatch_clipboard_paste(scope, target, execution)
    }

    fn dispatch_attached_link(
        &mut self,
        context: ResolvedCommandContext,
        arguments: &[String],
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (Some(exact), Some(target)) = (context.exact_target, context.invocation.target) else {
            return self
                .reject_command(links::failure("Link needs an attached terminal".to_owned()));
        };
        self.dispatch_link_open(target, &exact, arguments, execution)
    }

    fn dispatch_captured_terminal(
        &mut self,
        context: ResolvedCommandContext,
        arguments: &[String],
        export: bool,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (Some(exact), Some(target)) = (context.exact_target, context.invocation.target) else {
            return self.reject_command(CommandOutcome::Unavailable {
                message: "Capture needs an attached terminal".to_owned(),
            });
        };
        self.dispatch_terminal_capture(&exact, target, arguments, export, execution)
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
            CoreCommandExecutor::Keybind(KeybindAction::Mux(action)) => {
                self.plan_mux_key_action(*action, exact_target)
            }
            _ => None,
        };
        let arranging = matches!(&resolved.executor, CoreCommandExecutor::Pane(..));
        let scope = exact_target.map_or_else(|| self.mux_scope(), ExactMuxTarget::scope);
        if (arranging || planned_mux_command.is_some())
            && self.commands.pending.iter().any(|pending| {
                matches!(&pending.result, PendingCommandResult::Mux { scope: pending_scope, command, .. }
                if *pending_scope == scope && (arranging || command.arranges_panes()))
            })
        {
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
                let applied = self.apply_sidebar_action(action, effects);
                if !applied
                    && matches!(
                        action,
                        crate::app_actions::SidebarAction::ToggleGrouping
                            | crate::app_actions::SidebarAction::SortManual
                            | crate::app_actions::SidebarAction::SortRecentActivity
                    )
                {
                    return CommandDispatch::Complete(CommandOutcome::Failed {
                        code: "configuration_save_failed".to_owned(),
                        message: self.last_error.as_ref().map_or_else(
                            || "Could not save the session list view.".to_owned(),
                            ErrorNotice::raw_message,
                        ),
                    });
                }
                if applied
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
            && self.closes_window_with_last_pane()
        {
            if let Err(error) = executor::begin_synchronous_command(execution) {
                return CommandDispatch::Complete(command_outcome_for_mux_error(error));
            }
            effects.push(AppEffect::CloseWindow);
            effects.push(AppEffect::RequestRepaint);
            return CommandDispatch::Complete(CommandOutcome::success());
        }
        if matches!(
            action,
            KeybindAction::Mux(MuxKeyAction::ClosePane | MuxKeyAction::KillPane)
        ) && let Some(
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane),
            }
            | MuxCommand::KillPane {
                session_id,
                pane_id: Some(pane),
            },
        ) = &planned_mux_command
        {
            return self.dispatch_planned_pane_close(session_id, pane, execution);
        }
        let gui_tab_navigation =
            execution.is_none() && self.navigates_creation_form_tabs(&action, caller);
        if moves_terminal_focus(&action) && !gui_tab_navigation {
            effects.push(AppEffect::FocusTerminal);
        }
        let mut return_native_mux_focus = false;
        // Every invocation waits for the same server result; presentation-only session
        // navigation and creation-form tabs remain local UI policy.
        if let KeybindAction::Mux(mux_action) = action {
            let process_local_action = Self::process_local_mux_action_uses_local_layout(mux_action);
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
        CommandDispatch::Complete(self.apply_keybind_now(
            action,
            planned_mux_command,
            viewport,
            effects,
            return_native_mux_focus,
        ))
    }

    /// A tab key typed in the GUI while the new session dialog shows its creation form moves
    /// between the form's tabs instead of the terminal's.
    fn navigates_creation_form_tabs(&self, action: &KeybindAction, caller: Caller) -> bool {
        matches!(self.modal_dialog(), Some(crate::state::ModalDialog::NewSession(dialog))
            if dialog.is_creation_form())
            && !matches!(caller, Caller::Cli | Caller::Socket | Caller::Luau)
            && matches!(
                action,
                KeybindAction::Mux(
                    MuxKeyAction::NextTab
                        | MuxKeyAction::PreviousTab
                        | MuxKeyAction::LastTab
                        | MuxKeyAction::SelectTab(_)
                )
            )
    }

    /// Whether closing the last pane closes the window. This is a window policy, shared by every
    /// caller and backend; other sessions and Spaces stay reachable when only the selected
    /// session becomes empty.
    fn closes_window_with_last_pane(&self) -> bool {
        self.last_surface_close_policy()
            .is_some_and(|policy| match policy {
                bootty_config::config::WhenClosingWithNoTabs::CloseWindow => true,
                bootty_config::config::WhenClosingWithNoTabs::KeepWindowOpen => false,
                bootty_config::config::WhenClosingWithNoTabs::PlatformDefault => {
                    !cfg!(target_os = "macos")
                }
            })
    }

    /// Apply a keybind on the app thread and report what it did, with the native mux focus when
    /// asked.
    fn apply_keybind_now(
        &mut self,
        action: KeybindAction,
        planned_mux_command: Option<MuxCommand>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        report_mux_focus: bool,
    ) -> CommandOutcome {
        let previous_error = self.last_error.take();
        self.apply_resolved_keybind_action(action, planned_mux_command, viewport, effects);
        if let Some(notice) = self.last_error.clone() {
            return CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message: notice.raw_message(),
            };
        }
        self.last_error = previous_error;
        if report_mux_focus {
            CommandOutcome::Success {
                value: self.current_mux_focus_value(),
                warnings: Vec::new(),
            }
        } else {
            CommandOutcome::success()
        }
    }
    const fn process_local_mux_action_uses_local_layout(action: MuxKeyAction) -> bool {
        matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
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

/// Tab and pane navigation lands in the terminal it selects.
const fn moves_terminal_focus(action: &KeybindAction) -> bool {
    matches!(
        action,
        KeybindAction::Mux(
            MuxKeyAction::NewTab
                | MuxKeyAction::NextTab
                | MuxKeyAction::PreviousTab
                | MuxKeyAction::LastTab
                | MuxKeyAction::SelectTab(_)
                | MuxKeyAction::SplitPane(_)
                | MuxKeyAction::SelectPane(_)
                | MuxKeyAction::NextPane
                | MuxKeyAction::PreviousPane
        )
    )
}
