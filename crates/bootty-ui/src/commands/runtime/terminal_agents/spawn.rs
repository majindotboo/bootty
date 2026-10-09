//! Detached child creation from a live parent's host-issued authority.

use super::{LaunchContext, failure, register_created_agent};
use bootty_agents::{
    AgentCommandExecutor, AgentLaunch, PreparedTerminalAgent, TerminalAgentService,
    ToolChildAuthority, ToolSpawnParent, ToolSpawnRequest,
};
use bootty_control::{CommandCancellation, CommandInvocation, CommandOutcome};
use std::{sync::Arc, time::Instant};

pub(super) fn execute(
    service: &Arc<TerminalAgentService>,
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    context: &LaunchContext,
    deadline: Instant,
    cancellation: CommandCancellation,
) -> CommandOutcome {
    if !context.allow_spawn || !context.preferences.enabled {
        return denied("Child spawning is disabled in Settings");
    }
    if context.remote || !cfg!(unix) {
        return CommandOutcome::Unsupported {
            message: "Child spawning requires the parent's supported local Unix host".to_owned(),
        };
    }
    let Some(target) = invocation.target.as_ref() else {
        return denied("Child spawning requires an exact parent Terminal");
    };
    let captured = invocation.arguments.get(1).map_or_else(
        || {
            service.spawn_parent(target).map(|(record, lease)| {
                (
                    ToolSpawnParent {
                        binding_id: record.binding_id,
                        launch: record.launch,
                    },
                    lease,
                )
            })
        },
        |selector| {
            selector.parse::<u64>().ok().and_then(|selector| {
                service.spawn_parent_for_attachment(target, selector, invocation.caller)
            })
        },
    );
    let Some((parent, lease)) = captured else {
        return denied("The exact parent tool attachment is no longer live");
    };
    if lease.caller() != invocation.caller {
        return denied("The parent tool attachment belongs to another caller");
    }
    if context.binding.as_ref() != Some(&lease.scope().binding) {
        return denied("The parent Binding generation has changed");
    }
    let Some(encoded) = invocation.arguments.first() else {
        return failure("Choose a typed child task request");
    };
    let request = match ToolSpawnRequest::parse(encoded.as_bytes()) {
        Ok(request) => request,
        Err(error) => return failure(&error),
    };
    let authority = match lease.authorize_spawn(&request) {
        Ok(authority) => authority,
        Err(error) => return denied(&error),
    };
    if parent.launch.account_directory.is_none() {
        return denied("The parent's provider account was not captured");
    }
    let prepared = match prepare_child(
        service,
        commands,
        &request,
        &parent.launch,
        authority,
        context.process_local,
    ) {
        Ok(prepared) => prepared,
        Err(error) => return failure(&error),
    };
    create_child(
        service,
        commands,
        &lease,
        parent,
        &request,
        prepared,
        (deadline, cancellation),
    )
}

fn prepare_child(
    service: &Arc<TerminalAgentService>,
    commands: &Arc<dyn AgentCommandExecutor>,
    request: &ToolSpawnRequest,
    parent: &AgentLaunch,
    authority: ToolChildAuthority,
    process_local: bool,
) -> Result<Option<PreparedTerminalAgent>, String> {
    let ToolSpawnRequest::Agent {
        provider, prompt, ..
    } = request
    else {
        return Ok(None);
    };
    let mut launch = parent.clone();
    launch.arguments.extend(["--".to_owned(), prompt.clone()]);
    launch.validate()?;
    let executable =
        std::env::current_exe().map_err(|_| "Bootty tool executable is unavailable")?;
    let tools =
        bootty_agents::ToolBridge::prepare_child(authority, &executable, Arc::clone(commands))?;
    let unobserved = (!process_local && *provider == bootty_agents::AgentKind::Codex).then(|| {
        "Codex runs directly in this persistent terminal; app-owned observation is unavailable"
            .to_owned()
    });
    service
        .prepare_with_tools(*provider, launch, tools, unobserved)
        .map(Some)
}

fn create_child(
    service: &TerminalAgentService,
    commands: &Arc<dyn AgentCommandExecutor>,
    lease: &bootty_agents::ToolLease,
    parent: ToolSpawnParent,
    request: &ToolSpawnRequest,
    prepared: Option<PreparedTerminalAgent>,
    execution: (Instant, CommandCancellation),
) -> CommandOutcome {
    let Some(cwd) = parent.launch.cwd.as_deref() else {
        return denied("The parent's project directory was not captured");
    };
    let (deadline, cancellation) = execution;
    let (name, title) = match &request {
        ToolSpawnRequest::Shell { name, title } | ToolSpawnRequest::Agent { name, title, .. } => {
            (name, title.as_deref().unwrap_or(name))
        }
    };
    let argv = prepared
        .as_ref()
        .map_or_else(Vec::new, bootty_agents::PreparedTerminalAgent::argv);
    let argv = match serde_json::to_string(&argv) {
        Ok(argv) => argv,
        Err(error) => return failure(&error.to_string()),
    };
    let identity = bootty_mux::snapshot::new_session_identity();
    let mut create = CommandInvocation::new(
        "session.create",
        vec![
            name.clone(),
            cwd.to_owned(),
            argv,
            identity.clone(),
            title.to_owned(),
        ],
        lease.caller(),
    );
    create.target = Some(lease.scope().binding.clone());
    // Recheck immediately before the shared owner accepts its durable mutation.
    if cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    let guard = match lease.begin_spawn_with_cancellation(request, cancellation.clone()) {
        Ok(guard) => guard,
        Err(error) => return denied(&error),
    };
    let outcome = commands.execute_pending(create, deadline, guard.cancellation());
    drop(guard);
    let CommandOutcome::Success {
        mut value,
        mut warnings,
    } = outcome
    else {
        return outcome;
    };
    if let Some(prepared) = prepared
        && let Err(error) = register_created_agent(
            service,
            prepared,
            parent.binding_id,
            &mut value,
            &mut warnings,
            commands.as_ref(),
            (deadline, cancellation),
        )
    {
        return failure(&error);
    }
    if let Some(value) = value.as_object_mut() {
        value.insert("task_id".to_owned(), identity.into());
        value.insert("title".to_owned(), title.into());
    }
    // Creation does not select a child. Accepted outcomes retain their IDs even after revocation.
    CommandOutcome::Success { value, warnings }
}

fn denied(message: &str) -> CommandOutcome {
    CommandOutcome::Denied {
        message: message.to_owned(),
    }
}
