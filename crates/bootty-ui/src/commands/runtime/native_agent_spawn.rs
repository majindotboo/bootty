//! Native children reuse the accepted parent's host/account and real mux task creation.

use super::{
    AppCommandAgentExecutor, AppState, CommandDispatch, NativeLaunch, PendingCommandResult,
    PreparedNativeLaunch, executor, failure, start_prepared,
};
use bootty_agents::{
    AgentCommandExecutor, NativeAgentService, ToolBridge, ToolLease, ToolSpawnRequest,
};
use bootty_control::{CommandCancellation, CommandInvocation, CommandOutcome};
use serde_json::json;
use std::{
    sync::{Arc, mpsc},
    time::Instant,
};

impl AppState {
    fn capture_native_parent_attachment(
        &self,
        service: &NativeAgentService,
        invocation: &CommandInvocation,
    ) -> Result<(NativeLaunch, ToolLease), String> {
        let target = invocation
            .target
            .as_ref()
            .ok_or("Choose an exact parent conversation")?;
        let record = service
            .sessions()
            .into_iter()
            .find(|record| record.target() == *target)
            .ok_or("Parent conversation changed")?;
        let captured = self
            .capture_native_destination(&record, invocation)
            .map_err(|outcome| {
                crate::commands::command_outcome_message(&outcome)
                    .unwrap_or_else(|| "Parent destination is unavailable".into())
            })?;
        if !captured.context.allow_spawn || !captured.context.preferences.enabled {
            return Err("Child operations are disabled in Settings".into());
        }
        let attachment = invocation
            .arguments
            .get(3)
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or("Choose the exact parent tool attachment")?;
        let terminal = captured
            .terminal
            .as_ref()
            .ok_or("Parent pane is unavailable")?;
        let (_, lease) = captured
            .tools_owner
            .spawn_parent_for_attachment(terminal, attachment, invocation.caller)
            .ok_or("Parent tool grant is no longer live")?;
        if lease.native_session_target().as_ref() != Some(target)
            || captured.context.binding.as_ref() != Some(&lease.scope().binding)
        {
            return Err("Parent conversation or Binding changed".into());
        }
        Ok((captured, lease))
    }

    pub(super) fn dispatch_native_child_control(
        &self,
        service: Arc<NativeAgentService>,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let prepared = (|| {
            let (_, lease) = self.capture_native_parent_attachment(&service, invocation)?;
            let encoded = invocation
                .arguments
                .get(2)
                .ok_or("Choose a typed child operation")?;
            let request = bootty_agents::ToolChildControlRequest::parse(encoded.as_bytes())?;
            Ok::<_, String>((lease, request))
        })();
        let (lease, request) = match prepared {
            Ok(prepared) => prepared,
            Err(message) => return CommandDispatch::Complete(CommandOutcome::Denied { message }),
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        self.dispatch_committed_command(Some((deadline, cancellation.clone())), move || {
            let guard = match lease.begin_child_control(cancellation) {
                Ok(guard) => guard,
                Err(message) => return CommandOutcome::Denied { message },
            };
            let Some(parent) = lease.native_session_target() else {
                return failure("Native parent is no longer live".into());
            };
            match service.control_spawned_child(&parent, &request, &guard.cancellation()) {
                Ok(agent) => super::serialized_command_outcome(json!({
                    "agent":agent,"operation":request.operation,
                })),
                Err(message) => failure(message),
            }
        })
    }

    pub(super) fn dispatch_native_spawn(
        &self,
        service: Arc<NativeAgentService>,
        invocation: &CommandInvocation,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let captured = (|| {
            let (captured, lease) = self.capture_native_parent_attachment(&service, invocation)?;
            let request = invocation
                .arguments
                .get(2)
                .ok_or("Choose a typed child request")?;
            let request = ToolSpawnRequest::parse(request.as_bytes())?;
            lease.authorize_spawn(&request)?;
            Ok((captured, lease, request))
        })();
        let (captured, lease, request) = match captured {
            Ok(captured) => captured,
            Err(message) => return CommandDispatch::Complete(CommandOutcome::Denied { message }),
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        let commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            creation_receipt: None,
            sender: self.commands.sender.clone(),
        });
        let repaint = self.repaint.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = spawn(
                &service,
                &commands,
                &captured,
                &lease,
                &request,
                (deadline, cancellation),
            );
            _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
}

fn spawn(
    service: &NativeAgentService,
    commands: &Arc<dyn AgentCommandExecutor>,
    captured: &NativeLaunch,
    lease: &ToolLease,
    request: &ToolSpawnRequest,
    execution: (Instant, CommandCancellation),
) -> CommandOutcome {
    let (deadline, cancellation) = execution;
    if cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    if Instant::now() >= deadline {
        return CommandOutcome::deadline_exceeded();
    }
    let guard = match lease.begin_spawn_with_cancellation(request, cancellation) {
        Ok(guard) => guard,
        Err(message) => return CommandOutcome::Denied { message },
    };
    let Some(parent) = lease.native_session_target() else {
        return failure("Parent conversation is no longer live".into());
    };
    let Some(record) = service
        .sessions()
        .into_iter()
        .find(|record| record.target() == parent)
    else {
        return failure("Parent conversation changed".into());
    };
    let identity = bootty_mux::snapshot::new_session_identity();
    let (name, title) = match request {
        ToolSpawnRequest::Shell { name, title } | ToolSpawnRequest::Agent { name, title, .. } => {
            (name, title.as_deref().unwrap_or(name))
        }
    };
    let outcome = match request {
        ToolSpawnRequest::Shell { .. } => {
            let mut create = CommandInvocation::new(
                "session.create",
                vec![
                    name.clone(),
                    record.config.cwd.to_string_lossy().into_owned(),
                    "[]".into(),
                    identity.clone(),
                    title.into(),
                ],
                lease.caller(),
            );
            create.target = Some(lease.scope().binding.clone());
            commands.execute_pending(create, deadline, guard.cancellation())
        }
        ToolSpawnRequest::Agent {
            provider, prompt, ..
        } => {
            let prepared = match child_launch(
                &record,
                &captured.tools_owner,
                guard.authority(),
                Arc::clone(commands),
            ) {
                Ok(prepared) => prepared,
                Err(error) => return failure(error),
            };
            let mut create = CommandInvocation::new(
                "agents.native.start",
                vec![
                    provider.to_string(),
                    record.config.cwd.to_string_lossy().into_owned(),
                    record.config.program.clone(),
                    "[]".into(),
                    name.clone(),
                    String::new(),
                    identity.clone(),
                    title.into(),
                    prompt.clone(),
                ],
                lease.caller(),
            );
            create.target = Some(lease.scope().binding.clone());
            start_prepared(
                service,
                commands,
                &create,
                captured,
                prepared,
                deadline,
                &guard.cancellation(),
            )
        }
    };
    drop(guard);
    match outcome {
        CommandOutcome::Success {
            mut value,
            warnings,
        } => {
            if let Some(value) = value.as_object_mut() {
                value.insert("task_id".into(), json!(identity));
                value.insert("title".into(), json!(title));
            }
            CommandOutcome::Success { value, warnings }
        }
        outcome => outcome,
    }
}

fn child_launch(
    parent: &bootty_agents::NativeSessionRecord,
    owner: &bootty_agents::TerminalAgentService,
    authority: bootty_agents::ToolChildAuthority,
    commands: Arc<dyn AgentCommandExecutor>,
) -> Result<PreparedNativeLaunch, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let bridge = ToolBridge::prepare_child(authority, &executable, commands)?;
    let tools = owner.retain_tool_attachment(parent.config.provider, bridge)?;
    Ok(PreparedNativeLaunch {
        config: parent.config.clone(),
        tools: Some(tools),
        warnings: Vec::new(),
        spawn_parent: Some(parent.target()),
    })
}
