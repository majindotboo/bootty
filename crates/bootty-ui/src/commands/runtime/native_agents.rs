#[path = "native_agent_spawn.rs"]
mod spawning;

use std::{
    sync::{Arc, mpsc},
    task::{Poll, ready},
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bootty_agents::{
    AgentCommandExecutor, AgentKind, AgentLaunch, NativeAgentService, NativeSessionConfig,
    NativeSessionRecord, NativeSessionStatus, TerminalAgentService, ToolBridge,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, CommandWarning,
    ResourceKind,
};
use bootty_mux::executor;
use serde_json::{Value, json};

use super::{
    CommandDispatch, PendingAppCommand, PendingCommandResult, agents::AppCommandAgentExecutor,
    serialized_command_outcome,
};
use crate::{
    commands::ExactMuxTarget,
    state::{AppEffect, AppState},
};

struct NativeLaunch {
    remote: Option<bootty_config::config::RemoteConfig>,
    context: super::terminal_agents::LaunchContext,
    quick_model: String,
    terminal: Option<CommandTarget>,
    split: Option<bootty_mux::pane_layout::SplitDirection>,
    tools_owner: Arc<TerminalAgentService>,
}

struct PreparedNativeLaunch {
    config: NativeSessionConfig,
    tools: Option<Arc<ToolBridge>>,
    warnings: Vec<CommandWarning>,
    spawn_parent: Option<CommandTarget>,
}

impl AppState {
    pub(super) fn poll_native_panel_close(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        let PendingCommandResult::NativePanelClose {
            targets,
            result_target,
            command,
            stopping,
        } = &mut pending.result
        else {
            return Poll::Pending;
        };
        let outcome = ready!(self.poll_pending_app_command(command, now, effects));
        if *stopping || !matches!(outcome, Some(CommandOutcome::Success { .. })) {
            return Poll::Ready(outcome);
        }
        let targets = targets.clone();
        let outcome = result_target.as_ref().map_or(outcome, |target| {
            Some(serialized_command_outcome(json!(target)))
        });
        let Some(service) = self.native_agent_service() else {
            return Poll::Ready(Some(CommandOutcome::Unavailable {
                message: "The native provider host is unavailable".into(),
            }));
        };
        let stopped = self.dispatch_committed_command(None, move || {
            for target in targets {
                if service.activities().iter().any(|record| {
                    record.id == target.handle
                        && (record.generation != target.generation
                            || record.status == NativeSessionStatus::Stopped)
                }) {
                    continue;
                }
                if let Err(message) = service.stop(&target) {
                    return CommandOutcome::Failed {
                        code: "native_stop_failed".into(),
                        message,
                    };
                }
            }
            outcome.unwrap_or_else(CommandOutcome::success)
        });
        match stopped {
            CommandDispatch::Complete(outcome) => return Poll::Ready(Some(outcome)),
            CommandDispatch::Pending(result) => {
                command.result = result;
                *stopping = true;
            }
        }
        Poll::Pending
    }

    pub(super) fn poll_native_panel_focus(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        let PendingCommandResult::NativePanelFocus { target, command } = &mut pending.result else {
            return Poll::Pending;
        };
        let outcome = ready!(self.poll_pending_app_command(command, now, effects));
        if matches!(outcome, Some(CommandOutcome::Success { .. })) {
            let record = self.native_agent_service().and_then(|service| {
                service
                    .sessions()
                    .into_iter()
                    .find(|record| record.target() == *target)
            });
            let Some(record) = record else {
                return Poll::Ready(Some(CommandOutcome::StaleTarget {
                    message: "The focused conversation changed".into(),
                }));
            };
            effects.push(AppEffect::NativeConversation(target.clone()));
            return Poll::Ready(Some(serialized_command_outcome(serde_json::json!(record))));
        }
        Poll::Ready(outcome)
    }

    pub(super) fn reconcile_native_pane_ownership(&mut self) {
        let Some(service) = self.native_agent_service() else {
            return;
        };
        let activities = service.activities();
        let observed = activities
            .iter()
            .filter_map(|record| {
                let exact = self.native_panel_exact(
                    &record.id,
                    &record.binding_id,
                    record.task_identity.as_deref()?,
                )?;
                Some((
                    record.id.clone(),
                    (
                        exact.scope(),
                        CommandTarget {
                            kind: ResourceKind::Session,
                            handle: record.id.clone(),
                            generation: record.generation,
                        },
                    ),
                ))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let previous = std::mem::replace(&mut self.commands.native_panes, observed);
        for (id, (scope, target)) in previous {
            if self.commands.native_panes.contains_key(&id) {
                continue;
            }
            if self.workspace.binding(scope).is_some_and(|binding| {
                !binding.mux().has_session_snapshot()
                    || binding.mux().unavailable_reason().is_some()
            }) {
                self.commands.native_panes.insert(id, (scope, target));
                continue;
            }
            if self.commands.pending.iter().any(|pending| matches!(&pending.result, PendingCommandResult::NativePanelClose { targets, .. } if targets.contains(&target))) {
                continue;
            }
            if activities.iter().any(|record| {
                CommandTarget {
                    kind: ResourceKind::Session,
                    handle: record.id.clone(),
                    generation: record.generation,
                } == target
                    && !matches!(
                        record.status,
                        NativeSessionStatus::Stopped | NativeSessionStatus::Error
                    )
            }) {
                let mut stop = CommandInvocation::new(
                    "agents.native.stop",
                    vec![target.handle.clone(), target.generation.to_string()],
                    Caller::Internal,
                );
                stop.target = Some(target);
                self.commands.queue(stop);
            }
        }
    }

    pub(crate) fn saved_native_conversation_invocation(
        &self,
        target: &bootty_mux::workspace::ScopedSessionTarget,
    ) -> Option<CommandInvocation> {
        let binding = self.workspace.binding(target.scope)?;
        let identity = self
            .workspace
            .session_identity(target.scope, &target.session_id)?;
        let saved = binding.sessions().get(&identity)?;
        // A conversation is the fallback only when there is no terminal state to restore.
        if saved.state.deleted || saved.terminal_snapshot.is_some() {
            return None;
        }
        let activity = self
            .native_agent_service()?
            .activities()
            .into_iter()
            .rev()
            .find(|record| {
                record.binding_id == target.scope.persistence_value().to_string()
                    && record.task_identity.as_deref() == Some(identity.as_str())
            })?;
        let mut invocation =
            CommandInvocation::from_action("agents.native.focus", Caller::Internal);
        invocation.target = Some(CommandTarget {
            kind: ResourceKind::Session,
            handle: activity.id,
            generation: activity.generation,
        });
        Some(invocation)
    }

    pub(crate) fn activate_native_task_from_ui(
        &mut self,
        binding_id: &str,
        identity: &str,
    ) -> bool {
        let Some(binding) = self.workspace.all_bindings().find(|binding| {
            binding.scope().persistence_value().to_string() == binding_id
                && binding
                    .sessions()
                    .get(identity)
                    .is_some_and(|saved| !saved.state.deleted)
        }) else {
            self.record_error("This conversation’s saved task is unavailable.");
            return false;
        };
        let scope = binding.scope();
        if let Some(session) = binding.session_attachment(identity) {
            let target = bootty_mux::workspace::ScopedSessionTarget::new(scope, &session.id);
            return self.activate_scoped_session_from_ui(&target);
        }
        if scope != self.mux_scope() && !self.activate_space_from_ui(scope) {
            return false;
        }
        self.set_sidebar_session_cursor(bootty_mux::workspace::ScopedSessionTarget::new(
            scope, identity,
        ));
        true
    }

    pub(super) fn dispatch_native_agent(
        &mut self,
        invocation: CommandInvocation,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(service) = self.native_agent_service() else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: self
                    .commands
                    .native_agent_error
                    .clone()
                    .unwrap_or_else(|| "Native conversation storage is unavailable".to_owned()),
            });
        };
        let Some(target) = invocation.target.as_ref() else {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "Native conversations require an exact captured destination".to_owned(),
            });
        };
        let operation = invocation
            .command
            .strip_prefix("agents.native.")
            .unwrap_or_default();
        if operation == "permissions" && invocation.caller == Caller::Luau {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "Provider permission changes require a host caller".into(),
            });
        }
        if matches!(
            operation,
            "start"
                | "tab"
                | "pane"
                | "list"
                | "activities"
                | "catalog"
                | "names"
                | "catalog-favorite"
                | "catalog-completions"
        ) {
            return self.dispatch_native_start_or_list(service, invocation, execution);
        }
        let binding_id = match service.binding_for_target(target) {
            Ok(binding_id) => binding_id,
            Err(error) => {
                return CommandDispatch::Complete(CommandOutcome::StaleTarget { message: error });
            }
        };
        if !self
            .workspace
            .all_bindings()
            .any(|binding| binding.scope().persistence_value().to_string() == binding_id)
        {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Conversation Space no longer exists".to_owned(),
            });
        }
        if matches!(operation, "focus" | "close") {
            return self.dispatch_native_visibility(service, &invocation, effects, execution);
        }
        if invocation.arguments.first() != Some(&target.handle)
            || invocation
                .arguments
                .get(1)
                .and_then(|value| value.parse::<u64>().ok())
                != Some(target.generation)
        {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Conversation identity or generation changed".to_owned(),
            });
        }
        if let Err(outcome) = self.native_operation_enabled(&service, target, operation) {
            return CommandDispatch::Complete(outcome);
        }
        if operation == "profiles" {
            return CommandDispatch::Complete(self.native_profile_names(&service, target));
        }
        if operation == "terminal" {
            return CommandDispatch::Complete(self.native_terminal_destination(&service, target));
        }
        if operation == "panel" {
            return self.dispatch_native_panel(&service, &invocation, execution);
        }
        if operation == "computer" {
            return self.dispatch_native_computer(&service, &invocation, execution);
        }
        if operation == "spawn" {
            return self.dispatch_native_spawn(service, &invocation, execution);
        }
        if operation == "control-child" {
            return self.dispatch_native_child_control(service, &invocation, execution);
        }
        if operation == "fork" {
            return self.dispatch_native_fork(service, &invocation, execution);
        }
        if matches!(operation, "resume" | "permissions") {
            return self.dispatch_native_resume(service, &invocation, execution);
        }
        self.dispatch_native_operation(service, invocation, execution)
    }

    fn native_profile_names(
        &self,
        service: &NativeAgentService,
        target: &CommandTarget,
    ) -> CommandOutcome {
        let info = match service.provider_info(target) {
            Ok(info) => info,
            Err(message) => return failure(message),
        };
        let Some(preferences) = self.config().agents.provider(&info.provider.to_string()) else {
            return failure("The captured provider configuration is unavailable".into());
        };
        // Names describe current configuration; retained account authority stays unchanged.
        let profiles: Vec<_> = preferences
            .profiles
            .iter()
            .map(|(id, profile)| json!({"id":id,"name":profile.name}))
            .collect();
        serialized_command_outcome(json!({
            "provider":info.provider,"captured_profile":info.profile,"profiles":profiles,
        }))
    }

    fn dispatch_native_operation(
        &self,
        service: Arc<NativeAgentService>,
        invocation: CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(target) = invocation.target.as_ref() else {
            return CommandDispatch::Complete(failure("Choose a conversation".into()));
        };
        let operation = invocation
            .command
            .strip_prefix("agents.native.")
            .unwrap_or_default();
        let (deadline, cancellation) = executor::command_execution(execution);
        let repaint = self.repaint.clone();
        let activity = (operation == "prompt")
            .then(|| self.native_activity_invocation(&service, target))
            .flatten();
        let commands = self.commands.sender.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = if cancellation.is_cancelled() {
                CommandOutcome::cancelled()
            } else if Instant::now() >= deadline {
                CommandOutcome::deadline_exceeded()
            } else {
                invoke(&service, &invocation, deadline, &cancellation)
            };
            if matches!(outcome, CommandOutcome::Success { .. })
                && let Some(activity) = activity
            {
                super::sessions::submit_activity_receipt(
                    &commands,
                    activity,
                    crate::clock::ClockSnapshot::now().epoch,
                );
            }
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
    fn dispatch_native_computer(
        &self,
        service: &NativeAgentService,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(target) = invocation.target.as_ref() else {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "Choose a conversation".into(),
            });
        };
        let access = invocation
            .arguments
            .get(2)
            .ok_or_else(|| "Missing application reference".to_owned())
            .and_then(|reference| service.application_access(target, reference, invocation.caller));
        let action = invocation
            .arguments
            .get(3)
            .ok_or_else(|| "Missing computer action".to_owned())
            .and_then(|action| serde_json::from_str(action).map_err(|e| e.to_string()));
        match access.and_then(|access| action.map(|action| (access, action))) {
            Ok((access, action)) => self.dispatch_computer_command(
                crate::commands::ComputerCommand::Application { access, action },
                execution,
            ),
            Err(error) => CommandDispatch::Complete(CommandOutcome::Denied { message: error }),
        }
    }
    fn dispatch_native_visibility(
        &mut self,
        service: Arc<NativeAgentService>,
        invocation: &CommandInvocation,
        _effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(target) = invocation.target.as_ref() else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Conversation target changed".into(),
            });
        };
        let Some(record) = service
            .sessions()
            .into_iter()
            .find(|record| record.target() == *target)
        else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The conversation changed".into(),
            });
        };
        if invocation.command == "agents.native.focus" {
            if let Err(outcome) = self.native_operation_enabled(&service, target, "focus") {
                return CommandDispatch::Complete(outcome);
            }
            if matches!(
                record.snapshot.status,
                NativeSessionStatus::Stopped | NativeSessionStatus::Error
            ) {
                return self.dispatch_native_resume(service, invocation, execution);
            }
            if let Some((exact, _)) = self.native_panel_target(&record) {
                return self.dispatch_native_panel_focus(&record, exact, execution);
            }
            return self.restore_legacy_native_panel(record, invocation, execution);
        }
        let Some((exact, _)) = self.native_panel_target(&record) else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The conversation pane is no longer open".into(),
            });
        };
        let closed = self.dispatch_session_command(
            crate::commands::SessionAction::ClosePane,
            &[],
            Some(exact),
            execution.clone(),
        );
        let mut closed = Self::retire_native_after_close(closed, vec![target.clone()], execution);
        if let CommandDispatch::Pending(PendingCommandResult::NativePanelClose {
            result_target,
            ..
        }) = &mut closed
        {
            *result_target = Some(target.clone());
        }
        closed
    }

    fn dispatch_native_panel_focus(
        &mut self,
        record: &NativeSessionRecord,
        exact: ExactMuxTarget,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let ExactMuxTarget::Pane(scope, session_id, window_id, pane_id) = exact else {
            return CommandDispatch::Complete(failure(
                "The conversation pane is unavailable".into(),
            ));
        };
        if scope != self.mux_scope() && !self.activate_space_from_ui(scope) {
            return CommandDispatch::Complete(failure(
                "The conversation Space could not be activated".into(),
            ));
        }
        let command = bootty_mux::command::MuxCommand::ActivatePane {
            session_id,
            window_id,
            pane_id,
        };
        if let Some(outcome) = self.preflight_mux_command(&command) {
            return CommandDispatch::Complete(outcome);
        }
        let (deadline, cancellation) = executor::command_execution(execution);
        let Some(submitted) = executor::submit_authoritative_command_for_scope(
            &mut self.workspace,
            &self.repaint,
            scope,
            command,
            None,
            Some((deadline, cancellation.clone())),
            bootty_mux::controller::CommandSelection::Follow,
        ) else {
            return CommandDispatch::Complete(failure("The conversation Space was closed".into()));
        };
        CommandDispatch::Pending(PendingCommandResult::NativePanelFocus {
            target: record.target(),
            command: Box::new(PendingAppCommand {
                creation_receipt: None,
                label: "Focus conversation".into(),
                user_initiated_annotation_capture: false,
                deadline,
                cancellation,
                response: None,
                result: PendingCommandResult::Mux {
                    scope: submitted.scope,
                    command: Box::new(submitted.command),
                    membership: submitted.membership,
                    layout: submitted.layout,
                    result: submitted.result,
                },
            }),
        })
    }

    fn restore_legacy_native_panel(
        &self,
        record: NativeSessionRecord,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(service) = self.native_agent_service() else {
            return CommandDispatch::Complete(failure(
                "The native conversation owner is unavailable".into(),
            ));
        };
        let commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            creation_receipt: None,
            sender: self.commands.sender.clone(),
        });
        let (deadline, cancellation) = executor::command_execution(execution);
        let caller = invocation.caller;
        self.dispatch_committed_command(Some((deadline, cancellation.clone())), move || {
            // Rebind the provider's tools while adopting its real backend carrier.
            if let Err(error) = service.stop(&record.target()) {
                return failure(error);
            }
            let mut focus = CommandInvocation::from_action("agents.native.focus", caller);
            focus.target = Some(record.target());
            commands.execute(focus, deadline, cancellation)
        })
    }

    pub(super) fn native_targets_for_close(
        &self,
        scope: bootty_mux::controller::SpaceId,
        command: &bootty_mux::command::MuxCommand,
    ) -> Vec<CommandTarget> {
        use bootty_mux::command::MuxCommand;
        let Some(service) = self.native_agent_service() else {
            return Vec::new();
        };
        service
            .activities()
            .into_iter()
            .filter_map(|record| {
                let exact = self.native_panel_exact(
                    &record.id,
                    &record.binding_id,
                    record.task_identity.as_deref()?,
                )?;
                let (session, _, pane) = exact.ids();
                let closes = exact.scope() == scope
                    && match command {
                        MuxCommand::DitchSession { session_id } => session == Some(session_id),
                        MuxCommand::ClosePane {
                            session_id,
                            pane_id: Some(id),
                        }
                        | MuxCommand::KillPane {
                            session_id,
                            pane_id: Some(id),
                        } => session == Some(session_id) && pane == Some(id),
                        _ => false,
                    };
                (closes && record.status != NativeSessionStatus::Stopped).then_some(CommandTarget {
                    kind: ResourceKind::Session,
                    handle: record.id,
                    generation: record.generation,
                })
            })
            .collect()
    }

    pub(super) fn retire_native_after_close(
        dispatch: CommandDispatch,
        targets: Vec<CommandTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if targets.is_empty() {
            return dispatch;
        }
        let result = match dispatch {
            CommandDispatch::Pending(result) => result,
            CommandDispatch::Complete(outcome) => {
                let (reply, result) = mpsc::channel();
                _ = reply.send(outcome);
                PendingCommandResult::Outcome(result)
            }
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        CommandDispatch::Pending(PendingCommandResult::NativePanelClose {
            targets,
            result_target: None,
            stopping: false,
            command: Box::new(PendingAppCommand {
                creation_receipt: None,
                label: "Close native providers".into(),
                user_initiated_annotation_capture: false,
                deadline,
                cancellation,
                response: None,
                result,
            }),
        })
    }

    fn native_panel_exact(&self, id: &str, binding_id: &str, task: &str) -> Option<ExactMuxTarget> {
        let binding = self
            .workspace
            .all_bindings()
            .find(|binding| binding.scope().persistence_value().to_string() == binding_id)?;
        let session = binding.session_attachment(task)?;
        let (window, pane) = session.windows.iter().find_map(|window| {
            window
                .panes
                .iter()
                .find(|pane| pane.native_agent.as_deref() == Some(id))
                .map(|pane| (window, pane))
        })?;
        Some(ExactMuxTarget::Pane(
            binding.scope(),
            session.id.clone(),
            window.id.clone(),
            pane.pane_id.clone()?,
        ))
    }

    pub(crate) fn native_panel_target(
        &self,
        record: &NativeSessionRecord,
    ) -> Option<(ExactMuxTarget, CommandTarget)> {
        let exact = self.native_panel_exact(
            &record.id,
            &record.binding_id,
            record.task_identity.as_deref()?,
        )?;
        let binding = self.workspace.binding(exact.scope())?;
        let handle =
            self.binding_target_handle(binding.scope(), binding.mux().binding_generation());
        let target = exact.command_target(ResourceKind::Terminal, binding.mux(), &handle)?;
        Some((exact, target))
    }

    fn native_terminal_destination(
        &self,
        service: &NativeAgentService,
        target: &CommandTarget,
    ) -> CommandOutcome {
        let Some(record) = service
            .sessions()
            .into_iter()
            .find(|record| record.target() == *target)
        else {
            return CommandOutcome::StaleTarget {
                message: "The conversation changed".into(),
            };
        };
        let placed = self.native_panel_target(&record);
        let parent = record
            .side_chat
            .as_ref()
            .and_then(|side_chat| {
                service
                    .sessions()
                    .into_iter()
                    .find(|parent| parent.id == side_chat.source_id)
            })
            .and_then(|parent| self.native_panel_target(&parent));
        let destination = placed.clone().or_else(|| parent.clone()).map_or_else(
            || {
                self.native_task_target(
                    &record.binding_id,
                    record.task_identity.as_deref().unwrap_or_default(),
                )
            },
            Ok,
        );
        let (_, terminal) = match destination {
            Ok(destination) => destination,
            Err(outcome) => return outcome,
        };
        let exact = match self.resolve_command_target(
            "terminal.create_pane",
            Some(ResourceKind::Terminal),
            Some(&terminal),
        ) {
            Ok((_, Some(exact))) => exact,
            Ok(_) => return failure("The conversation pane is unavailable".into()),
            Err(outcome) => return outcome,
        };
        let Some(binding) = self.workspace.binding(exact.scope()) else {
            return failure("The conversation Space was closed".into());
        };
        let (session, _, pane) = exact.ids();
        let occupied = binding
            .mux()
            .all_sessions()
            .iter()
            .filter(|held| Some(held.id.as_str()) == session)
            .flat_map(|held| &held.windows)
            .flat_map(|window| window.panes.iter().chain(std::iter::once(&window.anchor)))
            .find(|held| held.pane_id.as_deref() == pane)
            .is_some_and(|held| held.native_agent.is_some());
        let session_target = session.and_then(|id| {
            ExactMuxTarget::Session(exact.scope(), id.into()).command_target(
                ResourceKind::Session,
                binding.mux(),
                &self.binding_target_handle(binding.scope(), binding.mux().binding_generation()),
            )
        });
        CommandOutcome::Success {
            value: json!({"terminal": terminal, "placed": placed.is_some(), "occupied": occupied, "session": session_target, "split": parent.is_some() && placed.is_none()}),
            warnings: Vec::new(),
        }
    }

    fn dispatch_native_panel(
        &mut self,
        service: &NativeAgentService,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(record) = service
            .sessions()
            .into_iter()
            .find(|record| Some(record.target()) == invocation.target)
        else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The native panel conversation changed".into(),
            });
        };
        let terminal = invocation
            .arguments
            .get(2)
            .and_then(|encoded| serde_json::from_str::<CommandTarget>(encoded).ok());
        let Some(terminal) = terminal.filter(|target| target.kind == ResourceKind::Terminal) else {
            return CommandDispatch::Complete(failure(
                "Native panel placement needs its exact backend pane".into(),
            ));
        };
        let exact = match self.resolve_command_target(
            "terminal.create_pane",
            Some(ResourceKind::Terminal),
            Some(&terminal),
        ) {
            Ok((_, Some(exact))) => exact,
            Ok(_) => {
                return CommandDispatch::Complete(failure(
                    "The native panel pane is unavailable".into(),
                ));
            }
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        let (Some(session), _, Some(pane)) = exact.ids() else {
            return CommandDispatch::Complete(failure(
                "Native panel placement needs a pane identity".into(),
            ));
        };
        if record.binding_id != exact.scope().persistence_value().to_string()
            || record.task_identity != self.workspace.session_identity(exact.scope(), session)
        {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "The native panel belongs to another task or Space".into(),
            });
        }
        if self
            .native_panel_target(&record)
            .is_some_and(|(placed, _)| placed != exact)
        {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "This conversation already has a backend pane".into(),
            });
        }
        if self
            .workspace
            .binding(exact.scope())
            .and_then(|binding| binding.mux().backend_session_by_id_or_name(session))
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .flat_map(|window| &window.panes)
                    .find(|held| held.pane_id.as_deref() == Some(pane))
            })
            .and_then(|pane| pane.native_agent.as_deref())
            .is_some_and(|id| id != record.id)
        {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "The pane already belongs to another conversation".into(),
            });
        }
        let command = bootty_mux::command::MuxCommand::SetPaneNativeAgent {
            session_id: session.to_owned(),
            pane_id: pane.to_owned(),
            agent_id: Some(record.id),
        };
        if let Some(outcome) = self.preflight_mux_command(&command) {
            return CommandDispatch::Complete(outcome);
        }
        let Some(submitted) = executor::submit_authoritative_command_for_scope(
            &mut self.workspace,
            &self.repaint,
            exact.scope(),
            command,
            None,
            execution,
            bootty_mux::controller::CommandSelection::Preserve,
        ) else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The native panel Space was closed".into(),
            });
        };
        CommandDispatch::Pending(PendingCommandResult::Mux {
            scope: submitted.scope,
            command: Box::new(submitted.command),
            membership: submitted.membership,
            layout: submitted.layout,
            result: submitted.result,
        })
    }

    fn native_operation_enabled(
        &self,
        service: &NativeAgentService,
        target: &CommandTarget,
        operation: &str,
    ) -> Result<(), CommandOutcome> {
        let Some(provider) = service
            .activities()
            .into_iter()
            .find(|record| record.id == target.handle)
            .map(|record| record.provider)
        else {
            return Err(CommandOutcome::StaleTarget {
                message: "Conversation target is stale".to_owned(),
            });
        };
        if self
            .config()
            .agents
            .provider(&provider.to_string())
            .is_none_or(|preferences| !preferences.enabled)
            && !matches!(operation, "interrupt" | "stop")
        {
            return Err(CommandOutcome::Denied {
                message: format!("{provider} is disabled in provider settings"),
            });
        }
        Ok(())
    }

    fn native_activity_invocation(
        &self,
        service: &NativeAgentService,
        target: &CommandTarget,
    ) -> Option<CommandInvocation> {
        let record = service
            .activities()
            .into_iter()
            .find(|record| record.id == target.handle)?;
        let scope = self
            .workspace
            .all_bindings()
            .find(|binding| binding.scope().persistence_value().to_string() == record.binding_id)?
            .scope();
        self.saved_session_invocation(
            scope,
            "session.input_accepted",
            vec![record.task_identity?, "0".to_owned()],
        )
    }

    fn native_task_target(
        &self,
        binding_id: &str,
        identity: &str,
    ) -> Result<(ExactMuxTarget, CommandTarget), CommandOutcome> {
        let binding = self
            .workspace
            .all_bindings()
            .find(|binding| binding.scope().persistence_value().to_string() == binding_id)
            .ok_or_else(|| CommandOutcome::StaleTarget {
                message: "Conversation Space is unavailable".to_owned(),
            })?;
        let scope = binding.scope();
        let task = binding
            .mux()
            .sessions()
            .iter()
            .find(|session| {
                self.workspace
                    .session_identity(scope, &session.id)
                    .as_deref()
                    == Some(identity)
            })
            .ok_or_else(|| CommandOutcome::StaleTarget {
                message: "The conversation’s saved task is not attached".to_owned(),
            })?;
        let exact = ExactMuxTarget::Session(scope, task.id.clone());
        let terminal = self
            .session_terminal_target(scope, &task.id)
            .ok_or_else(|| CommandOutcome::StaleTarget {
                message: "The task has no exact Terminal target".to_owned(),
            })?;
        Ok((exact, terminal))
    }

    fn dispatch_native_resume(
        &self,
        service: Arc<NativeAgentService>,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if invocation
            .target
            .as_ref()
            .is_some_and(|target| self.native_resume_pending(target))
        {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "This conversation is already being resumed".into(),
            });
        }
        let Some(record) = service
            .sessions()
            .into_iter()
            .find(|record| Some(record.target()) == invocation.target)
        else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Conversation target changed".to_owned(),
            });
        };
        let launch_invocation = CommandInvocation::new(
            format!("{}.start", record.config.provider.module()),
            vec![
                record.config.cwd.to_string_lossy().into_owned(),
                record.config.program.clone(),
                "[]".to_owned(),
                String::new(),
                String::new(),
            ],
            invocation.caller,
        );
        let permissions = if invocation.command == "agents.native.permissions" {
            match invocation
                .arguments
                .get(2)
                .ok_or_else(|| "Missing permission mode".to_owned())
                .and_then(|mode| mode.parse::<bootty_agents::NativePermissionMode>())
            {
                Ok(mode) if mode.supports(record.config.provider) => Some(mode),
                Ok(_) => {
                    return CommandDispatch::Complete(failure(
                        "This provider does not support the selected permission mode".into(),
                    ));
                }
                Err(error) => return CommandDispatch::Complete(failure(error)),
            }
        } else {
            None
        };
        let captured = match if permissions.is_some() {
            self.capture_native_destination(&record, &launch_invocation)
        } else {
            self.capture_native_resume(&record, &launch_invocation)
        } {
            Ok(captured) => captured,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        let commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            creation_receipt: None,
            sender: self.commands.sender.clone(),
        });
        let (deadline, cancellation) = executor::command_execution(execution);
        let repaint = self.repaint.clone();
        let (sender, receiver) = mpsc::channel();
        let focus_after_resume = invocation.command == "agents.native.focus";
        let target = record.target();
        std::thread::spawn(move || {
            if let Some(outcome) = permissions.and_then(|mode| {
                configure_native_permissions(
                    &service,
                    &record.target(),
                    mode,
                    (deadline, &cancellation),
                )
            }) {
                let _ = sender.send(outcome);
                repaint();
                return;
            }
            let outcome = resume_native_session(
                &service,
                &commands,
                &captured,
                &record,
                &launch_invocation,
                (deadline, cancellation),
                focus_after_resume,
            );
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::NativeResume {
            target,
            result: receiver,
        })
    }

    pub(crate) fn native_resume_pending(&self, target: &CommandTarget) -> bool {
        self.commands.pending.iter().any(|pending| {
            matches!(
                &pending.result,
                PendingCommandResult::NativeResume { target: current, .. } if current == target
            )
        })
    }

    fn dispatch_native_fork(
        &self,
        service: Arc<NativeAgentService>,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(record) = service
            .sessions()
            .into_iter()
            .find(|record| Some(record.target()) == invocation.target)
        else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The source conversation is stale".into(),
            });
        };
        let launch_invocation = CommandInvocation::new(
            format!("{}.start", record.config.provider.module()),
            Vec::new(),
            invocation.caller,
        );
        let captured = match self.capture_native_destination(&record, &launch_invocation) {
            Ok(captured) => captured,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        let commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            creation_receipt: None,
            sender: self.commands.sender.clone(),
        });
        let response = invocation
            .arguments
            .get(2)
            .filter(|response| !response.is_empty())
            .cloned();
        let invocation_caller = invocation.caller;
        let (deadline, cancellation) = executor::command_execution(execution);
        self.dispatch_committed_command(Some((deadline, cancellation.clone())), move || {
            let launch = AgentLaunch {
                program: record.config.program.clone(),
                cwd: Some(record.config.cwd.to_string_lossy().into_owned()),
                arguments: record.config.arguments.clone(),
                ephemeral: false,
                account_directory: record.config.account_directory.clone(),
            };
            let result = (|| {
                let (tools, mut warnings) = native_tools(
                    &commands,
                    &launch_invocation,
                    &captured,
                    &record.config,
                    &launch,
                )?;
                let parent = captured
                    .terminal
                    .as_ref()
                    .ok_or("The source native pane is unavailable")?;
                let (terminal, creation_warnings) = create_native_panel_terminal(
                    &commands,
                    parent,
                    captured.context.session.as_ref(),
                    &record.config.cwd,
                    Some("right"),
                    invocation_caller,
                    deadline,
                    &cancellation,
                )
                .map_err(|outcome| {
                    crate::commands::command_outcome_message(&outcome)
                        .unwrap_or_else(|| "Could not create the side chat pane".into())
                })?;
                warnings.extend(creation_warnings);
                if let Some(tools) = &tools {
                    let target = serde_json::from_value(terminal.clone())
                        .map_err(|error| format!("The side chat has no backend pane: {error}"))?;
                    bind_native_tools(&captured, tools, target, &launch)?;
                }
                let child = service.fork_side_chat_placed(
                    &record.target(),
                    response.as_deref(),
                    tools,
                    |child| {
                        let registered = register_native_panel(
                            &commands,
                            child,
                            &terminal,
                            invocation_caller,
                            deadline,
                            &cancellation,
                        );
                        match registered {
                            CommandOutcome::Success { .. } => Ok(()),
                            outcome => Err(crate::commands::command_outcome_message(&outcome)
                                .unwrap_or_else(|| {
                                    "Could not associate the side chat pane".into()
                                })),
                        }
                    },
                )?;
                Ok(CommandOutcome::Success {
                    value: json!(child),
                    warnings,
                })
            })();
            result.unwrap_or_else(failure)
        })
    }

    fn capture_native_resume(
        &self,
        record: &NativeSessionRecord,
        invocation: &CommandInvocation,
    ) -> Result<NativeLaunch, CommandOutcome> {
        if !matches!(
            record.snapshot.status,
            NativeSessionStatus::Stopped | NativeSessionStatus::Error
        ) {
            return Err(failure(
                "Native session already has a live process".to_owned(),
            ));
        }
        self.capture_native_destination(record, invocation)
    }

    fn capture_native_destination(
        &self,
        record: &NativeSessionRecord,
        invocation: &CommandInvocation,
    ) -> Result<NativeLaunch, CommandOutcome> {
        let identity =
            record
                .task_identity
                .as_deref()
                .ok_or_else(|| CommandOutcome::StaleTarget {
                    message: "Conversation has no captured task".to_owned(),
                })?;
        let binding = self
            .workspace
            .all_bindings()
            .find(|binding| {
                binding.scope().persistence_value().to_string() == record.binding_id
                    && binding
                        .sessions()
                        .get(identity)
                        .is_some_and(|saved| !saved.state.deleted)
            })
            .ok_or_else(|| CommandOutcome::StaleTarget {
                message: "Conversation’s saved task is unavailable".to_owned(),
            })?;
        if record.config.remote.as_ref().map(|remote| &remote.host)
            != binding.multiplexer().remote.as_ref()
        {
            return Err(CommandOutcome::StaleTarget {
                message: "Conversation’s captured host no longer matches this Space".into(),
            });
        }
        let (exact, terminal) = if let Some((exact, terminal)) = self.native_panel_target(record) {
            (exact, Some(terminal))
        } else if binding.session_attachment(identity).is_some() {
            let (exact, terminal) = self.native_task_target(&record.binding_id, identity)?;
            (exact, Some(terminal))
        } else {
            (ExactMuxTarget::Binding(binding.scope()), None)
        };
        let tools_owner =
            self.terminal_agent_service()
                .ok_or_else(|| CommandOutcome::Unavailable {
                    message: "Agent tool owner is unavailable".to_owned(),
                })?;
        let mut context = self.terminal_launch_context(invocation, Some(&exact));
        context.preferences = self
            .config()
            .agents
            .provider(&record.config.provider.to_string())
            .cloned()
            .unwrap_or_default();
        // Restoring an agent keeps its captured directory even when another task is selected.
        context.cwd = Some(record.config.cwd.to_string_lossy().into_owned());
        Ok(NativeLaunch {
            remote: binding.multiplexer().remote.clone(),
            context,
            quick_model: self.config().agents.quick_model.clone(),
            terminal,
            split: None,
            tools_owner,
        })
    }

    fn capture_native_launch(
        &self,
        invocation: &CommandInvocation,
        destination: &ExactMuxTarget,
        binding_id: &str,
    ) -> Result<NativeLaunch, CommandOutcome> {
        let exact = destination.scope();
        let binding = self
            .workspace
            .binding(exact)
            .ok_or_else(|| CommandOutcome::StaleTarget {
                message: "Conversation Space is unavailable".to_owned(),
            })?;
        let split = if invocation.command == "agents.native.pane" {
            Some(match invocation.arguments.get(15).map(String::as_str) {
                Some("right") => bootty_mux::pane_layout::SplitDirection::Right,
                Some("down") => bootty_mux::pane_layout::SplitDirection::Down,
                _ => return Err(failure("Choose a split direction".into())),
            })
        } else {
            None
        };
        let captured_task = if split.is_some() {
            let (Some(session), Some(_), Some(_)) = destination.ids() else {
                return Err(failure("An agent split requires an existing pane".into()));
            };
            if self.workspace.session_identity(exact, session).as_ref()
                != invocation.arguments.get(6)
            {
                return Err(CommandOutcome::Denied {
                    message: "The split pane belongs to another task".into(),
                });
            }
            let handle = self.binding_target_handle(exact, binding.mux().binding_generation());
            let terminal = destination
                .command_target(ResourceKind::Terminal, binding.mux(), &handle)
                .ok_or_else(|| failure("The captured split pane is unavailable".into()))?;
            Some((destination.clone(), terminal))
        } else if invocation.command == "agents.native.tab" {
            match self.native_task_target(
                binding_id,
                invocation.arguments.get(6).map_or("", String::as_str),
            ) {
                Ok((exact, terminal)) => {
                    let cwd = self.agent_launch_context(&exact).cwd;
                    if cwd.as_ref() != invocation.arguments.get(1) {
                        return Err(CommandOutcome::StaleTarget {
                            message: "The session directory changed. Open New agent tab again."
                                .to_owned(),
                        });
                    }
                    Some((exact, terminal))
                }
                Err(outcome) => return Err(outcome),
            }
        } else {
            None
        };
        let Some(provider) = invocation
            .arguments
            .first()
            .and_then(|id| native_provider(id))
        else {
            return Err(CommandOutcome::Unsupported {
                message: "This provider has no native integration".to_owned(),
            });
        };
        let Some(preferences) = self
            .config()
            .agents
            .provider(&provider.to_string())
            .filter(|preferences| preferences.enabled)
        else {
            return Err(CommandOutcome::Denied {
                message: format!("{provider} is disabled in provider settings"),
            });
        };
        let Some(tools_owner) = self.terminal_agent_service() else {
            return Err(CommandOutcome::Unavailable {
                message: "Agent tool owner is unavailable".to_owned(),
            });
        };
        let launch_invocation = provider_start_invocation(invocation, provider);
        let mut context = self.terminal_launch_context(
            &launch_invocation,
            Some(&captured_task.as_ref().map_or_else(
                || ExactMuxTarget::Binding(exact),
                |(exact, _)| exact.clone(),
            )),
        );
        context.preferences = preferences.clone();
        Ok(NativeLaunch {
            remote: binding.multiplexer().remote.clone(),
            context,
            quick_model: self.config().agents.quick_model.clone(),
            terminal: captured_task.map(|(_, terminal)| terminal),
            split,
            tools_owner,
        })
    }

    fn native_launch_destination(
        &self,
        invocation: &CommandInvocation,
    ) -> Result<ExactMuxTarget, CommandOutcome> {
        let pane = invocation.command == "agents.native.pane";
        match self.resolve_command_target(
            &invocation.command,
            Some(if pane {
                ResourceKind::Terminal
            } else {
                ResourceKind::Binding
            }),
            invocation.target.as_ref(),
        ) {
            Ok((_, Some(exact))) if pane || matches!(exact, ExactMuxTarget::Binding(_)) => {
                Ok(exact)
            }
            Ok(_) => Err(CommandOutcome::StaleTarget {
                message: "Choose an exact Space".to_owned(),
            }),
            Err(outcome) => Err(outcome),
        }
    }

    fn dispatch_native_start_or_list(
        &self,
        service: Arc<NativeAgentService>,
        invocation: CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let destination = match self.native_launch_destination(&invocation) {
            Ok(destination) => destination,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        let exact = destination.scope();
        if self.workspace.binding(exact).is_none() {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Conversation Space is unavailable".to_owned(),
            });
        }
        let binding_id = exact.persistence_value().to_string();
        if invocation.command == "agents.native.activities" {
            return CommandDispatch::Complete(serialized_command_outcome(json!(
                service.activities_for_binding(&binding_id)
            )));
        }
        if invocation.command == "agents.native.list" {
            return CommandDispatch::Complete(serialized_command_outcome(json!(
                service
                    .sessions()
                    .into_iter()
                    .filter(|record| record.binding_id == binding_id)
                    .collect::<Vec<_>>()
            )));
        }
        let captured = match self.capture_native_launch(&invocation, &destination, &binding_id) {
            Ok(captured) => captured,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        let commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            creation_receipt: None,
            sender: self.commands.sender.clone(),
        });
        let (deadline, cancellation) = executor::command_execution(execution);
        let repaint = self.repaint.clone();
        let activity = invocation
            .arguments
            .get(8)
            .filter(|prompt| !prompt.trim().is_empty())
            .or_else(|| {
                invocation
                    .arguments
                    .get(10)
                    .filter(|paths| !paths.trim().is_empty())
            })
            .and_then(|_| {
                self.saved_session_invocation(
                    exact,
                    "session.input_accepted",
                    vec![invocation.arguments.get(6)?.clone(), "0".to_owned()],
                )
            });
        let activity_sender = self.commands.sender.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = if matches!(
                invocation.command.as_str(),
                "agents.native.catalog"
                    | "agents.native.catalog-favorite"
                    | "agents.native.catalog-completions"
                    | "agents.native.names"
            ) {
                query_launch_metadata(&service, &invocation, &captured, deadline, &cancellation)
            } else {
                start(
                    &service,
                    &commands,
                    &invocation,
                    &captured,
                    deadline,
                    &cancellation,
                )
            };
            if matches!(outcome, CommandOutcome::Success { .. })
                && let Some(activity) = activity
            {
                super::sessions::submit_activity_receipt(
                    &activity_sender,
                    activity,
                    crate::clock::ClockSnapshot::now().epoch,
                );
            }
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
}

fn resume_native_session(
    service: &NativeAgentService,
    commands: &Arc<dyn AgentCommandExecutor>,
    resume: &NativeLaunch,
    record: &NativeSessionRecord,
    invocation: &CommandInvocation,
    execution: (Instant, CommandCancellation),
    focus_after_resume: bool,
) -> CommandOutcome {
    let (deadline, cancellation) = execution;
    let captured = resume;
    if cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    if Instant::now() >= deadline {
        return CommandOutcome::deadline_exceeded();
    }
    let launch = AgentLaunch {
        program: record.config.program.clone(),
        cwd: Some(record.config.cwd.to_string_lossy().into_owned()),
        arguments: record.config.arguments.clone(),
        ephemeral: false,
        account_directory: record.config.account_directory.clone(),
    };
    // Restoring a child resumes its provider identity, never its old parent's tool grant.
    // Tools can be renewed only by a fresh live parent grant.
    let prepared_tools = if record.spawn_parent.is_some() {
        Ok((None, Vec::new()))
    } else {
        native_tools(commands, invocation, captured, &record.config, &launch)
    };
    let (tools, mut warnings) = match prepared_tools {
        Ok(prepared) => prepared,
        Err(error) => return failure(error),
    };
    let launch_guard = match tools
        .as_ref()
        .map(|tools| tools.lease().begin_launch(&cancellation))
        .transpose()
    {
        Ok(guard) => guard,
        Err(error) => return failure(error),
    };
    let terminal = match restore_native_terminal(
        commands,
        resume,
        record,
        invocation.caller,
        deadline,
        &cancellation,
    ) {
        Ok((terminal, restore_warnings)) => {
            warnings.extend(restore_warnings);
            terminal
        }
        Err(outcome) => return outcome,
    };
    if cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    if Instant::now() >= deadline {
        return CommandOutcome::deadline_exceeded();
    }
    // Nested task restoration has its own mutation token. Claim this launch token before the
    // provider starts so the unbound-tool guard cannot cancel an accepted resume on drop.
    if !cancellation.try_start() {
        return CommandOutcome::cancelled();
    }
    let result = terminal.zip(tools.as_ref()).map_or_else(
        || service.resume(&record.target()),
        |(terminal, tools)| {
            bind_native_tools(captured, tools, terminal, &launch)
                .and_then(|()| service.resume_with_tools(&record.target(), Arc::clone(tools)))
        },
    );
    drop(launch_guard);
    drop(tools);
    match result {
        Ok(record) => {
            if focus_after_resume {
                let mut focus =
                    CommandInvocation::from_action("agents.native.focus", invocation.caller);
                focus.target = Some(record.target());
                let outcome = commands.execute(focus, deadline, CommandCancellation::new());
                if !matches!(outcome, CommandOutcome::Success { .. }) {
                    return outcome;
                }
            }
            CommandOutcome::Success {
                value: json!(record),
                warnings,
            }
        }
        Err(error) => failure(error),
    }
}

fn restore_native_terminal(
    commands: &Arc<dyn AgentCommandExecutor>,
    resume: &NativeLaunch,
    record: &NativeSessionRecord,
    caller: Caller,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> Result<(Option<CommandTarget>, Vec<CommandWarning>), CommandOutcome> {
    let captured = resume;
    let mut warnings = Vec::new();
    if captured.terminal.is_none() {
        let identity = record
            .task_identity
            .as_ref()
            .ok_or_else(|| failure("Conversation has no captured task".into()))?;
        let mut restore = CommandInvocation::new("session.reopen", vec![identity.clone()], caller);
        restore.target.clone_from(&captured.context.binding);
        match commands.execute(restore, deadline, cancellation.clone()) {
            CommandOutcome::Success {
                warnings: restored, ..
            } => warnings.extend(restored),
            outcome => return Err(outcome),
        }
    }
    let mut locate = CommandInvocation::new(
        "agents.native.terminal",
        vec![record.id.clone(), record.generation.to_string()],
        caller,
    );
    locate.target = Some(record.target());
    let destination = match commands.execute(locate, deadline, cancellation.clone()) {
        CommandOutcome::Success { value, .. } => value,
        outcome => return Err(outcome),
    };
    let mut terminal: CommandTarget =
        serde_json::from_value(destination.get("terminal").cloned().unwrap_or(Value::Null))
            .map_err(|error| failure(format!("The conversation has no backend pane: {error}")))?;
    if destination.get("placed").and_then(Value::as_bool) != Some(true) {
        if destination.get("occupied").and_then(Value::as_bool) == Some(true) {
            let session = serde_json::from_value::<CommandTarget>(
                destination.get("session").cloned().unwrap_or(Value::Null),
            )
            .map_err(|error| failure(format!("The conversation task is unavailable: {error}")))?;
            let (created, creation_warnings) = create_native_panel_terminal(
                commands,
                &terminal,
                Some(&session),
                &record.config.cwd,
                (destination.get("split").and_then(Value::as_bool) == Some(true))
                    .then_some("right"),
                caller,
                deadline,
                cancellation,
            )?;
            warnings.extend(creation_warnings);
            terminal = serde_json::from_value(created).map_err(|error| {
                failure(format!("The backend returned no created pane: {error}"))
            })?;
        }
        let placed = register_native_panel(
            commands,
            record,
            &json!(terminal),
            caller,
            deadline,
            cancellation,
        );
        if !matches!(placed, CommandOutcome::Success { .. }) {
            return Err(placed);
        }
    }
    Ok((Some(terminal), warnings))
}

fn start(
    service: &NativeAgentService,
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    captured: &NativeLaunch,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> CommandOutcome {
    let binding_id = captured.context.binding_id.as_str();
    let arg = |index| invocation.arguments.get(index).cloned().unwrap_or_default();
    if [arg(6), arg(7)]
        .iter()
        .any(|value| value.trim().is_empty() || value.chars().any(char::is_control))
        || arg(6).len() > 8192
        || arg(7).len() > 256
    {
        return failure(
            "Native start requires a valid captured task identity and title".to_owned(),
        );
    }
    let tab = matches!(
        invocation.command.as_str(),
        "agents.native.tab" | "agents.native.pane"
    );
    if !tab
        && service.activities().iter().any(|record| {
            record.binding_id == binding_id
                && record.task_identity.as_deref() == Some(arg(6).as_str())
        })
    {
        return failure("This task already has a saved conversation. Open its Conversation view to resume or retry it.".to_owned());
    }
    let prepared = match prepare_native_launch(service, commands, invocation, captured) {
        Ok(prepared) => prepared,
        Err(error) => return failure(error),
    };
    start_prepared(
        service,
        commands,
        invocation,
        captured,
        prepared,
        deadline,
        cancellation,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Linear acceptance phases keep the saved task and provider placement together"
)]
fn start_prepared(
    service: &NativeAgentService,
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    captured: &NativeLaunch,
    prepared: PreparedNativeLaunch,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> CommandOutcome {
    let arg = |index| invocation.arguments.get(index).cloned().unwrap_or_default();
    let attachment_paths = match native_start_attachment_paths(invocation) {
        Ok(paths) => paths,
        Err(error) => return failure(error),
    };
    let tab = matches!(
        invocation.command.as_str(),
        "agents.native.tab" | "agents.native.pane"
    );
    let PreparedNativeLaunch {
        config,
        tools,
        warnings: mut tools_warnings,
        spawn_parent,
    } = prepared;
    if cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    if Instant::now() >= deadline {
        return CommandOutcome::deadline_exceeded();
    }
    let (terminal, warnings) = if tab {
        let Some(parent) = captured.terminal.as_ref() else {
            return failure("The native tab has no captured backend parent".into());
        };
        match create_native_panel_terminal(
            commands,
            parent,
            captured.context.session.as_ref(),
            &config.cwd,
            captured.split.map(|direction| match direction {
                bootty_mux::pane_layout::SplitDirection::Right => "right",
                bootty_mux::pane_layout::SplitDirection::Down => "down",
            }),
            invocation.caller,
            deadline,
            cancellation,
        ) {
            Ok(created) => created,
            Err(outcome) => return outcome,
        }
    } else {
        match create_task_shell(commands, invocation, deadline, cancellation) {
            Ok(created) => created,
            Err(outcome) => return outcome,
        }
    };
    if cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    tools_warnings.extend(warnings);
    let terminal_target = match serde_json::from_value::<CommandTarget>(terminal.clone()) {
        Ok(target) => target,
        Err(error) => return failure(format!("Native launch has no exact task Terminal: {error}")),
    };
    let result = launch_native_record(
        service,
        captured,
        invocation,
        config,
        tools,
        &terminal_target,
        spawn_parent.as_ref(),
        &attachment_paths,
        commands,
        deadline,
        cancellation,
    );
    let record = match result {
        Ok(record) => record,
        Err(error) => {
            // A durable failed reservation still owns its new pane. Keep its error visible
            // instead of publishing that pane as an unrelated empty terminal.
            if spawn_parent.is_some()
                && let Some(record) = service
                    .sessions()
                    .into_iter()
                    .find(|record| Some(record.target()) == error.target)
            {
                let placed = register_native_panel(
                    commands,
                    &record,
                    &terminal,
                    invocation.caller,
                    deadline,
                    cancellation,
                );
                if !matches!(placed, CommandOutcome::Success { .. }) {
                    return placed;
                }
            }
            return failure(format!(
                "Provider launch failed: {error}. The saved task is retained."
            ));
        }
    };
    // Root conversations were placed at reservation; spawned children are placed here.
    if spawn_parent.is_some() {
        let placed = register_native_panel(
            commands,
            &record,
            &terminal,
            invocation.caller,
            deadline,
            cancellation,
        );
        if !matches!(placed, CommandOutcome::Success { .. }) {
            return placed;
        }
    }
    let first_prompt_error = initial_native_input(
        service,
        &record,
        &arg(8),
        &attachment_paths,
        invocation,
        deadline,
        cancellation,
    );
    if let Some(error) = &first_prompt_error {
        tools_warnings.push(CommandWarning {
            code: "native_first_prompt_failed".to_owned(),
            message: format!("Conversation saved; initial input failed: {error}. Open the conversation to review its state before retrying."),
        });
    }
    let record = service
        .sessions()
        .into_iter()
        .find(|candidate| candidate.id == record.id)
        .unwrap_or(record);
    CommandOutcome::Success {
        value: json!({"terminal":terminal,"native":record,"first_prompt_error":first_prompt_error}),
        warnings: tools_warnings,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Placement uses the accepted command lifetime and exact mux target"
)]
fn launch_native_record(
    service: &NativeAgentService,
    captured: &NativeLaunch,
    invocation: &CommandInvocation,
    config: NativeSessionConfig,
    tools: Option<Arc<ToolBridge>>,
    terminal_target: &CommandTarget,
    spawn_parent: Option<&CommandTarget>,
    attachment_paths: &[std::path::PathBuf],
    commands: &Arc<dyn AgentCommandExecutor>,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> Result<NativeSessionRecord, bootty_agents::NativeCreationError> {
    let binding_id = captured.context.binding_id.as_str();
    let arg = |index| invocation.arguments.get(index).cloned().unwrap_or_default();
    if let Some(tools) = &tools {
        bind_native_tools(
            captured,
            tools,
            terminal_target.clone(),
            &AgentLaunch {
                program: config.program.clone(),
                cwd: Some(config.cwd.to_string_lossy().into_owned()),
                arguments: config.arguments.clone(),
                ephemeral: false,
                account_directory: config.account_directory.clone(),
            },
        )?;
    }
    let place = |record: &NativeSessionRecord| {
        let placed = register_native_panel(
            commands,
            record,
            &json!(terminal_target),
            invocation.caller,
            deadline,
            cancellation,
        );
        match placed {
            CommandOutcome::Success { .. } => Ok(()),
            outcome => Err(crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "Could not place the conversation".into())),
        }
    };
    if let (Some(parent), Some(tools)) = (spawn_parent, tools.as_ref()) {
        return service.create_spawned_for_task_placed(
            parent,
            &arg(6),
            &arg(7),
            Arc::clone(tools),
            place,
        );
    }
    service.create_for_task_placed(
        binding_id,
        &arg(6),
        &arg(7),
        config,
        tools,
        &arg(8),
        attachment_paths,
        place,
    )
}

fn register_native_panel(
    commands: &Arc<dyn AgentCommandExecutor>,
    record: &NativeSessionRecord,
    terminal: &Value,
    caller: Caller,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> CommandOutcome {
    let mut invocation = CommandInvocation::new(
        "agents.native.panel",
        vec![
            record.id.clone(),
            record.generation.to_string(),
            terminal.to_string(),
        ],
        caller,
    );
    invocation.target = Some(record.target());
    commands.execute(invocation, deadline, cancellation.clone())
}

#[expect(
    clippy::too_many_arguments,
    reason = "The nested command preserves its caller and exact captured backend target"
)]
fn create_native_panel_terminal(
    commands: &Arc<dyn AgentCommandExecutor>,
    parent: &CommandTarget,
    session: Option<&CommandTarget>,
    cwd: &std::path::Path,
    direction: Option<&str>,
    caller: Caller,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> Result<(Value, Vec<CommandWarning>), CommandOutcome> {
    let mut arguments = vec!["[]".to_owned(), cwd.to_string_lossy().into_owned()];
    let operation = direction.map_or("terminal.create_tab", |direction| {
        arguments.insert(0, direction.to_owned());
        "terminal.create_pane"
    });
    let mut invocation = CommandInvocation::new(operation, arguments, caller);
    invocation.target = Some(if direction.is_some() {
        parent.clone()
    } else {
        session
            .cloned()
            .ok_or_else(|| failure("The captured task has no issued backend session".into()))?
    });
    match commands.execute(invocation, deadline, cancellation.clone()) {
        CommandOutcome::Success { value, warnings } => {
            let created = value
                .get("created")
                .cloned()
                .ok_or_else(|| failure("The backend did not report its created pane".into()))?;
            Ok((created, warnings))
        }
        outcome => Err(outcome),
    }
}

fn initial_native_input(
    service: &NativeAgentService,
    record: &NativeSessionRecord,
    message: &str,
    attachment_paths: &[std::path::PathBuf],
    invocation: &CommandInvocation,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> Option<String> {
    let mut attachment_ids = Vec::with_capacity(attachment_paths.len());
    if record.pending_initial_message.is_some() {
        attachment_ids.extend(
            record
                .attachments
                .iter()
                .map(|reference| reference.id.clone()),
        );
    } else {
        for path in attachment_paths {
            match service.import_attachment(&record.target(), path) {
                Ok(reference) => attachment_ids.push(reference.id),
                Err(error) => return Some(format!("Initial attachment import failed: {error}")),
            }
        }
    }
    if message.trim().is_empty() && attachment_ids.is_empty() {
        return None;
    }
    service
        .resolve_prompt_attachments_with(
            &record.target(),
            &attachment_ids,
            &native_input_runner(deadline, cancellation),
        )
        .and_then(|attachments| {
            bootty_agents::NativePrompt::new_with_context(
                message.to_owned(),
                Vec::new(),
                attachments,
                Vec::new(),
            )
        })
        .and_then(|prompt| {
            let applications = invocation
                .arguments
                .get(12)
                .filter(|value| !value.is_empty())
                .map_or(Ok(Vec::new()), |value| {
                    if invocation.caller != Caller::Internal || value.len() > 32 * 1024 {
                        return Err(
                            "Application mentions require an internal user submission".to_owned()
                        );
                    }
                    serde_json::from_str(value).map_err(|e| e.to_string())
                })?;
            let ranges: Vec<Vec<std::ops::Range<usize>>> = invocation
                .arguments
                .get(13)
                .filter(|value| !value.is_empty())
                .map_or(Ok(Vec::new()), |value| {
                    if value.len() > 32 * 1024 {
                        return Err(
                            "Inline attachment references exceed the prompt limit".to_owned()
                        );
                    }
                    serde_json::from_str(value).map_err(|error| error.to_string())
                })?;
            if !ranges.is_empty() && ranges.len() != attachment_ids.len() {
                return Err("Inline attachments do not match the selected files".into());
            }
            prompt
                .with_attachment_ranges(attachment_ids.iter().cloned().zip(ranges).collect())?
                .with_applications(applications)
        })
        .and_then(|prompt| {
            check_native_input(deadline, cancellation)?;
            service.prompt_input(&record.target(), &prompt).map(|_| ())
        })
        .err()
}

fn native_start_attachment_paths(
    invocation: &CommandInvocation,
) -> Result<Vec<std::path::PathBuf>, String> {
    let Some(encoded) = invocation
        .arguments
        .get(10)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(Vec::new());
    };
    if !matches!(
        invocation.caller,
        Caller::Internal | Caller::CommandPalette | Caller::Keybinding | Caller::BuiltinKeybinding
    ) {
        return Err("Initial attachments require a desktop caller".to_owned());
    }
    if encoded.len() > 16 * 8192 {
        return Err("Initial attachment paths exceed the command limit".to_owned());
    }
    let paths: Vec<std::path::PathBuf> =
        serde_json::from_str(encoded).map_err(|_| "Invalid initial attachment paths")?;
    if paths.len() > bootty_agents::MAX_NATIVE_PROMPT_ATTACHMENTS
        || paths.iter().any(|path| !path.is_absolute())
    {
        return Err("Attach at most 16 files using absolute local paths".to_owned());
    }
    Ok(paths)
}

fn discover_launch_config(
    invocation: &CommandInvocation,
    captured: &NativeLaunch,
) -> Result<NativeSessionConfig, String> {
    let provider = invocation
        .arguments
        .first()
        .and_then(|id| native_provider(id))
        .ok_or("Unsupported native provider")?;
    let launch = super::terminal_agents::prepare_launch(
        &provider_start_invocation(invocation, provider),
        provider,
        "start",
        None,
        &captured.context.preferences,
    )?;
    captured_launch_config(captured, invocation, provider, launch)
}

fn captured_launch_config(
    captured: &NativeLaunch,
    invocation: &CommandInvocation,
    provider: AgentKind,
    launch: AgentLaunch,
) -> Result<NativeSessionConfig, String> {
    let mut config = if let Some(remote) = &captured.remote {
        NativeSessionConfig::capture_remote(provider, launch, remote.clone())
    } else {
        NativeSessionConfig::from_launch(provider, launch)
    }?;
    config.profile = invocation.arguments.get(5).map_or_else(
        || {
            captured
                .context
                .preferences
                .selected_profile()
                .map(|_| captured.context.preferences.selected.clone())
        },
        |profile| Some(profile.clone()).filter(|profile| !profile.is_empty()),
    );
    Ok(config)
}

fn discover_launch_models(
    service: &NativeAgentService,
    invocation: &CommandInvocation,
    captured: &NativeLaunch,
) -> Result<Vec<bootty_agents::NativeModelOption>, String> {
    let config = discover_launch_config(invocation, captured)?;
    let favorite = invocation.command == "agents.native.catalog-favorite";
    let mut models = service.cached_model_catalog(&config).map_or_else(
        || bootty_agents::NativeAgentSession::discover_models(config.clone()),
        Ok,
    )?;
    if favorite {
        let model = invocation
            .arguments
            .get(6)
            .ok_or("Missing favorite model")?;
        if !models.iter().any(|option| option.id == *model) {
            return Err("Selected model is not advertised by this provider account".to_owned());
        }
        service.toggle_model_favorite(&config, model)?;
    }
    service.mark_model_favorites(&config, &mut models);
    service.cache_model_catalog(&config, &models)?;
    Ok(models)
}

fn prepare_native_launch(
    service: &NativeAgentService,
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    captured: &NativeLaunch,
) -> Result<PreparedNativeLaunch, String> {
    let arg = |index| invocation.arguments.get(index).cloned().unwrap_or_default();
    let Some(provider) = native_provider(&arg(0)) else {
        return Err("Unsupported native provider".to_owned());
    };
    let launch_invocation = provider_start_invocation(invocation, provider);
    let mut launch = super::terminal_agents::prepare_launch(
        &launch_invocation,
        provider,
        "start",
        None,
        &captured.context.preferences,
    )?;
    if matches!(
        invocation.command.as_str(),
        "agents.native.tab" | "agents.native.pane"
    ) {
        launch.cwd = Some(arg(1));
    }
    let selection = invocation
        .arguments
        .get(11)
        .filter(|value| !value.is_empty())
        .map(|value| {
            serde_json::from_str::<bootty_agents::NativeModelSelection>(value)
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    let mut config = captured_launch_config(captured, invocation, provider, launch)?;
    config.fast_mode = captured.context.preferences.fast_mode && provider != AgentKind::Pi;
    if let Some(selection) = &selection {
        let options = if let Some(options) = service.cached_model_catalog(&config) {
            options
        } else {
            let options = bootty_agents::NativeAgentSession::discover_models(config.clone())?;
            service.cache_model_catalog(&config, &options)?;
            options
        };
        let option = options
            .iter()
            .find(|option| option.id == selection.model)
            .ok_or("Selected model is not advertised by this provider account")?;
        if selection
            .reasoning_effort
            .as_ref()
            .is_some_and(|effort| !option.reasoning_efforts.contains(effort))
        {
            return Err("Selected reasoning effort is not supported by this model".to_owned());
        }
    }
    let launch = AgentLaunch {
        program: config.program.clone(),
        cwd: Some(config.cwd.to_string_lossy().into_owned()),
        arguments: config.arguments.clone(),
        ephemeral: false,
        account_directory: config.account_directory.clone(),
    };
    let (tools, warnings) = native_tools(commands, &launch_invocation, captured, &config, &launch)?;
    if let Some(selection) = selection {
        config.model = Some(selection.model);
        config.reasoning_effort = selection.reasoning_effort;
    }
    if let Some(mode) = invocation.arguments.get(14).filter(|mode| !mode.is_empty()) {
        config.permissions = mode.parse()?;
        if !config.permissions.supports(provider)
            || invocation.caller == Caller::Luau
                && config.permissions != bootty_agents::NativePermissionMode::ProviderDefault
        {
            return Err(
                "The selected provider permission mode is unavailable for this caller".into(),
            );
        }
    }

    if let Some(expected) = invocation
        .arguments
        .get(9)
        .filter(|expected| !expected.is_empty())
        && config
            .account_directory
            .as_deref()
            .map(std::path::Path::new)
            != Some(std::path::Path::new(expected))
    {
        return Err(
            "The provider account changed after capture; choose the task account again".to_owned(),
        );
    }
    Ok(PreparedNativeLaunch {
        config,
        tools,
        warnings,
        spawn_parent: None,
    })
}

pub(super) fn native_provider(id: &str) -> Option<AgentKind> {
    AgentKind::ALL.into_iter().find(|provider| {
        provider.to_string() == id && NativeSessionConfig::supports_provider(*provider)
    })
}

pub(super) fn provider_start_invocation(
    invocation: &CommandInvocation,
    provider: AgentKind,
) -> CommandInvocation {
    CommandInvocation::new(
        format!("{}.start", provider.module()),
        invocation
            .arguments
            .iter()
            .skip(1)
            .take(5)
            .cloned()
            .collect(),
        invocation.caller,
    )
}

fn native_tools(
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    captured: &NativeLaunch,
    config: &NativeSessionConfig,
    launch: &AgentLaunch,
) -> Result<(Option<Arc<ToolBridge>>, Vec<CommandWarning>), String> {
    let binding = captured
        .context
        .binding
        .clone()
        .ok_or("Native tools require the captured Binding")?;
    let executable = std::env::current_exe().map_err(|_| "Bootty tool executable unavailable")?;
    launch.validate()?;
    let spawn = (captured.context.allow_spawn && launch.account_directory.is_some()).then(|| {
        bootty_agents::ToolSpawnContext {
            profile: config.profile.clone(),
        }
    });
    let bridge = ToolBridge::prepare(
        bootty_agents::ToolBridgeContext {
            scope: bootty_agents::ToolScope {
                provider: config.provider,
                binding,
            },
            caller: invocation.caller,
            policy: bootty_agents::ToolPolicy {
                spawn_children: spawn.is_some(),
                // Catalog availability grants nothing until a user attaches an exact document.
                browser_capture: true,
                computer_capture: captured.context.computer_capture.is_some(),
                ..bootty_agents::ToolPolicy::own_terminal()
            },
            captures: captured.context.computer_capture.iter().cloned().collect(),
            spawn,
        },
        &executable,
        Arc::clone(commands),
    )?;
    let tools = captured
        .tools_owner
        .retain_tool_attachment(config.provider, bridge)?;
    Ok((Some(tools), Vec::new()))
}

fn bind_native_tools(
    captured: &NativeLaunch,
    tools: &ToolBridge,
    terminal: CommandTarget,
    launch: &AgentLaunch,
) -> Result<(), String> {
    let binding = captured
        .context
        .binding
        .as_ref()
        .ok_or("Native tools require the captured Binding")?;
    tools.lease().bind(binding, terminal)?;
    captured
        .tools_owner
        .retain_native_parent(tools, &captured.context.binding_id, launch.clone())
}

fn create_task_shell(
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> Result<(Value, Vec<bootty_control::CommandWarning>), CommandOutcome> {
    let arg = |index| invocation.arguments.get(index).cloned().unwrap_or_default();
    let mut shell = CommandInvocation::new(
        "session.create",
        vec![arg(4), arg(1), "[]".to_owned(), arg(6), arg(7)],
        invocation.caller,
    );
    shell.target.clone_from(&invocation.target);
    match commands.execute_pending(shell, deadline, cancellation.clone()) {
        CommandOutcome::Success { value, warnings } => {
            let terminal = value.get("terminal").cloned().unwrap_or(Value::Null);
            if serde_json::from_value::<CommandTarget>(terminal.clone()).is_err() {
                return Err(failure(
                    "Created shell did not report its exact terminal target".to_owned(),
                ));
            }
            Ok((terminal, warnings))
        }
        outcome => Err(outcome),
    }
}

fn invoke(
    service: &NativeAgentService,
    invocation: &CommandInvocation,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> CommandOutcome {
    let Some(target) = invocation.target.as_ref() else {
        return failure("Missing native session target".to_owned());
    };
    let arg = |index| invocation.arguments.get(index).map_or("", String::as_str);
    let result = match invocation
        .command
        .strip_prefix("agents.native.")
        .unwrap_or_default()
    {
        "prompt" => resolve_native_prompt(service, invocation, deadline, cancellation)
            .and_then(|prompt| {
                check_native_input(deadline, cancellation)?;
                service.prompt_input(target, &prompt)
            })
            .and_then(|snapshot| serde_json::to_value(snapshot).map_err(|error| error.to_string())),
        "import" if invocation.caller == Caller::Internal => service
            .import_attachment(target, std::path::Path::new(arg(2)))
            .and_then(|reference| {
                serde_json::to_value(reference).map_err(|error| error.to_string())
            }),
        "import" => Err("Native attachment imports require an internal caller".to_owned()),
        "attachment-preview" if invocation.caller == Caller::Internal => service
            .preview_attachment(target, arg(2))
            .map(|png| json!({"png": BASE64.encode(png)})),
        "attachment-preview" => {
            Err("Native attachment previews require an internal caller".to_owned())
        }
        "subagent-read" => service
            .read_subagent(target, arg(2))
            .and_then(|detail| serde_json::to_value(detail).map_err(|error| error.to_string())),
        "history" => service
            .read_history(target, arg(2))
            .and_then(|page| serde_json::to_value(page).map_err(|error| error.to_string())),
        "interrupt" => service.interrupt(target).map(|()| Value::Null),
        "stop" => service.stop(target).map(|()| Value::Null),
        "rename" => invocation.arguments.get(3).map_or_else(
            || service.rename(target, arg(2)).map(|()| Value::Bool(true)),
            |expected| {
                service
                    .rename_if_unchanged(target, expected, arg(2))
                    .map(Value::Bool)
            },
        ),
        "completions" => service
            .completions(target)
            .and_then(|catalog| serde_json::to_value(catalog).map_err(|e| e.to_string())),
        "models" => service
            .models(target)
            .and_then(|models| serde_json::to_value(models).map_err(|error| error.to_string())),
        "provider" => service
            .provider_info(target)
            .and_then(|info| serde_json::to_value(info).map_err(|error| error.to_string())),
        "status" => service
            .activity(target)
            .and_then(|activity| serde_json::to_value(activity).map_err(|error| error.to_string())),
        "activity" => arg(2)
            .parse()
            .map_err(|_| "Activity limit must be between 1 and 32".to_owned())
            .and_then(|limit| service.recent_activity(target, limit))
            .and_then(|page| serde_json::to_value(page).map_err(|error| error.to_string())),
        "favorite" => service
            .favorite_model(target, arg(2))
            .and_then(|models| serde_json::to_value(models).map_err(|error| error.to_string())),
        "configure" => serde_json::from_str::<bootty_agents::NativeModelSelection>(arg(2))
            .map_err(|error| error.to_string())
            .and_then(|selection| service.configure(target, &selection))
            .and_then(|config| serde_json::to_value(config).map_err(|error| error.to_string())),

        "approve" => bootty_agents::NativeApprovalDecision::ALL
            .into_iter()
            .find(|decision| decision.id() == arg(3))
            .ok_or_else(|| "Unknown approval decision".to_owned())
            .and_then(|decision| service.approve_decision(target, arg(2), decision))
            .map(|()| Value::Null),
        "respond" => serde_json::from_str(arg(3))
            .map_err(|error| error.to_string())
            .and_then(|response| service.respond(target, arg(2), response))
            .map(|()| Value::Null),
        "browser-attach" => {
            if arg(2).len() > 16 * 1024 {
                return failure("Browser attachment exceeds the input limit".into());
            }
            serde_json::from_str::<Option<bootty_agents::NativeBrowserAttachment>>(arg(2))
                .map_err(|_| "Invalid browser attachment".to_owned())
                .and_then(|attachment| {
                    check_native_input(deadline, cancellation)?;
                    if !cancellation.try_start() {
                        return Err("Browser attachment was cancelled".into());
                    }
                    service.attach_browser(target, attachment)
                })
                .map(|()| Value::Null)
        }
        _ => Err("Unknown native conversation operation".to_owned()),
    };
    match result {
        Ok(value) => serialized_command_outcome(value),
        Err(error) => failure(error),
    }
}

fn resolve_native_prompt(
    service: &NativeAgentService,
    invocation: &CommandInvocation,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> Result<bootty_agents::NativePrompt, String> {
    let text = invocation.arguments.get(2).cloned().unwrap_or_default();
    let mut images = Vec::new();
    if let Some(encoded) = invocation
        .arguments
        .get(3)
        .filter(|encoded| !encoded.is_empty())
    {
        if encoded.len() > 64 * 1024 {
            return Err("Image context exceeds the prompt limit".into());
        }
        let records: Vec<bootty_browser::Annotation> =
            serde_json::from_str(encoded).map_err(|_| "Invalid annotation image references")?;
        if records.is_empty() || records.len() > 4 {
            return Err("Include one to four annotation images".into());
        }
        let target = invocation
            .target
            .as_ref()
            .ok_or("Missing conversation target")?;
        let store = bootty_browser::AnnotationStore::new(
            &crate::gpui_workspace::browser_profile_directory(),
        );
        let mut ids = std::collections::BTreeSet::new();
        let mut total = 0usize;
        for record in records {
            if !ids.insert(record.id) {
                return Err("Duplicate annotation image".into());
            }
            let image = record.image.as_ref().ok_or("Annotation has no image")?;
            let png = store
                .load_image(&record, &target.handle)
                .map_err(|error| error.to_string())?;
            total = total
                .checked_add(png.len())
                .filter(|total| *total <= 8 * 1024 * 1024)
                .ok_or("Images exceed the prompt limit")?;
            images.push(bootty_agents::NativePromptImage::from_host_png(
                bootty_agents::NativeImageReference {
                    id: image.id.clone(),
                    pixel_width: image.pixel_width,
                    pixel_height: image.pixel_height,
                },
                png,
            )?);
        }
    }
    let target = invocation
        .target
        .as_ref()
        .ok_or("Missing conversation target")?;
    let native_attachments = if let Some(encoded) = invocation
        .arguments
        .get(4)
        .filter(|encoded| !encoded.is_empty())
    {
        if encoded.len() > 16 * 1024 {
            return Err("Native attachment references exceed the prompt limit".to_owned());
        }
        let ids: Vec<String> =
            serde_json::from_str(encoded).map_err(|_| "Invalid native attachment references")?;
        if ids.len() > bootty_agents::MAX_NATIVE_PROMPT_ATTACHMENTS {
            return Err("Attach at most 16 files to a prompt".to_owned());
        }
        service.resolve_prompt_attachments_with(
            target,
            &ids,
            &native_input_runner(deadline, cancellation),
        )?
    } else {
        service.resolve_prompt_attachments(target, &[])?
    };
    let citations = if let Some(encoded) = invocation
        .arguments
        .get(5)
        .filter(|encoded| !encoded.is_empty())
    {
        if encoded.len() > 64 * 1024 {
            return Err("Response citations exceed the prompt limit".to_owned());
        }
        serde_json::from_str(encoded).map_err(|_| "Invalid response citations")?
    } else {
        Vec::new()
    };
    let prompt =
        bootty_agents::NativePrompt::new_with_context(text, images, native_attachments, citations)?;
    append_prompt_applications(prompt, invocation)
}

fn native_input_runner(
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> bootty_host::CancellableCommandRunner {
    let cancellation = cancellation.clone();
    bootty_host::CancellableCommandRunner::with_deadline_and_cancellation_check(
        bootty_host::CommandCancellation::default(),
        deadline,
        move || cancellation.is_cancelled(),
    )
}

fn check_native_input(deadline: Instant, cancellation: &CommandCancellation) -> Result<(), String> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Err("Input submission was cancelled before reaching the provider".into());
    }
    Ok(())
}

fn failure(message: String) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "native_agent_failed".to_owned(),
        message,
    }
}

fn query_launch_metadata(
    service: &NativeAgentService,
    invocation: &CommandInvocation,
    captured: &NativeLaunch,
    deadline: Instant,
    cancellation: &CommandCancellation,
) -> CommandOutcome {
    if cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    if Instant::now() >= deadline {
        return CommandOutcome::deadline_exceeded();
    }
    let result = (|| {
        if invocation.command == "agents.native.names" {
            let config = discover_launch_config(invocation, captured)?;
            let names = bootty_agents::generate_session_names(
                &config,
                &captured.quick_model,
                invocation.arguments.get(6).map_or("", String::as_str),
                || cancellation.is_cancelled() || Instant::now() >= deadline,
            )?;
            serde_json::to_value(names).map_err(|error| error.to_string())
        } else if invocation.command == "agents.native.catalog-completions" {
            discover_launch_config(invocation, captured)
                .and_then(bootty_agents::NativeAgentSession::discover_completions)
                .and_then(|catalog| serde_json::to_value(catalog).map_err(|e| e.to_string()))
        } else {
            discover_launch_models(service, invocation, captured)
                .and_then(|models| serde_json::to_value(models).map_err(|e| e.to_string()))
        }
    })();
    result.map_or_else(failure, serialized_command_outcome)
}

fn configure_native_permissions(
    service: &NativeAgentService,
    target: &CommandTarget,
    mode: bootty_agents::NativePermissionMode,
    execution: (Instant, &CommandCancellation),
) -> Option<CommandOutcome> {
    let (deadline, cancellation) = execution;
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        return Some(failure("Permission change was cancelled or expired".into()));
    }
    match service.configure_permissions(target, mode) {
        Ok(bootty_agents::NativePermissionUpdate::Stopped) => None,
        Ok(bootty_agents::NativePermissionUpdate::Queued) => Some(
            service
                .sessions()
                .into_iter()
                .find(|record| record.target() == *target)
                .map_or_else(
                    || failure("Conversation target changed".into()),
                    serialized_command_outcome,
                ),
        ),
        Err(error) => Some(failure(error)),
    }
}

fn append_prompt_applications(
    prompt: bootty_agents::NativePrompt,
    invocation: &CommandInvocation,
) -> Result<bootty_agents::NativePrompt, String> {
    let applications = invocation
        .arguments
        .get(6)
        .filter(|value| !value.is_empty())
        .map_or(Ok(Vec::new()), |value| {
            if invocation.caller != Caller::Internal || value.len() > 32 * 1024 {
                return Err("Application mentions require an internal user submission".to_owned());
            }
            serde_json::from_str(value).map_err(|e| e.to_string())
        })?;
    let authored_bytes = invocation
        .arguments
        .get(7)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || Ok(prompt.message().len()),
            |value| {
                if invocation.caller != Caller::Internal {
                    return Err("Authored ranges require an internal user submission".to_owned());
                }
                value.parse::<usize>().map_err(|e| e.to_string())
            },
        )?;
    let ranges = invocation
        .arguments
        .get(8)
        .filter(|value| !value.is_empty())
        .map_or(Ok(std::collections::BTreeMap::new()), |value| {
            if value.len() > 32 * 1024 {
                return Err("Inline attachment references exceed the prompt limit".to_owned());
            }
            serde_json::from_str(value).map_err(|error| error.to_string())
        })?;
    prompt
        .with_attachment_ranges(ranges)?
        .with_applications(applications)?
        .with_authored_prefix(authored_bytes)
}
