use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Instant,
};

use bootty_agents::{AgentCommandExecutor, AgentKind, NativeAgentService, NativeSessionConfig};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use serde_json::{Value, json};

use super::{CommandDispatch, PendingCommandResult, serialized_command_outcome};
use crate::state::AppState;

impl AppState {
    pub(super) fn dispatch_native_agent(
        &self,
        mut invocation: CommandInvocation,
        exact_target: Option<&crate::commands::ExactMuxTarget>,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandDispatch {
        if let Some(command) = invocation.command.strip_prefix("harness.") {
            invocation.command = format!("agents.{command}");
        }
        let Some(service) = self.commands.native_agents.clone() else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "Native agent storage is unavailable".to_owned(),
            });
        };
        let native_binding = invocation
            .target
            .as_ref()
            .and_then(|target| service.binding_for_target(target).ok());
        let scope = exact_target.map_or_else(
            || {
                native_binding
                    .as_ref()
                    .and_then(|owner| {
                        self.workspace
                            .all_bindings()
                            .find(|binding| {
                                binding.scope().persistence_value().to_string() == *owner
                            })
                            .map(bootty_mux::workspace::BindingRuntime::scope)
                    })
                    .unwrap_or_else(|| self.workspace.active.binding.scope())
            },
            crate::commands::ExactMuxTarget::scope,
        );
        let Some(binding) = self.workspace.binding(scope) else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Native agent Space is no longer available".to_owned(),
            });
        };
        let binding_id = scope.persistence_value().to_string();
        let remote = binding.multiplexer().remote.is_some();
        let cwd = binding
            .mux()
            .selected_session_anchor()
            .and_then(|anchor| anchor.cwd.clone())
            .unwrap_or_else(|| crate::state::default_session_cwd(self.config()));
        let account_target = crate::commands::ExactMuxTarget::Binding(scope).command_target(
            ResourceKind::Binding,
            binding.mux(),
            &self.binding_target_handle(scope, binding.mux().binding_generation()),
        );
        let account_executor = super::agents::AppCommandAgentExecutor {
            sender: self.commands.sender.clone(),
        };
        let repaint = self.repaint.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = if cancellation.is_cancelled() {
                CommandOutcome::cancelled()
            } else if Instant::now() >= deadline {
                CommandOutcome::deadline_exceeded()
            } else if remote
                && invocation
                    .command
                    .rsplit_once('.')
                    .is_some_and(|(_, operation)| operation == "start")
            {
                CommandOutcome::Unsupported {
                    message: "Native agent sessions currently require a local Space".to_owned(),
                }
            } else if (invocation.command.starts_with("agents.claude.")
                || invocation.command.starts_with("agents.pi."))
                && (invocation.command.ends_with(".account.login")
                    || invocation.command.ends_with(".account.logout"))
            {
                account_terminal(
                    &service,
                    &invocation,
                    &account_executor,
                    account_target,
                    deadline,
                    cancellation,
                )
            } else {
                invoke(&service, &invocation, &binding_id, PathBuf::from(cwd))
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
}

fn account_terminal(
    service: &NativeAgentService,
    invocation: &CommandInvocation,
    executor: &dyn AgentCommandExecutor,
    target: Option<CommandTarget>,
    deadline: Instant,
    cancellation: CommandCancellation,
) -> CommandOutcome {
    let Some(native_target) = invocation.target.as_ref() else {
        return CommandOutcome::Unavailable {
            message: "Choose an agent account session".to_owned(),
        };
    };
    let Some(record) = service.sessions().into_iter().find(|record| {
        record.target() == *native_target
            && invocation
                .command
                .starts_with(&format!("agents.{}.", record.config.provider))
    }) else {
        return CommandOutcome::StaleTarget {
            message: "The agent account session is no longer available".to_owned(),
        };
    };
    let logout = invocation.command.ends_with(".logout");
    if logout {
        for session in service
            .sessions()
            .into_iter()
            .filter(|session| session.config.provider == record.config.provider)
        {
            if service.resolve(&session.target()).is_ok()
                && let Err(error) = service.stop(&session.target())
            {
                return CommandOutcome::Failed {
                    code: "account_sessions_active".to_owned(),
                    message: error,
                };
            }
        }
    }
    let launch =
        bootty_agents::agent_account_launch(record.config.provider, &record.config.program, logout);
    let argv = std::iter::once(launch.program)
        .chain(launch.arguments)
        .collect::<Vec<_>>();
    let arguments = match serde_json::to_string(&argv) {
        Ok(argv) => argv,
        Err(error) => {
            return CommandOutcome::Failed {
                code: "account_launch_failed".to_owned(),
                message: error.to_string(),
            };
        }
    };
    let mut request = CommandInvocation::new(
        "session.create",
        vec![
            format!(
                "{} account {}",
                record.config.provider,
                if logout { "sign out" } else { "sign in" }
            ),
            record.config.cwd.to_string_lossy().into_owned(),
            arguments,
        ],
        Caller::Internal,
    );
    request.target = target;
    match executor.execute(request, deadline, cancellation) {
        CommandOutcome::Success { value, warnings } => CommandOutcome::Success {
            value: json!({"terminal_target":value.get("terminal"),"message":if record.config.provider == AgentKind::Pi { if logout { "Enter /logout in the terminal tab, then refresh account status" } else { "Enter /login in the terminal tab, then refresh account status" } } else { "Complete the account action in the terminal tab, then refresh account status" }}),
            warnings,
        },
        outcome => outcome,
    }
}

fn invoke(
    service: &Arc<NativeAgentService>,
    invocation: &CommandInvocation,
    binding_id: &str,
    cwd: PathBuf,
) -> CommandOutcome {
    match execute(service, invocation, binding_id, cwd) {
        Ok(value) => serialized_command_outcome(value),
        Err(error) => CommandOutcome::Failed {
            code: "native_agent_failed".to_owned(),
            message: error,
        },
    }
}

fn execute(
    service: &NativeAgentService,
    invocation: &CommandInvocation,
    binding_id: &str,
    cwd: PathBuf,
) -> Result<Value, String> {
    if invocation.command == "agents.list" {
        return serde_json::to_value(native_agent_overview(Some(service)))
            .map_err(|error| error.to_string());
    }
    if invocation.command == "agents.native.list" {
        return serde_json::to_value(service.sessions()).map_err(|error| error.to_string());
    }
    let command = invocation
        .command
        .strip_prefix("agents.")
        .ok_or("Invalid native agent command")?;
    let (provider, operation) = command
        .split_once('.')
        .ok_or("Invalid native agent operation")?;
    let provider = AgentKind::ALL
        .into_iter()
        .find(|kind| kind.to_string() == provider)
        .ok_or("Unknown native agent provider")?;
    let arg = |index: usize| invocation.arguments.get(index).map(String::as_str);
    if operation == "start" {
        return start(service, invocation, provider, binding_id, cwd);
    }
    let target = invocation
        .target
        .as_ref()
        .ok_or("Choose an explicit native agent session")?;
    let record = service
        .sessions()
        .into_iter()
        .find(|record| record.target() == *target && record.config.provider == provider)
        .ok_or("Native session target is unknown, stale or belongs to another provider")?;
    match operation {
        "rename" => {
            service.rename(target, arg(0).ok_or("A session title is required")?)?;
            return Ok(Value::Null);
        }
        "remove" => {
            service.remove(target)?;
            return Ok(Value::Null);
        }
        "state" => return serde_json::to_value(record.snapshot).map_err(|error| error.to_string()),
        "resume" => {
            return serde_json::to_value(service.resume(target)?)
                .map_err(|error| error.to_string());
        }
        "stop" => {
            service.stop(target)?;
            return Ok(Value::Null);
        }
        "fork" => {
            return serde_json::to_value(service.fork(target)?).map_err(|error| error.to_string());
        }
        "history" if service.resolve(target).is_err() => {
            return serde_json::to_value(record.snapshot).map_err(|error| error.to_string());
        }
        _ => {}
    }
    let session = service.resolve(target)?;
    let value = match operation {
        "prompt" => {
            serde_json::to_value(service.prompt(target, arg(0).ok_or("A prompt is required")?)?)
                .map_err(|error| error.to_string())?
        }
        "interrupt" | "abort" => {
            session.interrupt()?;
            Value::Null
        }
        "history" => {
            serde_json::to_value(session.refresh_history()?).map_err(|error| error.to_string())?
        }
        "approve" => {
            let allow = match arg(1) {
                Some("true") => true,
                Some("false") => false,
                _ => return Err("Approval must be true or false".to_owned()),
            };
            session.approve(arg(0).ok_or("A request id is required")?, allow)?;
            Value::Null
        }
        "respond" => {
            session.respond(
                arg(0).ok_or("A request id is required")?,
                serde_json::from_str(arg(1).ok_or("A JSON response is required")?)
                    .map_err(|error| error.to_string())?,
            )?;
            Value::Null
        }
        "account.status" => session.account_status()?,
        "account.login" => session.account_login(arg(0))?,
        "account.logout" => session.account_logout()?,
        _ => return Err("Unknown native agent operation".to_owned()),
    };
    if matches!(operation, "prompt" | "history") {
        service.checkpoint()?;
    }
    Ok(value)
}

fn start(
    service: &NativeAgentService,
    invocation: &CommandInvocation,
    provider: AgentKind,
    binding_id: &str,
    cwd: PathBuf,
) -> Result<Value, String> {
    let arg = |index: usize| invocation.arguments.get(index).map(String::as_str);
    let mut config = NativeSessionConfig::new(
        provider,
        arg(0)
            .filter(|cwd| !cwd.is_empty())
            .map_or(cwd, PathBuf::from),
    );
    if let Some(program) = arg(1).filter(|program| !program.is_empty()) {
        program.clone_into(&mut config.program);
    }
    if let Some(arguments) = arg(2) {
        config.arguments = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid agent argv: {error}"))?;
    }
    serde_json::to_value(service.create(binding_id, &format!("New {provider} session"), config)?)
        .map_err(|error| error.to_string())
}

pub(super) fn native_agent_overview(
    service: Option<&NativeAgentService>,
) -> Vec<crate::state::agent_attention::AgentOverview> {
    service.map_or_else(Vec::new, |service| {
        service
            .sessions()
            .into_iter()
            .map(|record| {
                let message = record
                    .snapshot
                    .transcript
                    .iter()
                    .rev()
                    .find(|item| item.role == "assistant")
                    .map(|item| item.text.clone());
                let (message, truncated) =
                    crate::state::agent_attention::message_preview(message.as_deref());
                let status = match record.snapshot.status {
                    bootty_agents::NativeSessionStatus::Starting
                    | bootty_agents::NativeSessionStatus::Working => "working",
                    bootty_agents::NativeSessionStatus::Idle => "idle",
                    bootty_agents::NativeSessionStatus::Waiting => "waiting",
                    bootty_agents::NativeSessionStatus::Stopped => "stopped",
                    bootty_agents::NativeSessionStatus::Error => "error",
                };
                crate::state::agent_attention::AgentOverview {
                    provider: record.config.provider,
                    target: record.target(),
                    scope: record.binding_id,
                    pane: String::new(),
                    host: "Local".to_owned(),
                    title: record.title,
                    status: status.to_owned(),
                    unread: false,
                    attention_sequence: record.snapshot.revision.to_string(),
                    can_resume: record.config.session_id.is_some(),
                    cwd: Some(record.config.cwd.to_string_lossy().into_owned()),
                    source: "native".to_owned(),
                    session_id: record.snapshot.session_id,
                    session_file: record.snapshot.session_file,
                    last_event: None,
                    last_message: message,
                    last_message_truncated: truncated,
                    turn_ended_at: None,
                }
            })
            .collect()
    })
}
