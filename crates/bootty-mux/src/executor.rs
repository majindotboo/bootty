use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use bootty_control::CommandCancellation;

use crate::{
    RepaintHandle,
    capability::BindingOperationOutcome,
    command::MuxCommand,
    controller::{MuxCommandError, MuxCommandResult, SpaceId},
    repository::{BindingMembershipMutation, WorkspacePersistenceError},
    workspace::{BindingRuntime, WorkspaceRuntime},
};

pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[must_use]
pub fn command_execution(
    execution: Option<(Instant, CommandCancellation)>,
) -> (Instant, CommandCancellation) {
    execution.unwrap_or_else(|| {
        let now = Instant::now();
        (
            now.checked_add(COMMAND_TIMEOUT).unwrap_or(now),
            CommandCancellation::new(),
        )
    })
}

/// # Errors
/// Returns cancellation or deadline expiry before the command starts.
pub fn begin_synchronous_command(
    execution: Option<(Instant, CommandCancellation)>,
) -> Result<(), MuxCommandError> {
    let Some((deadline, cancellation)) = execution else {
        return Ok(());
    };
    if Instant::now() >= deadline && cancellation.cancel() {
        return Err(MuxCommandError::DeadlineExceeded);
    }
    if !cancellation.try_start() {
        return Err(MuxCommandError::Cancelled);
    }
    Ok(())
}

/// # Errors
/// Returns the active backend failure or its unsupported, unavailable, or stale operation status.
pub fn preflight_command(
    workspace: &WorkspaceRuntime,
    command: &MuxCommand,
) -> Result<(), MuxCommandError> {
    if let Some(message) = workspace.active.binding.mux().unavailable_reason() {
        return Err(MuxCommandError::Failed(message.to_owned()));
    }
    match workspace
        .active
        .binding
        .mux()
        .operation_outcome(workspace.active.binding.multiplexer(), command.operation())
    {
        BindingOperationOutcome::Supported(()) => Ok(()),
        BindingOperationOutcome::Unsupported => Err(MuxCommandError::Unsupported),
        BindingOperationOutcome::Unavailable => Err(MuxCommandError::Unavailable),
        BindingOperationOutcome::Stale => Err(MuxCommandError::Stale),
    }
}

/// # Errors
/// Returns a persistence error if the membership journal cannot be prepared.
pub fn begin_authoritative_membership(
    workspace: &mut WorkspaceRuntime,
    command: &MuxCommand,
) -> Result<Option<Box<BindingMembershipMutation>>, WorkspacePersistenceError> {
    workspace
        .begin_active_binding_membership_mutation(command, None)
        .map(|membership| membership.map(Box::new))
}

pub struct PendingMuxCommand {
    pub scope: SpaceId,
    pub command: MuxCommand,
    pub membership: Option<Box<BindingMembershipMutation>>,
    pub layout: Option<crate::workspace::PreparedPaneArrangement>,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
    pub result: mpsc::Receiver<MuxCommandResult>,
}

pub fn submit_authoritative_command(
    workspace: &mut WorkspaceRuntime,
    repaint: &RepaintHandle,
    command: MuxCommand,
    membership: Option<Box<BindingMembershipMutation>>,
    execution: Option<(Instant, CommandCancellation)>,
) -> PendingMuxCommand {
    let scope = workspace.active.binding.scope();
    submit_binding_command(
        &mut workspace.active.binding,
        repaint,
        scope,
        command,
        membership,
        execution,
    )
}

/// Submit an authoritative command to a binding that may no longer be active.
///
/// Cleanup can finish after the user switches Spaces. Keep the original scope attached to the
/// command so the backend result and membership journal are completed against that binding.
pub fn submit_authoritative_command_for_scope(
    workspace: &mut WorkspaceRuntime,
    repaint: &RepaintHandle,
    scope: SpaceId,
    command: MuxCommand,
    membership: Option<Box<BindingMembershipMutation>>,
    execution: Option<(Instant, CommandCancellation)>,
) -> Option<PendingMuxCommand> {
    let binding = workspace.binding_mut(scope)?;
    Some(submit_binding_command(
        binding, repaint, scope, command, membership, execution,
    ))
}

fn submit_binding_command(
    binding: &mut BindingRuntime,
    repaint: &RepaintHandle,
    scope: SpaceId,
    command: MuxCommand,
    membership: Option<Box<BindingMembershipMutation>>,
    execution: Option<(Instant, CommandCancellation)>,
) -> PendingMuxCommand {
    let config = binding.multiplexer().clone();
    let (deadline, cancellation) = command_execution(execution);
    let layout = binding.prepare_pane_arrangement(&command);
    let result = binding.mux_mut().execute_command_authoritatively(
        repaint,
        &config,
        command.clone(),
        deadline,
        cancellation.clone(),
    );
    PendingMuxCommand {
        scope,
        command,
        membership,
        layout,
        deadline,
        cancellation,
        result,
    }
}

/// Execute a UI initiated mux operation using the active binding's domain state.
///
/// Project session creation has a persistence journal and generated-name bookkeeping, so it
/// remains a workspace operation. Other commands use the controller's ordinary optimistic path.
/// # Errors
/// Returns a persistence error if project session membership cannot be prepared.
pub fn execute_local_command(
    workspace: &mut WorkspaceRuntime,
    repaint: &RepaintHandle,
    command: MuxCommand,
) -> Result<bool, WorkspacePersistenceError> {
    if matches!(&command, MuxCommand::CreateProjectSession { .. }) {
        return workspace.create_project_session(&command, repaint);
    }
    let config = workspace.active.binding.multiplexer().clone();
    workspace
        .active
        .binding
        .mux_mut()
        .execute_command(repaint, &config, command);
    Ok(false)
}

/// Commit membership before publishing the authoritative backend snapshot.
/// # Errors
/// Returns a persistence error if membership cannot be committed before publication.
pub fn complete_authoritative_command(
    workspace: &mut WorkspaceRuntime,
    scope: SpaceId,
    membership: Option<&BindingMembershipMutation>,
    result: MuxCommandResult,
    layout: Option<&crate::workspace::PreparedPaneArrangement>,
) -> Result<(MuxCommandResult, Option<String>), WorkspacePersistenceError> {
    workspace.complete_binding_membership_command(scope, membership, &result)?;
    Ok(workspace.complete_authoritative_command(scope, result, layout))
}
