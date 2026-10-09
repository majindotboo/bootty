use std::{
    task::{Poll, ready},
    time::Instant,
};

use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use bootty_mux::{controller::SpaceId, executor};
use serde_json::json;

use super::{
    CommandDispatch, PendingAppCommand, PendingCommandResult, command_outcome_for_mux_error,
    serialized_command_outcome,
};
use crate::{
    commands::{ExactMuxTarget, SurfaceCommand},
    state::{AppEffect, AppState, ViewportSnapshot},
    surface_creation::{PendingNewSurface, SurfaceParent, SurfacePlacement},
};

fn stale(message: &str) -> CommandOutcome {
    CommandOutcome::StaleTarget {
        message: message.to_owned(),
    }
}

fn surface_invalid(message: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: message.to_owned(),
    }
}

impl AppState {
    pub const fn pending_new_surface(&self) -> Option<&PendingNewSurface> {
        self.pending_new_surface.as_ref()
    }

    pub(super) fn open_surface_chooser(
        &mut self,
        invocation: &CommandInvocation,
        placement: SurfacePlacement,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let captured = self.capture_surface_destination(invocation, placement);
        let mut request = match captured {
            Ok(request) => request,
            Err(outcome) => return self.reject_command(outcome),
        };
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return self.reject_command(command_outcome_for_mux_error(error));
        }
        if !self.modal_dialog_dismissible() {
            return self.reject_command(CommandOutcome::Unavailable {
                message: "Session creation is already in progress".to_owned(),
            });
        }
        if let Some(previous) = &self.pending_new_surface
            && !self.cancel_surface_creation(previous.id)
        {
            return self.reject_command(CommandOutcome::Unavailable {
                message: "The current surface has been accepted; wait for creation to finish"
                    .to_owned(),
            });
        }
        let Some(next) = self.next_surface_request_id.checked_add(1) else {
            return self.reject_command(CommandOutcome::Unavailable {
                message: "New surface request ids are exhausted".to_owned(),
            });
        };
        request.id = self.next_surface_request_id;
        self.next_surface_request_id = next;
        self.pending_surface_caller = Some(invocation.caller);
        if let Some(previous) = self.pending_new_surface.replace(request.clone()) {
            effects.push(AppEffect::CloseSurfaceChooser(previous.id));
        }
        self.dismiss_session_creation();
        effects.push(AppEffect::OpenSurfaceChooser(request.clone()));
        CommandDispatch::Complete(serialized_command_outcome(
            json!({"request_id": request.id}),
        ))
    }

    fn capture_surface_destination(
        &self,
        invocation: &CommandInvocation,
        placement: SurfacePlacement,
    ) -> Result<PendingNewSurface, CommandOutcome> {
        if let Some(target) = &invocation.target
            && let Some(record) = self.native_agent_service().and_then(|service| {
                service.activities().into_iter().find(|record| {
                    target.kind == ResourceKind::Session
                        && record.id == target.handle
                        && record.generation == target.generation
                })
            })
        {
            let binding = self
                .workspace
                .all_bindings()
                .find(|binding| {
                    binding.scope().persistence_value().to_string() == record.binding_id
                })
                .ok_or_else(|| stale("The conversation Space was closed"))?;
            let identity = record
                .task_identity
                .ok_or_else(|| stale("The conversation has no saved task"))?;
            let saved = binding
                .sessions()
                .get(&identity)
                .filter(|saved| !saved.state.deleted)
                .ok_or_else(|| stale("The conversation task is unavailable"))?;
            return Ok(PendingNewSurface {
                id: 0,
                binding: self.surface_binding_target(binding.scope())?,
                task_identity: identity,
                cwd: saved.cwd.clone(),
                parent: SurfaceParent::Conversation(target.clone()),
                placement,
            });
        }
        let expected = invocation
            .target
            .as_ref()
            .map_or(ResourceKind::Terminal, |target| target.kind);
        if !matches!(
            expected,
            ResourceKind::Binding
                | ResourceKind::Session
                | ResourceKind::MuxWindow
                | ResourceKind::Pane
                | ResourceKind::Terminal
        ) {
            return Err(stale(
                "The new surface parent is not a terminal, conversation, or Space",
            ));
        }
        let (_, exact) = self.resolve_command_target(
            &invocation.command,
            Some(expected),
            invocation.target.as_ref(),
        )?;
        let exact = exact.ok_or_else(|| stale("The surface parent is unavailable"))?;
        let binding = self.surface_binding_target(exact.scope())?;
        let (session, _, _) = exact.ids();
        let identity = session
            .and_then(|session| self.workspace.session_identity(exact.scope(), session))
            .unwrap_or_default();
        let cwd = self
            .agent_launch_context(&exact)
            .cwd
            .unwrap_or_else(|| crate::state::default_session_cwd(self.config()));
        let parent = if matches!(exact, ExactMuxTarget::Binding(_)) {
            SurfaceParent::Binding(binding.clone())
        } else {
            let runtime = self
                .workspace
                .binding(exact.scope())
                .ok_or_else(|| stale("The Space was closed"))?;
            let handle =
                self.binding_target_handle(exact.scope(), runtime.mux().binding_generation());
            SurfaceParent::Terminal(
                exact
                    .command_target(ResourceKind::Terminal, runtime.mux(), &handle)
                    .ok_or_else(|| stale("The terminal parent is unavailable"))?,
            )
        };
        Ok(PendingNewSurface {
            id: 0,
            binding,
            task_identity: identity,
            cwd,
            parent,
            placement,
        })
    }

    fn surface_binding_target(&self, scope: SpaceId) -> Result<CommandTarget, CommandOutcome> {
        let runtime = self
            .workspace
            .binding(scope)
            .ok_or_else(|| stale("The Space was closed"))?;
        let handle = self.binding_target_handle(scope, runtime.mux().binding_generation());
        ExactMuxTarget::Binding(scope)
            .command_target(ResourceKind::Binding, runtime.mux(), &handle)
            .ok_or_else(|| stale("The Space destination is unavailable"))
    }

    pub(crate) fn validate_surface_request(
        &self,
        request: &PendingNewSurface,
    ) -> Result<SpaceId, CommandOutcome> {
        let (_, exact) = self.resolve_command_target(
            "session.saved",
            Some(ResourceKind::Binding),
            Some(&request.binding),
        )?;
        let scope = exact
            .ok_or_else(|| stale("The Space destination is unavailable"))?
            .scope();
        if !request.task_identity.is_empty() {
            let saved = self
                .workspace
                .binding(scope)
                .and_then(|binding| binding.sessions().get(&request.task_identity))
                .filter(|saved| !saved.state.deleted)
                .ok_or_else(|| stale("The captured task was removed"))?;
            if matches!(request.parent, SurfaceParent::Conversation(_)) && saved.cwd != request.cwd
            {
                return Err(stale("The captured task directory changed"));
            }
        }
        match &request.parent {
            SurfaceParent::Terminal(target) => {
                self.resolve_command_target(
                    "terminal.create_pane",
                    Some(ResourceKind::Terminal),
                    Some(target),
                )?;
            }
            SurfaceParent::Conversation(target) => {
                let valid = self.native_agent_service().is_some_and(|service| {
                    service.activities().iter().any(|record| {
                        target.kind == ResourceKind::Session
                            && record.id == target.handle
                            && record.generation == target.generation
                            && record.binding_id == scope.persistence_value().to_string()
                            && record.task_identity.as_deref()
                                == Some(request.task_identity.as_str())
                    })
                });
                if !valid {
                    return Err(stale("The captured conversation changed"));
                }
            }
            SurfaceParent::Binding(_) => {}
        }
        Ok(scope)
    }

    fn dispatch_surface_navigation(
        &mut self,
        action: crate::commands::SurfaceChooserAction,
        caller: Caller,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(request) = self.pending_new_surface.as_ref() else {
            return self.reject_command(CommandOutcome::Unavailable {
                message: "No new tab or split chooser is open".to_owned(),
            });
        };
        if !matches!(
            caller,
            Caller::Internal | Caller::Keybinding | Caller::CommandPalette
        ) && self.pending_surface_caller != Some(caller)
        {
            return self.reject_command(CommandOutcome::Denied {
                message: "The chooser belongs to another command caller".to_owned(),
            });
        }
        if let Err(error) = executor::begin_synchronous_command(execution) {
            return self.reject_command(command_outcome_for_mux_error(error));
        }
        effects.push(AppEffect::NavigateSurfaceChooser {
            id: request.id,
            action,
        });
        CommandDispatch::Complete(CommandOutcome::success())
    }

    pub(super) fn dispatch_surface_command(
        &mut self,
        command: SurfaceCommand,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if invocation.target.is_some() {
            return self.reject_command(CommandOutcome::Denied {
                message: "Surface commands use their captured request destination".to_owned(),
            });
        }
        let id = match &command {
            SurfaceCommand::Navigate(action) => {
                return self.dispatch_surface_navigation(
                    *action,
                    invocation.caller,
                    effects,
                    execution,
                );
            }
            SurfaceCommand::Choose { id, .. }
            | SurfaceCommand::CreateAgent { id, .. }
            | SurfaceCommand::Cancel(id) => *id,
        };
        let Some(request) = self
            .pending_new_surface
            .clone()
            .filter(|request| request.id == id)
        else {
            return self.reject_command(stale("The new surface chooser was closed or replaced"));
        };
        let Some(caller) = self
            .pending_surface_caller
            .filter(|caller| *caller == invocation.caller || invocation.caller == Caller::Internal)
        else {
            return self.reject_command(CommandOutcome::Denied {
                message: "The chooser belongs to another command caller".to_owned(),
            });
        };
        if matches!(command, SurfaceCommand::Cancel(_)) {
            if let Err(error) = executor::begin_synchronous_command(execution) {
                return self.reject_command(command_outcome_for_mux_error(error));
            }
            if !self.cancel_surface_creation(id) {
                return self.reject_command(CommandOutcome::Unavailable {
                    message: "The surface has been accepted; wait for creation to finish"
                        .to_owned(),
                });
            }
            self.pending_new_surface = None;
            self.close_surface_agent_form(id);
            effects.push(AppEffect::CloseSurfaceChooser(id));
            return CommandDispatch::Complete(CommandOutcome::success());
        }
        let scope = match self.validate_surface_request(&request) {
            Ok(scope) => scope,
            Err(outcome) => return self.reject_command(outcome),
        };
        if self
            .creating_surface_request
            .as_ref()
            .is_some_and(|(creating, _)| *creating == id)
        {
            return self.reject_command(CommandOutcome::Unavailable {
                message: "This surface is already being created".to_owned(),
            });
        }
        if let Err(outcome) = self.prevalidate_surface_choice(&request, scope, &command, caller) {
            return self.reject_command(outcome);
        }
        if let SurfaceCommand::Choose { kind, .. } = &command
            && kind == "agent"
        {
            if let Err(error) = executor::begin_synchronous_command(execution) {
                return self.reject_command(command_outcome_for_mux_error(error));
            }
            effects.push(AppEffect::OpenSurfaceAgentForm(request));
            return CommandDispatch::Complete(CommandOutcome::success());
        }
        if matches!(request.parent, SurfaceParent::Conversation(_))
            && self
                .workspace
                .binding(scope)
                .is_some_and(|binding| binding.session_attachment(&request.task_identity).is_none())
        {
            return self.restore_surface_for_choice(
                &request, command, caller, viewport, effects, execution,
            );
        }
        let mut delegated = match self.surface_choice_invocation(&request, scope, command, caller) {
            Ok(delegated) => delegated,
            Err(outcome) => return self.reject_command(outcome),
        };
        delegated.confirmation = invocation.confirmation;
        self.create_surface_from_invocation(id, delegated, viewport, effects, execution)
    }

    fn prevalidate_surface_choice(
        &self,
        request: &PendingNewSurface,
        scope: SpaceId,
        command: &SurfaceCommand,
        caller: Caller,
    ) -> Result<(), CommandOutcome> {
        let (invocation, provider, operation) = match command {
            SurfaceCommand::Choose { kind, .. } if kind == "agent" => return Ok(()),
            SurfaceCommand::Choose { kind, argv, .. } if kind == "terminal" => {
                let arguments = vec![
                    argv.clone().unwrap_or_else(|| "[]".to_owned()),
                    request.cwd.clone(),
                ];
                super::sessions::tab_create_arguments(&arguments)?;
                return self
                    .commands
                    .catalog
                    .resolve(CommandInvocation::new(
                        "terminal.create_tab",
                        arguments,
                        caller,
                    ))
                    .map(|_| ());
            }
            SurfaceCommand::Choose { kind, argv, .. } if kind == "profile" => {
                let provider = argv
                    .as_ref()
                    .and_then(|name| {
                        bootty_agents::AgentKind::ALL
                            .into_iter()
                            .find(|provider| provider.to_string() == *name)
                    })
                    .ok_or_else(|| surface_invalid("Choose an installed terminal provider"))?;
                (
                    CommandInvocation::new(
                        format!("{}.tab", provider.module()),
                        vec![request.cwd.clone()],
                        caller,
                    ),
                    provider,
                    "tab",
                )
            }
            SurfaceCommand::CreateAgent { .. } => {
                let invocation =
                    self.surface_choice_invocation(request, scope, command.clone(), caller)?;
                self.commands.catalog.resolve(invocation.clone())?;
                let provider = invocation
                    .arguments
                    .first()
                    .and_then(|id| super::native_agents::native_provider(id))
                    .ok_or_else(|| surface_invalid("Choose a native provider"))?;
                (
                    super::native_agents::provider_start_invocation(&invocation, provider),
                    provider,
                    "start",
                )
            }
            _ => {
                return Err(surface_invalid(
                    "Choose terminal, agent or a provider profile",
                ));
            }
        };
        self.commands.catalog.resolve(invocation.clone())?;
        let preferences = self
            .config()
            .agents
            .provider(&provider.to_string())
            .filter(|preferences| preferences.enabled)
            .ok_or_else(|| CommandOutcome::Denied {
                message: "This provider is disabled in Settings".to_owned(),
            })?;
        super::terminal_agents::prepare_launch(
            &invocation,
            provider,
            operation,
            Some(&request.cwd),
            preferences,
        )
        .map(|_| ())
        .map_err(|message| surface_invalid(&message))
    }

    fn surface_choice_invocation(
        &self,
        request: &PendingNewSurface,
        scope: SpaceId,
        command: SurfaceCommand,
        caller: Caller,
    ) -> Result<CommandInvocation, CommandOutcome> {
        match command {
            SurfaceCommand::Choose { kind, argv, .. } if kind == "profile" => {
                self.surface_profile_invocation(request, scope, argv, caller)
            }
            SurfaceCommand::Choose { kind, argv, .. } if kind == "terminal" => self
                .surface_shell_invocation(
                    request,
                    scope,
                    argv.unwrap_or_else(|| "[]".to_owned()),
                    caller,
                ),
            SurfaceCommand::CreateAgent { arguments, .. } => {
                if arguments.get(1) != Some(&request.cwd)
                    || (!request.task_identity.is_empty()
                        && arguments.get(6) != Some(&request.task_identity))
                {
                    return Err(stale("The agent form no longer matches its captured task"));
                }
                let operation = if request.task_identity.is_empty() {
                    "agents.native.start"
                } else {
                    "agents.native.tab"
                };
                let mut delegated = CommandInvocation::new(operation, arguments, caller);
                delegated.target = Some(request.binding.clone());
                Ok(delegated)
            }
            _ => Err(CommandOutcome::Failed {
                code: "invalid_arguments".to_owned(),
                message: "Choose terminal, agent or a provider profile".to_owned(),
            }),
        }
    }

    fn surface_terminal_destination(
        &self,
        request: &PendingNewSurface,
        scope: SpaceId,
    ) -> Result<(Option<&'static str>, CommandTarget), CommandOutcome> {
        if let SurfacePlacement::Split(direction) = request.placement {
            let target = match &request.parent {
                SurfaceParent::Terminal(target) => target.clone(),
                SurfaceParent::Conversation(target) => self
                    .native_agent_service()
                    .and_then(|service| {
                        service
                            .sessions()
                            .into_iter()
                            .find(|record| record.target() == *target)
                    })
                    .and_then(|record| self.native_panel_target(&record))
                    .map(|(_, target)| target)
                    .ok_or_else(|| stale("The captured conversation pane is unavailable"))?,
                SurfaceParent::Binding(_) => {
                    return Err(stale("A split requires an existing pane"));
                }
            };
            let direction = match direction {
                bootty_mux::pane_layout::SplitDirection::Right => "right",
                bootty_mux::pane_layout::SplitDirection::Down => "down",
            };
            return Ok((Some(direction), target));
        }
        let session = self
            .workspace
            .binding(scope)
            .and_then(|binding| binding.session_attachment(&request.task_identity))
            .ok_or_else(|| CommandOutcome::Unavailable {
                message: "The saved task has no terminal attachment".to_owned(),
            })?;
        let target = self
            .mux_resource_target(scope, ResourceKind::Session, &session.id, None)
            .ok_or_else(|| stale("The captured terminal session is unavailable"))?;
        Ok((None, target))
    }

    fn surface_profile_invocation(
        &self,
        request: &PendingNewSurface,
        scope: SpaceId,
        provider: Option<String>,
        caller: Caller,
    ) -> Result<CommandInvocation, CommandOutcome> {
        let provider = provider
            .filter(|provider| matches!(provider.as_str(), "claude" | "codex" | "pi"))
            .ok_or_else(|| CommandOutcome::Failed {
                code: "invalid_arguments".to_owned(),
                message: "Choose an installed terminal provider".to_owned(),
            })?;
        if request.task_identity.is_empty() && request.placement == SurfacePlacement::Tab {
            let mut delegated = CommandInvocation::new(
                format!("agents.{provider}.start"),
                vec![
                    request.cwd.clone(),
                    String::new(),
                    String::new(),
                    format!("terminal-{}", request.id),
                ],
                caller,
            );
            delegated.target = Some(request.binding.clone());
            return Ok(delegated);
        }
        let (direction, target) = self.surface_terminal_destination(request, scope)?;
        let mut arguments = vec![request.cwd.clone()];
        let operation = direction.map_or("tab", |direction| {
            arguments.insert(0, direction.to_owned());
            "pane"
        });
        let mut delegated =
            CommandInvocation::new(format!("agents.{provider}.{operation}"), arguments, caller);
        delegated.target = Some(target);
        Ok(delegated)
    }

    fn surface_shell_invocation(
        &self,
        request: &PendingNewSurface,
        scope: SpaceId,
        argv: String,
        caller: Caller,
    ) -> Result<CommandInvocation, CommandOutcome> {
        if request.task_identity.is_empty() && request.placement == SurfacePlacement::Tab {
            let mut delegated = CommandInvocation::new(
                "session.create",
                vec![
                    format!("terminal-{}", request.id),
                    request.cwd.clone(),
                    argv,
                ],
                caller,
            );
            delegated.target = Some(request.binding.clone());
            return Ok(delegated);
        }
        let (direction, target) = self.surface_terminal_destination(request, scope)?;
        let mut arguments = vec![argv, request.cwd.clone()];
        let operation = direction.map_or("terminal.create_tab", |direction| {
            arguments.insert(0, direction.to_owned());
            "terminal.create_pane"
        });
        let mut delegated = CommandInvocation::new(operation, arguments, caller);
        delegated.target = Some(target);
        Ok(delegated)
    }

    fn create_surface_from_invocation(
        &mut self,
        id: u64,
        delegated: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        self.creating_surface_request = Some((id, cancellation.clone()));
        match self.dispatch_command_with_execution(
            delegated,
            viewport,
            effects,
            Some((deadline, cancellation.clone())),
        ) {
            CommandDispatch::Complete(outcome) => {
                match self.finish_surface_creation(id, &outcome, effects) {
                    Ok(()) => CommandDispatch::Complete(outcome),
                    Err(error) => CommandDispatch::Complete(error),
                }
            }
            CommandDispatch::Pending(result) => {
                CommandDispatch::Pending(PendingCommandResult::SurfaceCreation {
                    request_id: id,
                    restored: None,
                    command: Box::new(PendingAppCommand {
                        creation_receipt: None,
                        label: "Create surface".to_owned(),
                        user_initiated_annotation_capture: false,
                        deadline,
                        cancellation,
                        response: None,
                        result,
                    }),
                })
            }
        }
    }

    fn restore_surface_for_choice(
        &mut self,
        request: &PendingNewSurface,
        choice: SurfaceCommand,
        caller: Caller,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        self.creating_surface_request = Some((request.id, cancellation.clone()));
        let mut restore = CommandInvocation::new(
            "session.reopen",
            vec![request.task_identity.clone()],
            caller,
        );
        restore.target = Some(request.binding.clone());
        match self.dispatch_command_with_execution(
            restore,
            viewport,
            effects,
            Some((deadline, cancellation.clone())),
        ) {
            CommandDispatch::Complete(outcome) => self
                .continue_restored_surface(request.id, choice, caller, deadline, outcome, effects),
            CommandDispatch::Pending(result) => {
                CommandDispatch::Pending(PendingCommandResult::SurfaceRestore {
                    request_id: request.id,
                    choice,
                    caller,
                    command: Box::new(PendingAppCommand {
                        creation_receipt: None,
                        label: "Restore captured task".to_owned(),
                        user_initiated_annotation_capture: false,
                        deadline,
                        cancellation,
                        response: None,
                        result,
                    }),
                })
            }
        }
    }

    pub(super) fn poll_surface_restore(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        let PendingCommandResult::SurfaceRestore {
            request_id,
            choice,
            caller,
            command,
        } = &mut pending.result
        else {
            return Poll::Pending;
        };
        let accepted = match &command.result {
            PendingCommandResult::SessionCheckpointCompletion { outcome, .. } => {
                Some(outcome.as_ref().clone())
            }
            _ => None,
        };
        let outcome = ready!(self.poll_pending_app_command(command, now, effects));
        let Some(outcome) = outcome else {
            return Poll::Ready(None);
        };
        if !matches!(outcome, CommandOutcome::Success { .. }) {
            return Poll::Ready(Some(
                self.finish_surface_creation(
                    *request_id,
                    accepted.as_ref().unwrap_or(&outcome),
                    effects,
                )
                .err()
                .unwrap_or(outcome),
            ));
        }
        let dispatch = self.continue_restored_surface(
            *request_id,
            choice.clone(),
            *caller,
            command.deadline,
            outcome,
            effects,
        );
        match dispatch {
            CommandDispatch::Complete(outcome) => Poll::Ready(Some(outcome)),
            CommandDispatch::Pending(result) => {
                pending.result = result;
                Poll::Pending
            }
        }
    }

    fn continue_restored_surface(
        &mut self,
        id: u64,
        choice: SurfaceCommand,
        caller: Caller,
        deadline: Instant,
        outcome: CommandOutcome,
        effects: &mut Vec<AppEffect>,
    ) -> CommandDispatch {
        if !matches!(outcome, CommandOutcome::Success { .. }) {
            return CommandDispatch::Complete(
                self.finish_surface_creation(id, &outcome, effects)
                    .err()
                    .unwrap_or(outcome),
            );
        }
        let Some(request) = self
            .pending_new_surface
            .clone()
            .filter(|request| request.id == id)
        else {
            return self.reject_command(stale("The restored task chooser is no longer available"));
        };
        let delegated = self
            .validate_surface_request(&request)
            .and_then(|scope| self.surface_choice_invocation(&request, scope, choice, caller));
        let delegated = match delegated {
            Ok(delegated) => delegated,
            Err(failure) => {
                let failure = self
                    .finish_surface_creation(id, &outcome, effects)
                    .err()
                    .unwrap_or(failure);
                return self.reject_command(failure);
            }
        };
        // Restoration already accepted the outer invocation; the nested mutation has its own admission token.
        let cancellation = CommandCancellation::new();
        match self.dispatch_command_with_execution(
            delegated,
            ViewportSnapshot::default(),
            effects,
            Some((deadline, cancellation.clone())),
        ) {
            CommandDispatch::Complete(created) => {
                let accepted = if matches!(created, CommandOutcome::Success { .. }) {
                    &created
                } else {
                    &outcome
                };
                CommandDispatch::Complete(
                    self.finish_surface_creation(id, accepted, effects)
                        .err()
                        .unwrap_or(created),
                )
            }
            CommandDispatch::Pending(result) => {
                CommandDispatch::Pending(PendingCommandResult::SurfaceCreation {
                    request_id: id,
                    restored: Some(Box::new(outcome)),
                    command: Box::new(PendingAppCommand {
                        creation_receipt: None,
                        label: "Create restored task surface".to_owned(),
                        user_initiated_annotation_capture: false,
                        deadline,
                        cancellation,
                        response: None,
                        result,
                    }),
                })
            }
        }
    }

    pub(super) fn poll_surface_creation(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        let PendingCommandResult::SurfaceCreation {
            request_id,
            command,
            restored,
        } = &mut pending.result
        else {
            return Poll::Pending;
        };
        let accepted = match &command.result {
            PendingCommandResult::SessionCheckpointCompletion { outcome, .. } => {
                Some(outcome.as_ref().clone())
            }
            _ => None,
        };
        let outcome = ready!(self.poll_pending_app_command(command, now, effects));
        if let Some(outcome) = &outcome {
            if let PendingCommandResult::TerminalAgent {
                observed: Some(target),
                ..
            } = &command.result
                && !matches!(outcome, CommandOutcome::Success { .. })
            {
                return Poll::Ready(Some(
                    self.finish_surface_target(*request_id, target.clone(), effects)
                        .err()
                        .unwrap_or_else(|| outcome.clone()),
                ));
            }
            let receipt = accepted.as_ref().unwrap_or_else(|| {
                if matches!(outcome, CommandOutcome::Success { .. }) {
                    outcome
                } else {
                    restored.as_deref().unwrap_or(outcome)
                }
            });
            if let Err(error) = self.finish_surface_creation(*request_id, receipt, effects) {
                return Poll::Ready(Some(error));
            }
        }
        Poll::Ready(outcome)
    }

    pub(super) fn finish_surface_creation(
        &mut self,
        request_id: u64,
        outcome: &CommandOutcome,
        effects: &mut Vec<AppEffect>,
    ) -> Result<(), CommandOutcome> {
        if self
            .creating_surface_request
            .as_ref()
            .is_some_and(|(creating, _)| *creating == request_id)
        {
            self.creating_surface_request = None;
        }
        if self
            .pending_new_surface
            .as_ref()
            .is_none_or(|request| request.id != request_id)
        {
            return Ok(());
        }
        let CommandOutcome::Success { value, .. } = outcome else {
            return Ok(());
        };
        let target = value
            .get("native")
            .and_then(|native| {
                serde_json::from_value::<bootty_agents::NativeSessionRecord>(native.clone()).ok()
            })
            .map(|record| record.target())
            .or_else(|| {
                value
                    .get("terminal")
                    .or_else(|| value.get("created"))
                    .and_then(|target| serde_json::from_value::<CommandTarget>(target.clone()).ok())
            });
        if let Some(target) = target {
            self.finish_surface_target(request_id, target, effects)?;
        }
        Ok(())
    }

    fn finish_surface_target(
        &mut self,
        request_id: u64,
        target: CommandTarget,
        effects: &mut Vec<AppEffect>,
    ) -> Result<(), CommandOutcome> {
        if self
            .pending_new_surface
            .as_ref()
            .is_none_or(|request| request.id != request_id)
        {
            return Ok(());
        }
        if target.kind == ResourceKind::Terminal {
            let (_, exact) = self.resolve_command_target(
                "terminal.capture",
                Some(ResourceKind::Terminal),
                Some(&target),
            )?;
            let exact = exact.ok_or_else(|| stale("The created terminal has no mux target"))?;
            // Foreground creation owns selection before publishing its attached panel.
            self.activate_terminal_target(&exact)?;
        }
        self.creating_surface_request = None;
        self.pending_new_surface = None;
        effects.push(AppEffect::AttachNewSurface { request_id, target });
        effects.push(AppEffect::CloseSurfaceChooser(request_id));
        Ok(())
    }

    fn cancel_surface_creation(&self, request_id: u64) -> bool {
        self.creating_surface_request
            .as_ref()
            .is_none_or(|(creating, cancellation)| {
                *creating != request_id || cancellation.cancel() || cancellation.is_cancelled()
            })
    }
}
