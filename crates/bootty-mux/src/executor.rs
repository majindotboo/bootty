use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use bootty_control::CommandCancellation;

use crate::{
    RepaintHandle,
    command::MuxCommand,
    controller::{CommandSelection, MuxCommandError, MuxCommandResult, SpaceId},
    repository::{BindingMembershipMutation, WorkspacePersistenceError, WorkspaceRepository},
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
/// Returns the active backend failure or its unavailable status.
pub fn preflight_command(
    workspace: &WorkspaceRuntime,
    command: &MuxCommand,
) -> Result<(), MuxCommandError> {
    preflight_binding_command(&workspace.active.binding, command)
}

/// # Errors
/// Returns the binding's backend failure or its unavailable status.
pub fn preflight_binding_command(
    binding: &BindingRuntime,
    _command: &MuxCommand,
) -> Result<(), MuxCommandError> {
    if let Some(message) = binding.mux().unavailable_reason() {
        return Err(MuxCommandError::Failed(message.to_owned()));
    }
    Ok(())
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
    let repository = workspace.project_repository();
    submit_binding_command(
        repository,
        &mut workspace.active.binding,
        repaint,
        command,
        membership,
        execution,
        CommandSelection::Follow,
    )
}

/// Submit an authoritative command to a binding that may no longer be active.
///
/// Cleanup can finish after the user switches Spaces. Keep the original scope attached to the
/// command so the native result and saved membership are completed against that binding.
pub fn submit_authoritative_command_for_scope(
    workspace: &mut WorkspaceRuntime,
    repaint: &RepaintHandle,
    scope: SpaceId,
    command: MuxCommand,
    membership: Option<Box<BindingMembershipMutation>>,
    execution: Option<(Instant, CommandCancellation)>,
    selection: CommandSelection,
) -> Option<PendingMuxCommand> {
    let repository = workspace.project_repository();
    let binding = workspace.binding_mut(scope)?;
    Some(submit_binding_command(
        repository, binding, repaint, command, membership, execution, selection,
    ))
}

fn submit_binding_command(
    mut repository: WorkspaceRepository,
    binding: &mut BindingRuntime,
    repaint: &RepaintHandle,
    command: MuxCommand,
    membership: Option<Box<BindingMembershipMutation>>,
    execution: Option<(Instant, CommandCancellation)>,
    selection: CommandSelection,
) -> PendingMuxCommand {
    let scope = binding.scope();
    let config = binding.multiplexer().clone();
    let (deadline, cancellation) = command_execution(execution);
    let mut sessions = binding.sessions().clone();
    let persisted_membership = membership.clone();
    let result = binding.mux_mut().execute_command_authoritatively(
        repaint,
        &config,
        command.clone(),
        selection,
        (deadline, &cancellation),
        move || {
            if let Some(membership) = &persisted_membership {
                repository.commit_binding_membership_mutation(scope, membership, &mut sessions)?;
            }
            Ok(())
        },
    );
    PendingMuxCommand {
        scope,
        command,
        membership,
        deadline,
        cancellation,
        result,
    }
}

/// Execute a UI initiated mux operation using the active binding's domain state.
///
/// Project session creation has saved membership and generated-name bookkeeping, so it
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
    command: &MuxCommand,
    membership: Option<&BindingMembershipMutation>,
    result: MuxCommandResult,
) -> Result<(MuxCommandResult, Option<String>), WorkspacePersistenceError> {
    workspace.complete_binding_membership_command(scope, membership, &result)?;
    workspace.release_closed_session(scope, command, &result)?;
    Ok(workspace.complete_authoritative_command(scope, command, result))
}
