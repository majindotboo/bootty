//! Literal provider launches and exact terminal observation through the shared command mailbox.

use std::{
    sync::{Arc, mpsc},
    time::Instant,
};

use bootty_agents::{AgentCommandExecutor, AgentKind, AgentLaunch, TerminalAgentService};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, CommandWarning,
    ResourceKind,
};
use bootty_mux::executor;

use super::{
    CommandDispatch, PendingCommandResult, agents::AppCommandAgentExecutor,
    command_outcome_for_mux_error, serialized_command_outcome,
};
use crate::{commands::ExactMuxTarget, state::AppState};

struct LaunchContext {
    binding: Option<CommandTarget>,
    session: Option<CommandTarget>,
    binding_id: String,
    cwd: Option<String>,
    names: Vec<String>,
    remote: bool,
    process_local: bool,
}

impl AppState {
    pub(super) fn dispatch_terminal_agent(
        &self,
        invocation: CommandInvocation,
        exact: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(service) = self.commands.terminal_agents.clone() else {
            return CommandDispatch::Complete(CommandOutcome::Unsupported {
                message: "Terminal agent owner is not composed".to_owned(),
            });
        };
        let (deadline, cancellation) = executor::command_execution(execution);
        if let Err(error) =
            executor::begin_synchronous_command(Some((deadline, cancellation.clone())))
        {
            return CommandDispatch::Complete(command_outcome_for_mux_error(error));
        }
        let scope = exact.map_or_else(
            || self.workspace.active.binding.scope(),
            ExactMuxTarget::scope,
        );
        let binding = self.workspace.binding(scope);
        let binding_target = binding.and_then(|binding| {
            ExactMuxTarget::Binding(scope).command_target(
                ResourceKind::Binding,
                binding.mux(),
                &self.binding_target_handle(scope, binding.mux().binding_generation()),
            )
        });
        let context = LaunchContext {
            binding: binding_target,
            session: exact.and_then(|exact| exact.ids().0).and_then(|session| {
                self.mux_resource_target(scope, ResourceKind::Session, session, None)
            }),
            binding_id: scope.persistence_value().to_string(),
            cwd: exact
                .and_then(|exact| self.agent_launch_context(exact).cwd)
                .or_else(|| {
                    self.config()
                        .session
                        .working_directory
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned())
                }),
            names: binding.map_or_else(Vec::new, |binding| {
                binding
                    .mux()
                    .all_sessions()
                    .iter()
                    .map(|session| session.name.clone())
                    .collect()
            }),
            remote: binding.is_some_and(|binding| binding.multiplexer().remote.is_some()),
            process_local: binding.is_some_and(|binding| {
                binding.backend_policy().panes.topology
                    == bootty_mux::provider::PaneTopology::ProcessLocal
            }),
        };
        let commands = AppCommandAgentExecutor {
            sender: self.commands.sender.clone(),
        };
        let repaint = self.repaint.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = execute(
                &service,
                &commands,
                &invocation,
                context,
                deadline,
                cancellation,
            );
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(receiver))
    }
}

fn execute(
    service: &Arc<TerminalAgentService>,
    commands: &dyn AgentCommandExecutor,
    invocation: &CommandInvocation,
    context: LaunchContext,
    deadline: Instant,
    cancellation: CommandCancellation,
) -> CommandOutcome {
    let Some((provider, operation)) = parse_command(&invocation.command) else {
        return failure("Unknown terminal provider command");
    };
    match operation {
        "state" => invocation
            .target
            .as_ref()
            .and_then(|target| service.record(target))
            .filter(|record| record.provider == provider)
            .map_or_else(
                || CommandOutcome::Unavailable {
                    message: "This terminal has no native provider observation".to_owned(),
                },
                serialized_command_outcome,
            ),
        "history" => serialized_command_outcome(
            service
                .records()
                .into_iter()
                .filter(|record| record.provider == provider)
                .collect::<Vec<_>>(),
        ),
        "account.status" => {
            let program = invocation
                .arguments
                .get(1)
                .filter(|arg| !arg.is_empty())
                .map_or_else(|| provider.default_program(), String::as_str);
            bootty_agents::terminal_account_status(
                provider,
                program,
                invocation.arguments.first().map(String::as_str),
            )
            .map_or_else(|error| failure(&error), serialized_command_outcome)
        }
        "start" | "tab" | "resume" | "fork" | "account.login" | "account.logout" => {
            let launch =
                match prepare_launch(invocation, provider, operation, context.cwd.as_deref()) {
                    Ok(launch) => launch,
                    Err(error) => return failure(&error),
                };
            start_terminal(
                service,
                commands,
                invocation,
                (provider, operation),
                launch,
                context,
                (deadline, cancellation),
            )
        }
        "prompt" | "steer" | "follow_up" | "abort" | "interrupt" | "stop" => control(
            service,
            commands,
            invocation,
            (provider, operation),
            (deadline, cancellation),
        ),
        _ => failure("Unsupported terminal provider operation"),
    }
}

fn control(
    service: &TerminalAgentService,
    commands: &dyn AgentCommandExecutor,
    invocation: &CommandInvocation,
    operation: (AgentKind, &str),
    execution: (Instant, CommandCancellation),
) -> CommandOutcome {
    let (provider, operation) = operation;
    let (deadline, cancellation) = execution;
    if invocation
        .target
        .as_ref()
        .and_then(|target| service.record(target))
        .is_none_or(|record| record.provider != provider)
    {
        return CommandOutcome::Unavailable {
            message: "The target is not a registered terminal for this provider".to_owned(),
        };
    }
    let execute = |command: &str, arguments: Vec<String>| {
        let mut nested = CommandInvocation::new(command, arguments, invocation.caller);
        nested.target.clone_from(&invocation.target);
        if operation == "stop" && invocation.confirmation.is_some() {
            nested.confirmation = Some(nested.confirmation());
        }
        commands.execute(nested, deadline, cancellation.clone())
    };
    if matches!(operation, "abort" | "interrupt") {
        return execute(
            "terminal.write",
            vec![
                if provider == AgentKind::Claude {
                    "\u{1b}"
                } else {
                    "\u{3}"
                }
                .to_owned(),
            ],
        );
    }
    if operation == "stop" {
        let mut outcome = execute("pane.close", Vec::new());
        if let CommandOutcome::Success { warnings, .. } = &mut outcome
            && let Some(target) = &invocation.target
            && let Err(error) = service.retire(target)
        {
            warnings.push(CommandWarning {
                code: "agent_retirement_failed".to_owned(),
                message: format!("Terminal closed, but observation retirement failed: {error}"),
            });
        }
        return outcome;
    }
    let Some(message) = invocation
        .arguments
        .first()
        .filter(|message| message.len() <= 64 * 1024)
    else {
        return failure("Prompt must be at most 64 KiB");
    };
    let pasted = execute("terminal.paste", vec![message.clone()]);
    if !matches!(pasted, CommandOutcome::Success { .. }) {
        return pasted;
    }
    execute("terminal.submit", Vec::new())
}

fn parse_command(command: &str) -> Option<(AgentKind, &str)> {
    AgentKind::ALL.into_iter().find_map(|provider| {
        command
            .strip_prefix(&format!("agents.{provider}."))
            .map(|operation| (provider, operation))
    })
}

fn prepare_launch(
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    cwd: Option<&str>,
) -> Result<AgentLaunch, String> {
    let arg = |index: usize| {
        invocation
            .arguments
            .get(index)
            .filter(|argument| !argument.is_empty())
            .cloned()
    };
    let offset = usize::from(matches!(operation, "resume" | "fork"));
    let program =
        arg(offset.saturating_add(1)).unwrap_or_else(|| provider.default_program().to_owned());
    let cwd = arg(offset).or_else(|| cwd.map(str::to_owned)).or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    });
    if operation.starts_with("account.") {
        let mut launch = bootty_agents::terminal_account_launch(
            provider,
            &program,
            operation == "account.logout",
        );
        launch.cwd = cwd;
        return Ok(launch);
    }
    let arguments = arg(offset.saturating_add(2))
        .map(|encoded| {
            if encoded.len() > 64 * 1024 {
                return Err("Agent argv exceeds 64 KiB".to_owned());
            }
            serde_json::from_str::<Vec<String>>(&encoded).map_err(|error| error.to_string())
        })
        .transpose()?
        .unwrap_or_default();
    let mut launch = AgentLaunch {
        program,
        cwd,
        arguments,
        ephemeral: false,
    };
    launch.validate()?;
    if offset == 1 {
        let session = arg(0).ok_or("Choose an exact provider session")?;
        launch.arguments = launch.session_arguments(provider, &session, operation == "fork")?;
    }
    Ok(launch)
}

fn start_terminal(
    service: &Arc<TerminalAgentService>,
    commands: &dyn AgentCommandExecutor,
    invocation: &CommandInvocation,
    operation: (AgentKind, &str),
    launch: AgentLaunch,
    context: LaunchContext,
    execution: (Instant, CommandCancellation),
) -> CommandOutcome {
    let (provider, operation) = operation;
    let (deadline, cancellation) = execution;
    let account = operation.starts_with("account.");
    // A private app-server must not become the owner of a persistent backend's Codex turn.
    let persistent_codex = provider == AgentKind::Codex && !context.process_local;
    let prepared = if context.remote || account || persistent_codex {
        TerminalAgentService::prepare_unobserved(
            provider,
            launch,
            if account {
                "Account command uses the provider's own terminal"
            } else if context.remote {
                "Native activity observation needs a supported remote host bridge"
            } else {
                "Codex runs directly in this persistent terminal; app-owned observation is unavailable"
            }
            .to_owned(),
        )
    } else {
        service.prepare(provider, launch)
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return failure(&error),
    };
    let argv = match serde_json::to_string(&prepared.argv()) {
        Ok(argv) => argv,
        Err(error) => return failure(&error.to_string()),
    };
    // Shared backend validation remains authoritative; its explicit name is never reused.
    let name = bootty_mux::session_names::unique_session_name(
        provider.default_program(),
        context.names.iter().map(String::as_str),
    );
    let create = if operation == "tab" {
        let mut create = CommandInvocation::new(
            "terminal.create_tab",
            vec![argv, prepared.launch.cwd.clone().unwrap_or_default()],
            invocation.caller,
        );
        create.target = context.session;
        create
    } else {
        let mut create = CommandInvocation::new(
            "session.create",
            vec![name, prepared.launch.cwd.clone().unwrap_or_default(), argv],
            invocation.caller,
        );
        create.target = context.binding;
        create
    };
    let outcome = commands.execute(create, deadline, cancellation.clone());
    let CommandOutcome::Success {
        mut value,
        mut warnings,
    } = outcome
    else {
        return outcome;
    };
    let target = match serde_json::from_value::<CommandTarget>(
        value
            .get("terminal")
            .or_else(|| value.get("created"))
            .cloned()
            .unwrap_or_default(),
    ) {
        Ok(target) if target.kind == ResourceKind::Terminal => target,
        _ => return failure("Backend created the session without an issued terminal target"),
    };
    match service.register(prepared, target.clone(), context.binding_id) {
        Ok(record) => {
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "agent".to_owned(),
                    serde_json::to_value(record).unwrap_or_default(),
                );
            }
        }
        Err(error) => warnings.push(CommandWarning {
            code: "agent_metadata_failed".to_owned(),
            message: format!(
                "Terminal started, but native observation could not be retained: {error}"
            ),
        }),
    }
    if matches!(
        invocation.caller,
        Caller::CommandPalette | Caller::Keybinding | Caller::BuiltinKeybinding
    ) {
        let mut focus = CommandInvocation::from_action("agents.focus", invocation.caller);
        focus.target = Some(target);
        let outcome = commands.execute(focus, deadline, cancellation);
        if !matches!(outcome, CommandOutcome::Success { .. }) {
            warnings.push(CommandWarning {
                code: "agent_focus_failed".to_owned(),
                message: "Agent terminal started but could not be selected".to_owned(),
            });
        }
    }
    CommandOutcome::Success { value, warnings }
}

fn failure(message: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "terminal_agent_failed".to_owned(),
        message: message.to_owned(),
    }
}
