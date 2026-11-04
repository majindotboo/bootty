use std::{
    task::{Poll, ready},
    time::Instant,
};

use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use bootty_mux::{
    controller::{CommandSelection, SpaceId},
    executor,
};

use super::{CommandDispatch, PendingAppCommand, PendingCommandResult};
use crate::{
    commands::ExactMuxTarget,
    state::{AppEffect, AppState, ViewportSnapshot},
    surface_creation::{PendingNewSurface, SurfaceParent, SurfacePlacement},
};

fn stale(message: &str) -> CommandOutcome {
    CommandOutcome::StaleTarget {
        message: message.to_owned(),
    }
}

impl AppState {
    pub const fn pending_new_surface(&self) -> Option<&PendingNewSurface> {
        self.pending_new_surface.as_ref()
    }

    /// Captures where a new tab or split goes, then starts its terminal there. The request is
    /// retained until the backend creates the terminal or the creation fails.
    pub(super) fn open_new_surface(
        &mut self,
        invocation: &CommandInvocation,
        placement: SurfacePlacement,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let mut request = match self.capture_surface_destination(invocation, placement) {
            Ok(request) => request,
            Err(outcome) => return self.reject_command(outcome),
        };
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
        self.pending_new_surface = Some(request.clone());
        self.dismiss_session_creation();
        let scope = match self.validate_surface_request(&request) {
            Ok(scope) => scope,
            Err(outcome) => return self.reject_command(outcome),
        };
        let delegated = match self.surface_shell_invocation(
            &request,
            scope,
            "[]".to_owned(),
            invocation.caller,
        ) {
            Ok(mut delegated) => {
                delegated.confirmation.clone_from(&invocation.confirmation);
                delegated
            }
            Err(outcome) => return self.reject_command(outcome),
        };
        self.create_surface_from_invocation(request.id, delegated, viewport, effects, execution)
    }

    fn capture_surface_destination(
        &self,
        invocation: &CommandInvocation,
        placement: SurfacePlacement,
    ) -> Result<PendingNewSurface, CommandOutcome> {
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
            return Err(stale("The new surface parent is not a terminal or Space"));
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
            .target_working_directory(&exact)
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

    /// The directory the target's pane, window or session last reported; for a Space, its
    /// selected session's.
    fn target_working_directory(&self, exact: &ExactMuxTarget) -> Option<String> {
        let binding = self.workspace.binding(exact.scope())?;
        if matches!(exact, ExactMuxTarget::Binding(_)) {
            return binding
                .mux()
                .selected_session_anchor()
                .and_then(|anchor| anchor.cwd.clone());
        }
        let (session, window, pane) = exact.ids();
        let session = binding
            .mux()
            .all_sessions()
            .iter()
            .find(|candidate| Some(candidate.id.as_str()) == session)?;
        let Some(window) = window else {
            return session.anchor.cwd.clone();
        };
        let window = session
            .windows
            .iter()
            .find(|candidate| candidate.id == window)?;
        window
            .panes
            .iter()
            .find(|candidate| candidate.pane_id.as_deref() == pane)
            .unwrap_or(&window.anchor)
            .cwd
            .clone()
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
        if !request.task_identity.is_empty()
            && self
                .workspace
                .binding(scope)
                .and_then(|binding| binding.sessions().get(&request.task_identity))
                .is_none()
        {
            return Err(stale("The captured task was removed"));
        }
        match &request.parent {
            SurfaceParent::Terminal(target) => {
                self.resolve_command_target(
                    "terminal.create_pane",
                    Some(ResourceKind::Terminal),
                    Some(target),
                )?;
            }
            SurfaceParent::Binding(_) => {}
        }
        Ok(scope)
    }

    fn surface_terminal_destination(
        &self,
        request: &PendingNewSurface,
        scope: SpaceId,
    ) -> Result<(Option<&'static str>, CommandTarget), CommandOutcome> {
        if let SurfacePlacement::Split(direction) = request.placement {
            let target = match &request.parent {
                SurfaceParent::Terminal(target) => target.clone(),
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
        let mut arguments = vec![argv];
        if direction.is_none() {
            arguments.push(request.cwd.clone());
        }
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
            CommandSelection::Preserve,
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
                    command: Box::new(PendingAppCommand {
                        creation_receipt: None,
                        label: "Create surface".to_owned(),
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
            let receipt = accepted.as_ref().unwrap_or(outcome);
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
            .get("terminal")
            .or_else(|| value.get("created"))
            .and_then(|target| serde_json::from_value::<CommandTarget>(target.clone()).ok());
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
            // Foreground creation owns selection before publishing its attached panel.
            self.activate_created_terminal(&target)?;
        }
        self.creating_surface_request = None;
        if let Some(request) = self.pending_new_surface.take() {
            effects.push(AppEffect::AttachNewSurface { request, target });
        }
        Ok(())
    }

    /// Cancel a chooser bound to a Space that is no longer active. The workspace closes its
    /// presentation once this request is gone; an accepted creation still finishes in its Space.
    pub(crate) fn cancel_surface_chooser_outside_active_space(&mut self) {
        let Some(request) = &self.pending_new_surface else {
            return;
        };
        if self
            .surface_binding_target(self.workspace.active.binding.scope())
            .is_ok_and(|active| active == request.binding)
        {
            return;
        }
        let id = request.id;
        if self.cancel_surface_creation(id) {
            self.pending_new_surface = None;
        }
    }

    fn cancel_surface_creation(&self, request_id: u64) -> bool {
        self.creating_surface_request
            .as_ref()
            .is_none_or(|(creating, cancellation)| {
                *creating != request_id || cancellation.cancel() || cancellation.is_cancelled()
            })
    }
}
