//! Agent TUIs use backend terminal topology and the ordinary command mailbox.
use super::{CommandDispatch, PendingCommandResult};
use crate::{commands::ExactMuxTarget, state::AppState};
use bootty_agents::{
    AgentCommandExecutor, AgentKind, AgentLaunch, TerminalAgentRecord, TerminalAgentService,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, CommandWarning,
    ResourceKind,
};
use serde_json::json;
use std::{sync::mpsc, time::Instant};

impl AppState {
    pub(super) fn dispatch_terminal_agent(
        &self,
        invocation: CommandInvocation,
        exact: Option<&ExactMuxTarget>,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandDispatch {
        let Some(service) = self.commands.terminal_agents.clone() else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "Terminal agent metadata storage is unavailable".to_owned(),
            });
        };
        let scope = exact.map_or_else(
            || self.workspace.active.binding.scope(),
            ExactMuxTarget::scope,
        );
        let Some(binding) = self.workspace.binding(scope) else {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "The agent Space is no longer available".to_owned(),
            });
        };
        let binding_id = scope.persistence_value().to_string();
        let cwd = binding
            .mux()
            .selected_session_anchor()
            .and_then(|anchor| anchor.cwd.clone())
            .unwrap_or_else(|| crate::state::default_session_cwd(self.config()));
        let binding_target = ExactMuxTarget::Binding(scope).command_target(
            ResourceKind::Binding,
            binding.mux(),
            &self.binding_target_handle(scope, binding.mux().binding_generation()),
        );
        let remote = binding.multiplexer().remote.is_some();
        let session_names = self.agent_session_names_in_use();
        let isolate_color_environment = cfg!(unix)
            && matches!(
                binding.multiplexer().backend,
                bootty_config::config::MultiplexerBackendConfig::Rmux
                    | bootty_config::config::MultiplexerBackendConfig::Tmux
            );
        let color_override = self
            .config()
            .session
            .env
            .iter()
            .find(|(name, _)| name == "NO_COLOR")
            .map(|(_, value)| value.clone());
        let executor = super::agents::AppCommandAgentExecutor {
            sender: self.commands.sender.clone(),
        };
        let repaint = self.repaint.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let context = TerminalAgentContext {
                service: &service,
                executor: &executor,
                binding_target,
                binding_id: &binding_id,
                cwd,
                remote,
                session_names,
                isolate_color_environment,
                color_override,
                deadline,
                cancellation,
            };
            let outcome = execute(context, &invocation);
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }

    fn agent_session_names_in_use(&self) -> Vec<String> {
        let mut names = self
            .workspace
            .all_bindings()
            .flat_map(|binding| binding.mux().backend_session_names().iter().cloned())
            .collect::<Vec<_>>();
        names.extend(self.commands.pending.iter().filter_map(|pending| match &pending.result {
            PendingCommandResult::Mux {
                command: bootty_mux::command::MuxCommand::CreateProjectSession { session_id, .. },
                ..
            } => Some(session_id.clone()),
            PendingCommandResult::SessionStart { name, .. } => Some(name.clone()),
            _ => None,
        }));
        names
    }
}

struct TerminalAgentContext<'a> {
    service: &'a TerminalAgentService,
    executor: &'a dyn AgentCommandExecutor,
    binding_target: Option<CommandTarget>,
    binding_id: &'a str,
    cwd: String,
    remote: bool,
    session_names: Vec<String>,
    isolate_color_environment: bool,
    color_override: Option<String>,
    deadline: Instant,
    cancellation: CommandCancellation,
}

fn execute(context: TerminalAgentContext<'_>, invocation: &CommandInvocation) -> CommandOutcome {
    if context.cancellation.is_cancelled() {
        return CommandOutcome::cancelled();
    }
    if Instant::now() >= context.deadline {
        return CommandOutcome::deadline_exceeded();
    }
    let Some((provider, operation)) = invocation
        .command
        .strip_prefix("agents.")
        .and_then(|command| command.split_once('.'))
    else {
        return failure("Invalid terminal agent command");
    };
    let Some(provider) = AgentKind::ALL
        .into_iter()
        .find(|kind| kind.to_string() == provider)
    else {
        return failure("Unknown agent provider");
    };
    match operation {
        "prompt" | "interrupt" | "abort" | "stop" => {
            terminal_operation(&context, invocation, provider, operation)
        }
        "sessions" => session_history(invocation, provider, context.remote),
        "account.status" => account_status(invocation, provider, context.remote),
        _ => match prepare_launch(invocation, provider, operation, context.cwd.clone()) {
            Ok((launch, session_id)) => {
                start_terminal(context, provider, operation, launch, session_id)
            }
            Err(error) => failure(&error),
        },
    }
}

fn terminal_operation(
    context: &TerminalAgentContext<'_>,
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
) -> CommandOutcome {
    let TerminalAgentContext {
        service,
        executor,
        deadline,
        cancellation,
        ..
    } = context;
    let deadline = *deadline;
    let Some(target) = invocation.target.as_ref() else {
        return failure("Choose an agent terminal");
    };
    if service
        .record(target)
        .is_none_or(|record| record.provider != provider)
    {
        return CommandOutcome::StaleTarget {
            message: "The terminal is not owned by this agent provider".to_owned(),
        };
    }
    let (command, arguments) = match operation {
        "prompt" => {
            let Some(message) = invocation.arguments.first() else {
                return failure("A prompt is required");
            };
            let mut paste =
                CommandInvocation::new("terminal.paste", vec![message.clone()], Caller::Internal);
            paste.target = Some(target.clone());
            let outcome = executor.execute(paste, deadline, cancellation.clone());
            if !matches!(outcome, CommandOutcome::Success { .. }) {
                return outcome;
            }
            ("terminal.submit", Vec::new())
        }
        "interrupt" | "abort" => (
            "terminal.write",
            vec![
                if provider == AgentKind::Claude {
                    "\u{1b}"
                } else {
                    "\u{3}"
                }
                .to_owned(),
            ],
        ),
        "stop" => ("pane.close", Vec::new()),
        _ => return failure("Unknown terminal operation"),
    };
    let mut request = CommandInvocation::new(command, arguments, Caller::Internal);
    request.target = Some(target.clone());
    if operation == "stop" {
        request.confirmation = Some(request.confirmation());
    }
    executor.execute(request, deadline, cancellation.clone())
}

fn session_history(
    invocation: &CommandInvocation,
    provider: AgentKind,
    remote: bool,
) -> CommandOutcome {
    if remote {
        return CommandOutcome::Unsupported {
            message: "Provider history discovery requires the local host".to_owned(),
        };
    }
    let Some(root) = bootty_agents::terminal_history_root(provider) else {
        return failure("Provider history directory is unavailable");
    };
    let records = match bootty_agents::discover_terminal_history(provider, &root) {
        Ok(records) => records,
        Err(error) => return failure(&error),
    };
    let cwd = invocation.arguments.first().filter(|cwd| !cwd.is_empty());
    CommandOutcome::Success {
        value: json!({ "sessions": records.into_iter().filter(|record| cwd.is_none_or(|cwd| record.cwd == std::path::Path::new(cwd))).collect::<Vec<_>>() }),
        warnings: Vec::new(),
    }
}

fn account_status(
    invocation: &CommandInvocation,
    provider: AgentKind,
    remote: bool,
) -> CommandOutcome {
    if remote {
        return CommandOutcome::Unsupported {
            message: "Query accounts on the remote host through the provider terminal".to_owned(),
        };
    }
    bootty_agents::terminal_account_status(
        provider,
        provider.default_program(),
        invocation.arguments.first().map(String::as_str),
    )
    .map_or_else(
        |error| failure(&error),
        |value| CommandOutcome::Success {
            value,
            warnings: Vec::new(),
        },
    )
}

fn prepare_launch(
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    cwd: String,
) -> Result<(AgentLaunch, Option<String>), String> {
    let arg = |index: usize| {
        invocation
            .arguments
            .get(index)
            .filter(|value| !value.is_empty())
            .cloned()
    };
    let mut launch = AgentLaunch {
        program: provider.default_program().to_owned(),
        cwd: Some(cwd),
        arguments: Vec::new(),
        ephemeral: false,
    };
    let mut session_id = None;
    match operation {
        "start" => {
            if let Some(cwd) = arg(0) {
                launch.cwd = Some(cwd);
            }
            if let Some(program) = arg(1) {
                launch.program = program;
            }
            if let Some(argv) = arg(2) {
                match serde_json::from_str(&argv) {
                    Ok(argv) => launch.arguments = argv,
                    Err(error) => return Err(format!("Invalid agent argv: {error}")),
                }
            }
            if launch.arguments.is_empty() && matches!(provider, AgentKind::Claude | AgentKind::Pi)
            {
                let id = TerminalAgentService::new_session_id();
                launch
                    .arguments
                    .extend(["--session-id".to_owned(), id.clone()]);
                session_id = Some(id);
            }
        }
        "resume" | "fork" | "history" => {
            if let Some(cwd) = arg(usize::from(operation != "history")) {
                launch.cwd = Some(cwd);
            }
            session_id = if operation == "history" { None } else { arg(0) };
            resume_arguments(&mut launch, provider, operation, session_id.as_ref())?;
            if operation == "fork" {
                session_id = None;
            }
        }
        "account.login" | "account.logout" => {
            let cwd = launch.cwd.take();
            launch = bootty_agents::agent_account_launch(
                provider,
                &launch.program,
                operation == "account.logout",
            );
            launch.cwd = cwd;
        }
        _ => return Err("Unknown terminal agent operation".to_owned()),
    }
    launch.validate()?;
    Ok((launch, session_id))
}

fn resume_arguments(
    launch: &mut AgentLaunch,
    provider: AgentKind,
    operation: &str,
    session_id: Option<&String>,
) -> Result<(), String> {
    if let Some(id) = session_id
        && let Err(error) = launch.session_arguments(provider, id, operation == "fork")
    {
        return Err(error);
    }
    match provider {
        AgentKind::Codex => {
            launch.arguments.push(
                if operation == "fork" {
                    "fork"
                } else {
                    "resume"
                }
                .to_owned(),
            );
            if let Some(id) = session_id {
                launch.arguments.push(id.clone());
            }
        }
        AgentKind::Claude => {
            launch.arguments.push("--resume".to_owned());
            if let Some(id) = session_id {
                launch.arguments.push(id.clone());
            }
            if operation == "fork" {
                launch.arguments.push("--fork-session".to_owned());
            }
        }
        AgentKind::Pi => {
            launch.arguments.push(
                if operation == "fork" {
                    "--fork"
                } else if session_id.is_some() {
                    "--session"
                } else {
                    "--resume"
                }
                .to_owned(),
            );
            if let Some(id) = session_id {
                launch.arguments.push(id.clone());
            }
            if operation == "fork" && session_id.is_none() {
                return Err("Select a Pi session before forking".to_owned());
            }
        }
    }
    Ok(())
}

fn start_terminal(
    context: TerminalAgentContext<'_>,
    provider: AgentKind,
    operation: &str,
    launch: AgentLaunch,
    session_id: Option<String>,
) -> CommandOutcome {
    let argv = if context.isolate_color_environment {
        launch.posix_terminal_argv(context.color_override.as_deref())
    } else {
        std::iter::once(launch.program.clone())
            .chain(launch.arguments.iter().cloned())
            .collect()
    };
    let cwd = launch.cwd.as_deref().unwrap_or_default();
    let project = if context.remote {
        bootty_mux::session_names::session_name_for_remote_path(cwd)
    } else {
        bootty_git::suggested_session_name(cwd)
    };
    let name = bootty_mux::session_names::unique_session_name(
        &bootty_mux::session_names::portable_session_name(&format!("{provider} {project}")),
        context.session_names.iter().map(String::as_str),
    );
    let TerminalAgentContext {
        service,
        executor,
        binding_target,
        binding_id,
        deadline,
        cancellation,
        ..
    } = context;
    let encoded = match serde_json::to_string(&argv) {
        Ok(value) => value,
        Err(error) => return failure(&error.to_string()),
    };
    let mut request = CommandInvocation::new(
        "session.create",
        vec![name, launch.cwd.clone().unwrap_or_default(), encoded],
        Caller::Internal,
    );
    request.target = binding_target;
    match executor.execute(request, deadline, cancellation) {
        CommandOutcome::Success {
            value,
            mut warnings,
        } => {
            let target = value
                .get("terminal")
                .cloned()
                .and_then(|value| serde_json::from_value::<CommandTarget>(value).ok());
            if let Some(target) = &target
                && let Err(error) = service.register(TerminalAgentRecord {
                    provider,
                    target: target.clone(),
                    binding_id: binding_id.to_owned(),
                    launch,
                    session_id,
                })
            {
                warnings.push(CommandWarning {
                    code: "agent_metadata_failed".to_owned(),
                    message: format!(
                        "Agent terminal started; metadata could not be saved: {error}"
                    ),
                });
            }
            CommandOutcome::Success {
                value: json!({"created":value.get("created"),"terminal_target":target,"target":target,"provider":provider.to_string(),"message":if provider==AgentKind::Pi&&operation.starts_with("account."){"Use /login or /logout in the Pi terminal"}else{""}}),
                warnings,
            }
        }
        outcome => outcome,
    }
}

fn failure(message: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "terminal_agent_failed".to_owned(),
        message: message.to_owned(),
    }
}
