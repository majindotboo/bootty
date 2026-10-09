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

mod computer_capture;
mod pane;
mod recovery;
mod spawn;

#[derive(Clone)]
pub(super) struct LaunchContext {
    pub(super) session: Option<CommandTarget>,
    parent: Option<CommandTarget>,
    pub(super) binding: Option<CommandTarget>,
    pub(super) binding_id: String,
    pub(super) cwd: Option<String>,
    pub(super) preferences: bootty_config::config::AgentProviderConfig,
    remote: bool,
    process_local: bool,
    pub(super) allow_spawn: bool,
    pub(super) computer_capture: Option<bootty_agents::ToolCapturedCommand>,
}

impl AppState {
    pub(crate) fn reconcile_agent_ownership(&mut self) {
        self.commands.refresh_agent_scopes(&self.workspace);
        self.reconcile_native_pane_ownership();
        let Some(service) = self.terminal_agent_service() else {
            return;
        };
        if let Some(error) = service.take_retirement_error() {
            self.record_error(error);
        }
        for record in service.live_records() {
            let authoritative = self.workspace.all_bindings().any(|binding| {
                binding.scope().persistence_value().to_string() == record.binding_id
                    && binding.mux().has_session_snapshot()
                    && binding.mux().unavailable_reason().is_none()
            });
            if authoritative
                && self
                    .resolve_command_target(
                        "agents.focus",
                        Some(ResourceKind::Terminal),
                        Some(&record.target),
                    )
                    .is_err()
            {
                service.retire_closed(record.target);
            }
        }
    }

    pub(super) fn dispatch_terminal_agent(
        &mut self,
        invocation: CommandInvocation,
        exact: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        self.dispatch_terminal_agent_with_launch(invocation, exact, execution, None)
    }

    pub(super) fn dispatch_captured_terminal_agent(
        &mut self,
        invocation: CommandInvocation,
        exact: &ExactMuxTarget,
        execution: Option<(Instant, CommandCancellation)>,
        launch: AgentLaunch,
    ) -> CommandDispatch {
        self.dispatch_terminal_agent_with_launch(invocation, Some(exact), execution, Some(launch))
    }

    fn dispatch_terminal_agent_with_launch(
        &mut self,
        invocation: CommandInvocation,
        exact: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
        captured_launch: Option<AgentLaunch>,
    ) -> CommandDispatch {
        let Some(service) = self.commands.terminal_agents.clone() else {
            return CommandDispatch::Complete(CommandOutcome::Unsupported {
                message: "Terminal agent owner is not composed".to_owned(),
            });
        };
        if let Some((provider, "associate")) = parse_command(&invocation.command) {
            return self
                .dispatch_terminal_agent_association(invocation, exact, execution, provider);
        }
        if let Some((provider, operation @ ("restore" | "restore.launch"))) =
            parse_command(&invocation.command)
        {
            let launch = operation == "restore.launch";
            return self
                .dispatch_restored_terminal_agent(invocation, exact, execution, provider, launch);
        }
        let (deadline, cancellation) = executor::command_execution(execution);
        if invocation.command == "agents.spawn" {
            // Child creation accepts this pending token at the final shared mux mutation gate.
            if cancellation.is_cancelled() {
                return CommandDispatch::Complete(CommandOutcome::cancelled());
            }
            if Instant::now() >= deadline {
                let _ = cancellation.cancel();
                return CommandDispatch::Complete(CommandOutcome::deadline_exceeded());
            }
        } else if let Err(error) =
            executor::begin_synchronous_command(Some((deadline, cancellation.clone())))
        {
            return CommandDispatch::Complete(command_outcome_for_mux_error(error));
        }
        if invocation.command.rsplit('.').next() == Some("start")
            && invocation
                .arguments
                .get(3)
                .is_some_and(|name| !name.is_empty())
            && !matches!(exact, Some(ExactMuxTarget::Binding(_)))
        {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "Named agent creation requires an explicit host destination".to_owned(),
            });
        }
        if let Some((provider, "history.open")) = parse_command(&invocation.command) {
            return CommandDispatch::Complete(self.open_terminal_history(provider, exact));
        }
        let mut context = self.terminal_launch_context(&invocation, exact);
        if captured_launch.is_some() {
            // Recovery restores a conversation, not previously issued spawn or capture authority.
            context.allow_spawn = false;
            context.computer_capture = None;
        }
        let (receipt, creation) = mpsc::channel();
        let commands: Arc<dyn AgentCommandExecutor> = Arc::new(AppCommandAgentExecutor {
            creation_receipt: Some(receipt),
            sender: self.commands.sender.clone(),
        });
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
                captured_launch,
            );
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::TerminalAgent {
            result: receiver,
            creation,
            observed: None,
        })
    }

    pub(super) fn poll_terminal_agent(
        pending: &mut super::PendingAppCommand,
    ) -> std::task::Poll<Option<CommandOutcome>> {
        let PendingCommandResult::TerminalAgent {
            result,
            creation,
            observed,
        } = &mut pending.result
        else {
            return std::task::Poll::Pending;
        };
        for target in creation.try_iter() {
            *observed = Some(target);
        }
        match super::poll_command_result(result) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Ok(outcome)) => std::task::Poll::Ready(Some(outcome)),
            std::task::Poll::Ready(Err(_)) => {
                std::task::Poll::Ready(Some(failure("Terminal agent worker stopped")))
            }
        }
    }
    pub(super) fn terminal_launch_context(
        &self,
        invocation: &CommandInvocation,
        exact: Option<&ExactMuxTarget>,
    ) -> LaunchContext {
        let scope = exact.map_or_else(
            || self.workspace.active.binding.scope(),
            ExactMuxTarget::scope,
        );
        let binding = self.workspace.binding(scope);
        LaunchContext {
            binding: binding.and_then(|binding| {
                let mux = binding.mux();
                let handle = self.binding_target_handle(scope, mux.binding_generation());
                ExactMuxTarget::Binding(scope).command_target(ResourceKind::Binding, mux, &handle)
            }),
            parent: exact
                .filter(|exact| matches!(exact, ExactMuxTarget::Pane(..)))
                .and_then(|exact| {
                    let binding = binding?;
                    let mux = binding.mux();
                    let handle = self.binding_target_handle(scope, mux.binding_generation());
                    exact.command_target(ResourceKind::Terminal, mux, &handle)
                }),
            session: exact.and_then(|exact| exact.ids().0).and_then(|session| {
                self.mux_resource_target(scope, ResourceKind::Session, session, None)
            }),
            binding_id: scope.persistence_value().to_string(),
            cwd: exact
                .and_then(|exact| self.agent_launch_context(exact).cwd)
                .or_else(|| {
                    binding.and_then(|binding| {
                        let mux = binding.mux();
                        mux.selected_session()
                            .and_then(|id| mux.backend_session_by_id_or_name(id))
                            .and_then(|session| session.anchor.cwd.clone())
                    })
                })
                .or_else(|| {
                    if invocation.command.rsplit('.').next() == Some("history") {
                        return None;
                    }
                    self.config()
                        .session
                        .working_directory
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned())
                }),
            preferences: parse_command(&invocation.command)
                .map(|(provider, _)| provider)
                .or_else(|| {
                    invocation.target.as_ref().and_then(|target| {
                        let service = self.terminal_agent_service()?;
                        if invocation.command == "agents.spawn"
                            && let Some(selector) = invocation.arguments.get(1)
                        {
                            return selector.parse::<u64>().ok().and_then(|selector| {
                                service
                                    .spawn_parent_for_attachment(
                                        target,
                                        selector,
                                        invocation.caller,
                                    )
                                    .map(|(_, lease)| lease.scope().provider)
                            });
                        }
                        service.record(target).map(|record| record.provider)
                    })
                })
                .and_then(|provider| self.config().agents.provider(&provider.to_string()))
                .cloned()
                .unwrap_or_default(),
            remote: binding.is_some_and(|binding| binding.multiplexer().remote.is_some()),
            process_local: binding.is_some_and(|binding| {
                binding.backend_policy().panes.topology
                    == bootty_mux::provider::PaneTopology::ProcessLocal
            }),
            allow_spawn: self.config().agents.allow_spawn,
            computer_capture: computer_capture::computer_launch_capture(
                self.config().computer,
                invocation.caller,
                self.current_command_target(ResourceKind::ApplicationWindow),
                self.native_computer_window,
            ),
        }
    }
}

fn execute(
    service: &Arc<TerminalAgentService>,
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    context: LaunchContext,
    deadline: Instant,
    cancellation: CommandCancellation,
    captured_launch: Option<AgentLaunch>,
) -> CommandOutcome {
    if invocation.command == "agents.spawn" {
        return spawn::execute(
            service,
            commands,
            invocation,
            &context,
            deadline,
            cancellation,
        );
    }
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
        "history" => saved_history(invocation, provider, &context),
        "provider.status" | "account.status" => {
            inspect_provider(service, invocation, provider, operation, &context)
        }
        "start" | "tab" | "pane" | "resume" | "fork" | "account.login" | "account.logout"
        | "provider.update" => {
            if !context.preferences.enabled
                && matches!(operation, "start" | "tab" | "pane" | "resume" | "fork")
            {
                return CommandOutcome::Denied {
                    message: "This provider is disabled in Settings".to_owned(),
                };
            }
            if context.remote
                && matches!(operation, "resume" | "fork")
                && invocation.arguments.get(5).is_some()
            {
                return CommandOutcome::Unsupported {
                    message: "Saved provider history resumes on its owning local host".to_owned(),
                };
            }
            if operation == "pane"
                && let Err(outcome) = pane::validate(invocation, &context)
            {
                return outcome;
            }
            let launch = match prepare_native_launch(
                invocation,
                provider,
                operation,
                &context,
                captured_launch,
            ) {
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
            commands.as_ref(),
            invocation,
            (provider, operation),
            (deadline, cancellation),
        ),
        _ => failure("Unsupported terminal provider operation"),
    }
}

fn prepare_native_launch(
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    context: &LaunchContext,
    captured_launch: Option<AgentLaunch>,
) -> Result<AgentLaunch, String> {
    let mut launch = if let Some(launch) = captured_launch {
        if !matches!(operation, "resume" | "fork") || launch.account_directory.is_none() {
            return Err("Recovery requires an exact captured native account launch".to_owned());
        }
        launch
    } else {
        prepare_launch(
            invocation,
            provider,
            operation,
            context.cwd.as_deref(),
            &context.preferences,
        )?
    };
    if !context.remote && cfg!(unix) && launch.account_directory.is_none() {
        launch.account_directory = Some(
            effective_account_directory(provider, None)?
                .to_string_lossy()
                .into_owned(),
        );
    }
    launch.validate()?;
    Ok(launch)
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
    let Some(record) = invocation
        .target
        .as_ref()
        .and_then(|target| service.record(target))
        .filter(|record| record.provider == provider)
    else {
        return CommandOutcome::Unavailable {
            message: "The target is not a registered terminal for this provider".to_owned(),
        };
    };
    if operation != "stop" {
        let Some(live) = service
            .live_records()
            .into_iter()
            .find(|live| live.target == record.target && live.provider == provider)
        else {
            return CommandOutcome::Unavailable {
                message: "This terminal has no live native provider observation".to_owned(),
            };
        };
        // These operations submit normal TUI input; a completed turn is ready for a new prompt.
        let completed_turn = live.observation.status
            == bootty_agents::TerminalAgentStatus::Finished
            || (provider == AgentKind::Codex
                && live.observation.status == bootty_agents::TerminalAgentStatus::Stopped);
        let next_prompt = completed_turn && matches!(operation, "prompt" | "follow_up" | "steer");
        if !next_prompt
            && !matches!(
                live.observation.status,
                bootty_agents::TerminalAgentStatus::Idle
                    | bootty_agents::TerminalAgentStatus::Working
                    | bootty_agents::TerminalAgentStatus::Waiting
                    | bootty_agents::TerminalAgentStatus::Approval
                    | bootty_agents::TerminalAgentStatus::Input
            )
        {
            return CommandOutcome::Unavailable {
                message: format!(
                    "Native provider observation is {:?}; terminal input is unavailable",
                    live.observation.status
                ),
            };
        }
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
                if matches!(provider, AgentKind::Claude | AgentKind::Pi) {
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

fn selected_profile<'a>(
    preferences: &'a bootty_config::config::AgentProviderConfig,
    explicit: Option<&str>,
) -> Result<Option<&'a bootty_config::config::AgentProfileConfig>, String> {
    match explicit {
        Some("") => Ok(None),
        Some(id) => preferences
            .profiles
            .get(id)
            .map(Some)
            .ok_or_else(|| "The selected provider profile is unavailable".to_owned()),
        None => Ok(preferences.selected_profile()),
    }
}

pub(super) fn effective_account_directory(
    provider: AgentKind,
    profile: Option<&bootty_config::config::AgentProfileConfig>,
) -> Result<std::path::PathBuf, String> {
    let directory = if let Some(directory) = profile.and_then(|profile| profile.directory.as_ref())
    {
        std::path::PathBuf::from(directory.as_str())
    } else if let Some(directory) = std::env::var_os(provider.account_directory_variable()) {
        directory.into()
    } else {
        let home = bootty_git::home_dir()
            .ok_or("The provider's default account directory is unavailable")?;
        home.join(match provider {
            AgentKind::Codex => ".codex",
            AgentKind::Claude => ".claude",
            AgentKind::Pi => ".pi/agent",
        })
    };
    let text = directory
        .to_str()
        .ok_or("The provider account directory must be UTF-8")?;
    if text.len() > 8192
        || text.chars().any(char::is_control)
        || !directory.is_absolute()
        || directory
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("The provider account directory must be a bounded absolute path without control characters".to_owned());
    }
    Ok(directory)
}

const HISTORY_RESPONSE_MAX_BYTES: usize = 96 * 1024;

#[derive(serde::Serialize)]
struct HistoryEnvelope {
    entries: Vec<bootty_agents::TerminalHistoryEntry>,
    account_directory: std::path::PathBuf,
    omitted_entries: usize,
}

fn bounded_history(
    entries: Vec<bootty_agents::TerminalHistoryEntry>,
    account_directory: std::path::PathBuf,
) -> Result<HistoryEnvelope, String> {
    let mut response = HistoryEnvelope {
        entries: Vec::new(),
        account_directory,
        omitted_entries: entries.len(),
    };
    // The initial omitted count reserves enough digits for every retained-entry decision.
    let mut bytes = serde_json::to_vec(&response)
        .map_err(|error| error.to_string())?
        .len();
    if bytes > HISTORY_RESPONSE_MAX_BYTES {
        return Err("The provider account directory exceeds the response limit".to_owned());
    }
    for entry in entries {
        let encoded = serde_json::to_vec(&entry).map_err(|error| error.to_string())?;
        let next = bytes
            .saturating_add(encoded.len())
            .saturating_add(usize::from(!response.entries.is_empty()));
        if next > HISTORY_RESPONSE_MAX_BYTES {
            break;
        }
        bytes = next;
        response.entries.push(entry);
        response.omitted_entries = response.omitted_entries.saturating_sub(1);
    }
    Ok(response)
}

fn saved_history(
    invocation: &CommandInvocation,
    provider: AgentKind,
    context: &LaunchContext,
) -> CommandOutcome {
    if context.remote {
        return CommandOutcome::Unsupported {
            message:
                "Provider history requires its owning local host; remote history is unavailable"
                    .to_owned(),
        };
    }
    let query = || {
        let profile = selected_profile(
            &context.preferences,
            invocation.arguments.get(1).map(String::as_str),
        )?;
        if provider == AgentKind::Pi
            && profile.is_some_and(|profile| {
                profile
                    .arguments
                    .iter()
                    .any(|arg| arg == "--session-dir" || arg.starts_with("--session-dir="))
            })
        {
            return Err("Pi history with a custom session directory is unsupported".to_owned());
        }
        let account = effective_account_directory(provider, profile)?;
        if let Some(expected) = invocation.arguments.get(2)
            && account != std::path::Path::new(expected)
        {
            return Err("The provider account changed; reopen history before querying".to_owned());
        }
        let cwd = match invocation.arguments.first() {
            Some(cwd) if cwd.is_empty() => None,
            Some(cwd) => Some(cwd.as_str()),
            None => Some(
                context
                    .cwd
                    .as_deref()
                    .ok_or("The current session has no project directory; choose All projects")?,
            ),
        };
        let entries =
            bootty_agents::terminal_provider_history(&bootty_agents::TerminalHistoryQuery {
                provider,
                program: provider_program(provider, &context.preferences),
                account_directory: &account,
                cwd: cwd.map(std::path::Path::new),
                limit: 200,
            })?;
        bounded_history(entries, account)
    };
    query().map_or_else(|error| failure(&error), serialized_command_outcome)
}

fn launch_account_directory(
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    profile: Option<&bootty_config::config::AgentProfileConfig>,
) -> Result<Option<String>, String> {
    if matches!(operation, "resume" | "fork")
        && let Some(expected) = invocation.arguments.get(5)
    {
        if effective_account_directory(provider, profile)? != std::path::Path::new(expected) {
            return Err("The provider account changed; reopen history before resuming".to_owned());
        }
        return Ok(Some(expected.clone()));
    }
    Ok(profile.and_then(|profile| profile.directory.clone()))
}

pub(super) fn prepare_launch(
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    cwd: Option<&str>,
    preferences: &bootty_config::config::AgentProviderConfig,
) -> Result<AgentLaunch, String> {
    let arg = |index: usize| {
        invocation
            .arguments
            .get(if operation == "pane" && index < 3 {
                index.saturating_add(1)
            } else {
                index
            })
            .filter(|argument| !argument.is_empty())
            .cloned()
    };
    let offset = usize::from(matches!(operation, "resume" | "fork"));
    let program = arg(offset.saturating_add(1))
        .unwrap_or_else(|| provider_program(provider, preferences).to_owned());
    let cwd = arg(offset).or_else(|| cwd.map(str::to_owned)).or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    });
    if operation == "provider.update" {
        let mut launch = bootty_agents::terminal_provider_update(provider, &program)?;
        launch.cwd = cwd;
        return Ok(launch);
    }
    let profile = selected_profile(
        preferences,
        if matches!(operation, "start" | "pane" | "resume" | "fork") {
            invocation.arguments.get(4).map(String::as_str)
        } else if operation == "tab" {
            invocation.arguments.get(3).map(String::as_str)
        } else {
            None
        },
    )?;
    let account_directory = launch_account_directory(invocation, provider, operation, profile)?;
    if operation.starts_with("account.") {
        let mut launch = bootty_agents::terminal_account_launch(
            provider,
            &program,
            operation == "account.logout",
        );
        launch.cwd = cwd;
        launch.account_directory = profile.and_then(|profile| profile.directory.clone());
        launch.validate()?;
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
        .unwrap_or_else(|| profile.map_or_else(Vec::new, |profile| profile.arguments.clone()));
    let mut launch = AgentLaunch {
        program,
        cwd,
        arguments,
        ephemeral: false,
        account_directory,
    };
    launch.validate()?;
    if offset == 1 {
        let session = arg(0).ok_or("Choose an exact provider session")?;
        launch.arguments = launch.session_arguments(provider, &session, operation == "fork")?;
    }
    Ok(launch)
}

pub(super) fn launch_tools(
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    context: &LaunchContext,
    launch: &mut AgentLaunch,
) -> (Option<bootty_agents::ToolBridge>, Option<CommandWarning>) {
    if !matches!(operation, "start" | "tab" | "pane" | "resume" | "fork") {
        return (None, None);
    }
    let (spawn, warning) = if context.allow_spawn && !context.remote && cfg!(unix) {
        match capture_spawn(invocation, provider, operation, context, launch) {
            Ok(spawn) => (Some(spawn), None),
            Err(error) => (
                None,
                Some(CommandWarning {
                    code: "agent_spawning_unavailable".to_owned(),
                    message: format!(
                        "Child spawning is unavailable: {}",
                        error.chars().take(384).collect::<String>()
                    ),
                }),
            ),
        }
    } else {
        (None, None)
    };
    let prepare = || {
        if context.remote || !cfg!(unix) {
            return Err("Agent tools require a supported local Unix host".to_owned());
        }
        let binding = context
            .binding
            .clone()
            .ok_or("Agent tools require the launch's exact Binding")?;
        let executable = std::env::current_exe()
            .map_err(|_| "Bootty tool executable is unavailable".to_owned())?;
        bootty_agents::ToolBridge::prepare(
            bootty_agents::ToolBridgeContext {
                scope: bootty_agents::ToolScope { provider, binding },
                caller: invocation.caller,
                policy: bootty_agents::ToolPolicy {
                    spawn_children: spawn.is_some(),
                    computer_capture: context.computer_capture.is_some(),
                    ..bootty_agents::ToolPolicy::own_terminal()
                },
                captures: context.computer_capture.iter().cloned().collect(),
                spawn,
            },
            &executable,
            Arc::clone(commands),
        )
    };
    match prepare() {
        Ok(tools) => (Some(tools), warning),
        Err(error) => (
            None,
            Some(CommandWarning {
                code: "agent_tools_unavailable".to_owned(),
                message: format!(
                    "Agent tools are unavailable: {}",
                    error.chars().take(384).collect::<String>()
                ),
            }),
        ),
    }
}

fn capture_spawn(
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    context: &LaunchContext,
    launch: &mut AgentLaunch,
) -> Result<bootty_agents::ToolSpawnContext, String> {
    let id = if operation == "tab" {
        invocation.arguments.get(3).map(String::as_str)
    } else if matches!(operation, "start" | "pane" | "resume" | "fork") {
        invocation.arguments.get(4).map(String::as_str)
    } else {
        None
    }
    .unwrap_or(&context.preferences.selected);
    let profile = selected_profile(&context.preferences, Some(id))?;
    let directory = launch.account_directory.as_deref().map_or_else(
        || effective_account_directory(provider, profile),
        |directory| Ok(std::path::PathBuf::from(directory)),
    )?;
    let directory = directory
        .to_str()
        .ok_or("The account directory is not UTF-8")?
        .to_owned();
    let mut captured = launch.clone();
    captured.account_directory = Some(directory);
    captured.validate()?;
    launch.account_directory = captured.account_directory;
    Ok(bootty_agents::ToolSpawnContext {
        profile: (!id.is_empty()).then(|| id.to_owned()),
    })
}

#[allow(clippy::too_many_lines)]
fn start_terminal(
    service: &Arc<TerminalAgentService>,
    commands: &Arc<dyn AgentCommandExecutor>,
    invocation: &CommandInvocation,
    operation: (AgentKind, &str),
    mut launch: AgentLaunch,
    context: LaunchContext,
    execution: (Instant, CommandCancellation),
) -> CommandOutcome {
    let (provider, operation) = operation;
    let (deadline, cancellation) = execution;
    let name = (operation == "start")
        .then(|| invocation.arguments.get(3))
        .flatten()
        .filter(|name| !name.is_empty());
    if name.is_some()
        && invocation
            .target
            .as_ref()
            .is_none_or(|target| target.kind != ResourceKind::Binding)
    {
        return failure("Named agent creation requires an explicit host destination");
    }
    let account = operation.starts_with("account.") || operation == "provider.update";
    if (context.remote || cfg!(windows)) && launch.account_directory.is_some() {
        return CommandOutcome::Unsupported { message: "Account directory selection requires a local POSIX terminal; remote accounts remain owned by their host".to_owned() };
    }
    if operation == "provider.update" && context.remote {
        return CommandOutcome::Unsupported {
            message: "Update providers on their owning host".to_owned(),
        };
    }
    if operation == "provider.update" && service.has_live_provider(provider) {
        return failure(
            "Close this provider's active terminal tabs before updating its executable",
        );
    }
    if operation == "resume"
        && let Some(session) = invocation.arguments.first()
        && let Some(target) = service.live_session(
            provider,
            session,
            &context.binding_id,
            launch.account_directory.as_deref(),
        )
    {
        let mut focus = CommandInvocation::from_action("agents.focus", invocation.caller);
        focus.target = Some(target.clone());
        return match commands.execute(focus, deadline, cancellation) {
            CommandOutcome::Success { warnings, .. } => CommandOutcome::Success {
                value: serde_json::json!({"terminal": target, "reused": true}),
                warnings,
            },
            outcome => outcome,
        };
    }
    // A private app-server must not become the owner of a persistent backend's Codex turn.
    let persistent_codex = provider == AgentKind::Codex && !context.process_local;
    let (tools, tools_warning) = launch_tools(
        commands,
        invocation,
        provider,
        operation,
        &context,
        &mut launch,
    );
    let unobserved = (context.remote || account || persistent_codex).then(|| {
        if account {
            "Account command uses the provider's own terminal"
        } else if context.remote {
            "Native activity observation needs a supported remote host bridge"
        } else {
            "Codex runs directly in this persistent terminal; app-owned observation is unavailable"
        }
        .to_owned()
    });
    let prepared = if let Some(tools) = tools {
        service.prepare_with_tools(provider, launch, tools, unobserved)
    } else if let Some(detail) = unobserved {
        TerminalAgentService::prepare_unobserved(provider, launch, detail)
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
    let mut create = if operation == "pane" {
        pane::create_invocation(invocation, &context, argv, prepared.launch.cwd.clone())
    } else if let Some(name) = name {
        let mut arguments = vec![
            name.clone(),
            prepared.launch.cwd.clone().unwrap_or_default(),
            argv,
        ];
        arguments.extend(invocation.arguments.iter().skip(5).take(2).cloned());
        CommandInvocation::new("session.create", arguments, invocation.caller)
    } else {
        CommandInvocation::new(
            "terminal.create_tab",
            vec![argv, prepared.launch.cwd.clone().unwrap_or_default()],
            invocation.caller,
        )
    };
    // A named request keeps the caller's captured host through preparation and backend creation.
    if operation != "pane" {
        create.target = if name.is_some() {
            invocation.target.clone()
        } else {
            context.session
        };
    }
    let outcome = commands.execute(create, deadline, cancellation.clone());
    let CommandOutcome::Success {
        mut value,
        mut warnings,
    } = outcome
    else {
        return outcome;
    };
    warnings.extend(tools_warning);
    let target = match register_created_agent(
        service,
        prepared,
        context.binding_id,
        &mut value,
        &mut warnings,
        commands.as_ref(),
        (deadline, cancellation.clone()),
    ) {
        Ok(target) => target,
        Err(error) => return failure(&error),
    };
    if account
        || invocation.caller == Caller::Internal
        || matches!(
            invocation.caller,
            Caller::CommandPalette | Caller::Keybinding | Caller::BuiltinKeybinding
        )
    {
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

pub(super) fn register_created_agent(
    service: &TerminalAgentService,
    prepared: bootty_agents::PreparedTerminalAgent,
    binding_id: String,
    value: &mut serde_json::Value,
    warnings: &mut Vec<CommandWarning>,
    commands: &dyn AgentCommandExecutor,
    execution: (Instant, CommandCancellation),
) -> Result<CommandTarget, String> {
    let target = match serde_json::from_value::<CommandTarget>(
        value
            .get("terminal")
            .or_else(|| value.get("created"))
            .cloned()
            .unwrap_or_default(),
    ) {
        Ok(target) if target.kind == ResourceKind::Terminal => target,
        _ => {
            return Err("Backend created the session without an issued terminal target".to_owned());
        }
    };
    match service.register(prepared, target.clone(), binding_id) {
        Ok(record) => {
            let mut associate = CommandInvocation::new(
                format!("agents.{}.associate", record.provider),
                vec![target.handle.clone()],
                Caller::Internal,
            );
            associate.target = Some(target.clone());
            let associated = commands.execute(associate, execution.0, execution.1);
            let record: bootty_agents::TerminalAgentRecord = if let CommandOutcome::Success {
                value,
                ..
            } = associated
            {
                serde_json::from_value(value).map_err(|error| error.to_string())?
            } else {
                return Err(super::command_outcome_message(&associated)
                    .unwrap_or_else(|| "Terminal agent topology association failed".to_owned()));
            };
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "terminal".to_owned(),
                    serde_json::to_value(&target).unwrap_or_default(),
                );
                object.insert(
                    "agent".to_owned(),
                    serde_json::to_value(record).unwrap_or_default(),
                );
            }
        }
        Err(error) => warnings.push(CommandWarning {
            code: "agent_metadata_failed".to_owned(),
            message: format!("Terminal started, but integration setup was incomplete: {error}"),
        }),
    }
    Ok(target)
}

fn failure(message: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "terminal_agent_failed".to_owned(),
        message: message.to_owned(),
    }
}

fn provider_program(
    provider: AgentKind,
    preferences: &bootty_config::config::AgentProviderConfig,
) -> &str {
    if preferences.program.is_empty() {
        provider.default_program()
    } else {
        &preferences.program
    }
}

fn inspect_provider(
    service: &TerminalAgentService,
    invocation: &CommandInvocation,
    provider: AgentKind,
    operation: &str,
    context: &LaunchContext,
) -> CommandOutcome {
    if context.remote {
        return CommandOutcome::Unsupported { message: "Provider inspection requires a local host; remote account status is not queried locally".to_owned() };
    }
    let program = invocation
        .arguments
        .get(1)
        .filter(|arg| !arg.is_empty())
        .map_or_else(
            || provider_program(provider, &context.preferences),
            String::as_str,
        );
    let profile = context.preferences.selected_profile();
    let directory = profile.and_then(|profile| profile.directory.as_deref());
    let selector = if let Some(provider) = invocation
        .arguments
        .first()
        .filter(|value| !value.is_empty())
    {
        Some(bootty_agents::PiAccountSelector::provider(provider))
    } else if provider == AgentKind::Pi {
        match profile
            .map(|profile| bootty_agents::PiAccountSelector::from_arguments(&profile.arguments))
            .transpose()
        {
            Ok(selector) => selector.flatten(),
            Err(error) => return failure(&error),
        }
    } else {
        None
    };

    if operation == "account.status" {
        return bootty_agents::terminal_account_status_with_pi_selector_in(
            provider,
            program,
            selector.as_ref(),
            directory,
        )
        .map_or_else(|error| failure(&error), serialized_command_outcome);
    }
    serialized_command_outcome(service.inspect_provider_with_pi_selector(
        provider,
        program,
        directory,
        selector.as_ref(),
    ))
}
