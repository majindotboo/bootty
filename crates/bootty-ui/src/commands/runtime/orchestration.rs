//! Finite run dispatch through the shared command mailbox; no terminal text completion inference.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};

use bootty_agents::{
    AgentCommandExecutor, AgentKind, NativeAgentService, NativeSessionRecord, NativeTurnOutcome,
    OrchestrationContext, OrchestrationDispatch, OrchestrationOutcome, OrchestrationRun,
    OrchestrationService, TerminalAgentService, TerminalAgentStatus, ToolBridge, ToolBridgeContext,
    ToolPolicy, ToolScope,
};
use bootty_control::{
    CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, CommandWarning,
    ResourceKind,
};
use bootty_mux::executor;
use serde_json::{Value, json};

use super::{
    CommandDispatch, PendingAppCommand,
    agents::AppCommandAgentExecutor,
    terminal_agents::{effective_account_directory, register_created_agent},
};
use crate::{
    commands::{ExactMuxTarget, RunCommand, capture_run_plan},
    state::AppState,
};

#[derive(Clone)]
struct Destination {
    binding: CommandTarget,
    binding_id: String,
    window: String,
    cwd: Option<String>,
    tasks: BTreeMap<String, String>,
    process_local: bool,
    remote: bool,
}

struct Host {
    runs: Arc<OrchestrationService>,
    agents: Option<Arc<TerminalAgentService>>,
    native_agents: Option<Arc<NativeAgentService>>,
    commands: Arc<dyn AgentCommandExecutor>,
    destinations: Vec<Destination>,
    enabled: BTreeSet<AgentKind>,
    deadline: Instant,
}

impl AppState {
    fn run_destinations(&self) -> Vec<Destination> {
        self.workspace
            .all_bindings()
            .filter_map(|binding| {
                let scope = binding.scope();
                let mux = binding.mux();
                let handle = self.binding_target_handle(scope, mux.binding_generation());
                let target = ExactMuxTarget::Binding(scope).command_target(
                    ResourceKind::Binding,
                    mux,
                    &handle,
                )?;
                Some(Destination {
                    binding: target,
                    binding_id: scope.persistence_value().to_string(),
                    window: self.window_state_key.clone(),
                    tasks: mux
                        .sessions()
                        .iter()
                        .filter_map(|session| {
                            let identity = self.workspace.session_identity(scope, &session.id)?;
                            let cwd = self
                                .agent_launch_context(&ExactMuxTarget::Session(
                                    scope,
                                    session.id.clone(),
                                ))
                                .cwd?;
                            Some((identity, cwd))
                        })
                        .collect(),
                    cwd: mux
                        .selected_session()
                        .and_then(|id| mux.backend_session_by_id_or_name(id))
                        .and_then(|session| session.anchor.cwd.clone())
                        .or_else(|| {
                            self.config()
                                .session
                                .working_directory
                                .as_ref()
                                .map(|path| path.to_string_lossy().into_owned())
                        }),
                    process_local: binding.backend_policy().panes.topology
                        == bootty_mux::provider::PaneTopology::ProcessLocal,
                    remote: binding.multiplexer().remote.is_some(),
                })
            })
            .collect()
    }

    /// Fresh target for explicit restart of only the run's original durable destination.
    pub fn orchestration_restart_target(&self, run: &OrchestrationRun) -> Option<CommandTarget> {
        let (window, binding_id) = run.context.destination()?;
        self.run_destinations()
            .into_iter()
            .find(|destination| {
                destination.window == window
                    && destination.binding_id == binding_id
                    && !destination.remote
            })
            .map(|destination| destination.binding)
    }

    fn run_host(&self, deadline: Instant) -> Option<Host> {
        Some(Host {
            runs: self.commands.orchestration.clone()?,
            agents: self.commands.terminal_agents.clone(),
            native_agents: self.commands.native_agents.clone(),
            commands: Arc::new(AppCommandAgentExecutor {
                creation_receipt: None,
                sender: self.commands.sender.clone(),
            }),
            destinations: self.run_destinations(),
            enabled: [AgentKind::Codex, AgentKind::Claude, AgentKind::Pi]
                .into_iter()
                .filter(|provider| {
                    self.config()
                        .agents
                        .provider(&provider.to_string())
                        .is_some_and(|config| config.enabled)
                })
                .collect(),
            deadline,
        })
    }

    pub(super) fn dispatch_orchestration_command(
        &self,
        command: RunCommand,
        invocation: CommandInvocation,
        _exact: Option<&ExactMuxTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        let Some(host) = self.run_host(deadline) else {
            return CommandDispatch::Complete(CommandOutcome::Unsupported {
                message: self
                    .commands
                    .orchestration_error
                    .clone()
                    .unwrap_or_else(|| "Durable Runs owner is not composed".to_owned()),
            });
        };
        let providers = self.config().agents.clone();
        self.dispatch_committed_command(Some((deadline, cancellation)), move || {
            let result = match command {
                RunCommand::Create { nodes } => (|| {
                    let destination = invocation.target.as_ref().and_then(|target| host.destinations.iter().find(|destination| &destination.binding == target)).ok_or("Choose an exact live Binding")?;
                    if destination.remote || !cfg!(unix) { return Err("Runs require their supported local Unix host".to_owned()); }
                    let cwd = destination.cwd.as_deref().ok_or("The Binding has no project directory")?;
                    let plan = capture_run_plan(nodes, cwd, &destination.tasks, &providers, |provider, profile| effective_account_directory(provider, profile).map(|path| path.to_string_lossy().into_owned()))?;
                    let context = OrchestrationContext::capture_destination(destination.binding.clone(), invocation.caller, destination.window.clone(), destination.binding_id.clone())?;
                    let run = host.runs.create(bootty_agents::terminal_session_id()?, context, &plan)?;
                    Ok(response(run_view(&run), Vec::new()))
                })(),
                RunCommand::CreateForSession { nodes } => host.create_for_session(nodes, invocation.target.as_ref(), invocation.caller, &providers),
                RunCommand::List => Ok(response(json!({"runs": host.runs.snapshot_arc().runs.iter().map(run_view).collect::<Vec<_>>() }), Vec::new())),
                RunCommand::Read { run } => find_run(&host.runs, &run).map(|run| response(run_view(&run), Vec::new())),
                RunCommand::Dispatch { run } => (|| {
                    exact_run(&host.runs, &run, invocation.target.as_ref())?;
                    host.dispatch(&run)
                })(),
                RunCommand::Restart { run } => (|| {
                    let saved = find_run(&host.runs, &run)?;
                    let (window, binding_id) = saved.context.destination().ok_or("Run has no captured durable host destination")?;
                    let destination = host.destinations.iter().find(|destination| destination.window == window && destination.binding_id == binding_id && Some(&destination.binding) == invocation.target.as_ref()).ok_or("Restart requires its exact fresh host destination")?;
                    let context = OrchestrationContext::capture_destination(destination.binding.clone(), saved.context.caller(), destination.window.clone(), destination.binding_id.clone())?;
                    host.runs.restart_on_binding(&run, saved.generation, context)?;
                    host.dispatch(&run)
                })(),
                RunCommand::Retry { run, node } => (|| {
                    exact_run(&host.runs, &run, invocation.target.as_ref())?;
                    host.runs.retry(&run, &node)?;
                    host.dispatch(&run)
                })(),
                RunCommand::Cancel { run } => (|| {
                    exact_run(&host.runs, &run, invocation.target.as_ref())?;
                    let running = host.runs.running_dispatches();
                    let targets = host.runs.cancel(&run)?;
                    let mut warnings = Vec::new();
                    for (dispatch, target) in running {
                        if targets.contains(&target) { host.abort(&dispatch, &target, &mut warnings); }
                    }
                    Ok(response(json!({"run": run_view(&find_run(&host.runs, &run)?), "terminals": targets}), warnings))
                })(),
            };
            result.unwrap_or_else(|error| failure(&error))
        })
    }

    pub(crate) fn reconcile_orchestration(&mut self) {
        if self
            .commands
            .pending
            .iter()
            .any(|pending| pending.label == "runs.reconcile")
        {
            return;
        }
        let Some(runs) = self.commands.orchestration.as_ref() else {
            return;
        };
        let revision = (
            self.commands
                .terminal_agents
                .as_ref()
                .map_or(0, |agents| agents.revision()),
            self.commands
                .native_agents
                .as_ref()
                .map_or(0, |agents| agents.revision()),
            runs.revision(),
        );
        if revision == self.commands.orchestration_observed_revision {
            return;
        }
        self.commands.orchestration_observed_revision = revision;
        let (deadline, cancellation) = executor::command_execution(None);
        let Some(host) = self.run_host(deadline) else {
            return;
        };
        let result = self
            .dispatch_committed_command(Some((deadline, cancellation.clone())), move || {
                host.reconcile()
            });
        if let CommandDispatch::Pending(result) = result {
            self.commands.pending.push(PendingAppCommand {
                creation_receipt: None,
                label: "runs.reconcile".to_owned(),
                user_initiated_annotation_capture: false,
                deadline,
                cancellation,
                response: None,
                result,
            });
        }
    }
}

impl Host {
    fn create_for_session(
        &self,
        nodes: Vec<crate::commands::RunNodeRequest>,
        parent: Option<&CommandTarget>,
        caller: bootty_control::Caller,
        providers: &bootty_config::config::AgentProvidersConfig,
    ) -> Result<CommandOutcome, String> {
        let parent = parent
            .filter(|target| target.kind == ResourceKind::Session)
            .ok_or("Session tasks require the exact parent native conversation")?;
        let record = self
            .native_agents
            .as_ref()
            .ok_or("Native conversation owner is unavailable")?
            .sessions()
            .into_iter()
            .find(|record| record.target() == *parent)
            .ok_or("The parent conversation is stale")?;
        let task = record
            .task_identity
            .as_deref()
            .ok_or("The parent has no saved task")?;
        let destination = self
            .destinations
            .iter()
            .find(|destination| destination.binding_id == record.binding_id)
            .ok_or("The parent Binding is unavailable")?;
        if destination.remote || !cfg!(unix) {
            return Err("Session tasks require their supported local Unix host".to_owned());
        }
        let cwd = destination
            .tasks
            .get(task)
            .ok_or("The parent's saved task is unavailable")?;
        if std::path::Path::new(cwd) != record.config.cwd
            || nodes.iter().any(|node| {
                node.provider != record.config.provider
                    || node.task_identity.as_deref() != Some(task)
            })
        {
            return Err(
                "Dependent work must use its parent provider and exact saved task".to_owned(),
            );
        }
        let plan = capture_run_plan(
            nodes,
            cwd,
            &destination.tasks,
            providers,
            |provider, profile| {
                effective_account_directory(provider, profile)
                    .map(|path| path.to_string_lossy().into_owned())
            },
        )?;
        if plan.nodes().iter().any(|node| {
            let launch = node.launch.launch();
            launch.program != record.config.program
                || launch.account_directory != record.config.account_directory
                || launch.retained(node.launch.provider()).arguments != record.config.arguments
        }) {
            return Err("The parent provider profile or account changed; choose its original configuration before creating dependent work".to_owned());
        }
        let context = OrchestrationContext::capture_destination(
            destination.binding.clone(),
            caller,
            destination.window.clone(),
            destination.binding_id.clone(),
        )?;
        let run = self
            .runs
            .create(bootty_agents::terminal_session_id()?, context, &plan)?;
        Ok(response(run_view(&run), Vec::new()))
    }

    fn reconcile(&self) -> CommandOutcome {
        let native = self
            .native_agents
            .as_ref()
            .map_or_default(|agents| agents.activities());
        let mut ready = BTreeSet::new();
        for (dispatch, target) in self.runs.running_dispatches() {
            let finished = if let Some(task) = dispatch.launch.task_identity() {
                let Some(activity) = native.iter().find(|activity| {
                    (activity.id.as_str(), activity.generation)
                        == (target.handle.as_str(), target.generation)
                        && activity.provider == dispatch.launch.provider()
                        && activity.task_identity.as_deref() == Some(task)
                        && dispatch
                            .context
                            .destination()
                            .is_some_and(|(_, binding)| activity.binding_id == binding)
                }) else {
                    continue;
                };
                let Some(receipt) = &activity.first_turn else {
                    continue;
                };
                receipt.outcome != NativeTurnOutcome::Running
                    && self
                        .runs
                        .finish_native(&dispatch.token, &target, receipt)
                        .is_ok()
            } else {
                let Some(record) = self
                    .agents
                    .as_ref()
                    .and_then(|agents| agents.record(&target))
                    .filter(|record| record.provider == dispatch.launch.provider())
                else {
                    continue;
                };
                let outcome = match record.observation.status {
                    TerminalAgentStatus::Finished => OrchestrationOutcome::Succeeded,
                    TerminalAgentStatus::Error | TerminalAgentStatus::Stopped => {
                        OrchestrationOutcome::Failed {
                            message: "Provider reported an error or stopped terminal".to_owned(),
                        }
                    }
                    _ => continue,
                };
                self.runs.finish(&dispatch.token, &target, outcome).is_ok()
            };
            if finished {
                ready.insert(dispatch.token.run_id().to_owned());
            }
        }
        let mut values = Vec::new();
        let mut warnings = Vec::new();
        for run in ready {
            match self.dispatch(&run) {
                Ok(CommandOutcome::Success {
                    value,
                    warnings: observed,
                }) => {
                    values.push(value);
                    warnings.extend(observed);
                }
                Ok(_) | Err(_) => warnings.push(warning(
                    "run_dispatch_failed",
                    "A ready task could not be dispatched; inspect its durable state",
                )),
            }
        }
        response(json!({"dispatched": values}), warnings)
    }

    fn dispatch(&self, run: &str) -> Result<CommandOutcome, String> {
        let dispatches = self.runs.claim_ready(run, bootty_agents::MAX_RUN_NODES)?;
        let mut created = Vec::new();
        let mut warnings = Vec::new();
        for dispatch in dispatches {
            if let Ok(CommandOutcome::Success {
                value,
                warnings: observed,
            }) = self.start(&dispatch)
            {
                created.push(value);
                warnings.extend(observed);
            } else {
                // Stale cancellation wins; rejection must never overwrite a newer attempt.
                let _ = self.runs.reject_dispatch(
                    &dispatch.token,
                    "Task creation failed or was cancelled".to_owned(),
                );
                warnings.push(warning(
                    "run_task_not_started",
                    "A task did not start; inspect its durable state",
                ));
            }
        }
        Ok(response(
            json!({"run": run_view(&find_run(&self.runs, run)?), "created": created}),
            warnings,
        ))
    }

    fn start(&self, dispatch: &OrchestrationDispatch) -> Result<CommandOutcome, String> {
        let guard = self.runs.begin_dispatch(&dispatch.token)?;
        let provider = dispatch.launch.provider();
        if !self.enabled.contains(&provider) {
            return Err("This provider is disabled in Settings".to_owned());
        }
        let destination = self
            .destinations
            .iter()
            .find(|destination| &destination.binding == dispatch.context.binding())
            .ok_or("The captured Binding is stale")?;
        if destination.remote || !cfg!(unix) {
            return Err("Runs require their supported local Unix host".to_owned());
        }
        if let Some(task) = dispatch.launch.task_identity() {
            if !destination.tasks.contains_key(task) {
                return Err("The captured saved task is unavailable".to_owned());
            }
            return self.start_native(dispatch, destination, &guard);
        }
        self.start_terminal(dispatch, destination, &guard)
    }

    fn start_terminal(
        &self,
        dispatch: &OrchestrationDispatch,
        destination: &Destination,
        guard: &bootty_agents::OrchestrationDispatchGuard,
    ) -> Result<CommandOutcome, String> {
        let provider = dispatch.launch.provider();
        let agents = self
            .agents
            .as_ref()
            .ok_or("Native provider owner is unavailable")?;
        let mut launch = dispatch.launch.launch().clone();
        launch
            .arguments
            .extend(["--".to_owned(), dispatch.prompt.text().to_owned()]);
        launch.validate()?;
        let executable =
            std::env::current_exe().map_err(|_| "Bootty tool executable is unavailable")?;
        let tools = ToolBridge::prepare(
            ToolBridgeContext {
                scope: ToolScope {
                    provider,
                    binding: dispatch.context.binding().clone(),
                },
                caller: dispatch.context.caller(),
                policy: ToolPolicy::own_terminal(),
                captures: Vec::new(),
                spawn: None,
            },
            &executable,
            Arc::clone(&self.commands),
        )?;
        let unobserved = (!destination.process_local && provider == AgentKind::Codex).then(|| "Codex runs directly in this persistent terminal; app-owned observation is unavailable".to_owned());
        let prepared = agents.prepare_with_tools(provider, launch, tools, unobserved)?;
        let identity = bootty_mux::snapshot::new_session_identity();
        let mut create = CommandInvocation::new(
            "session.create",
            vec![
                format!("run-{}", identity.chars().take(16).collect::<String>()),
                prepared
                    .launch
                    .cwd
                    .clone()
                    .ok_or("The captured task has no project directory")?,
                serde_json::to_string(&prepared.argv())
                    .map_err(|_| "Task argv could not be encoded")?,
                identity,
                dispatch.title.clone(),
            ],
            dispatch.context.caller(),
        );
        create.target = Some(dispatch.context.binding().clone());
        let outcome = self
            .commands
            .execute_pending(create, self.deadline, guard.cancellation());
        let CommandOutcome::Success {
            mut value,
            mut warnings,
        } = outcome
        else {
            return Ok(outcome);
        };
        match register_created_agent(
            agents,
            prepared,
            destination.binding_id.clone(),
            &mut value,
            &mut warnings,
            self.commands.as_ref(),
            (self.deadline, bootty_control::CommandCancellation::new()),
        ) {
            Ok(target) => {
                if self
                    .runs
                    .accept_terminal(&dispatch.token, dispatch.context.binding(), target.clone())
                    .is_err()
                {
                    warnings.push(warning("run_tracking_failed", "Terminal was created but the run attempt changed; its exact created IDs are retained"));
                    self.abort(dispatch, &target, &mut warnings);
                }
            }
            Err(_) => warnings.push(warning(
                "run_tracking_failed",
                "Backend created a terminal without an issued target; created IDs are retained",
            )),
        }
        if let Some(object) = value.as_object_mut() {
            object.remove("agent");
        }
        Ok(response(value, warnings))
    }

    fn start_native(
        &self,
        dispatch: &OrchestrationDispatch,
        destination: &Destination,
        guard: &bootty_agents::OrchestrationDispatchGuard,
    ) -> Result<CommandOutcome, String> {
        self.native_agents
            .as_ref()
            .ok_or("Native conversation owner is unavailable")?;
        let task = dispatch
            .launch
            .task_identity()
            .ok_or("Native work requires a saved task")?;
        let launch = dispatch.launch.launch();
        let account = launch
            .account_directory
            .as_ref()
            .ok_or("Native work has no captured account")?;
        let mut invocation = CommandInvocation::new(
            "agents.native.tab",
            vec![
                dispatch.launch.provider().to_string(),
                launch
                    .cwd
                    .clone()
                    .ok_or("Native work has no captured project directory")?,
                launch.program.clone(),
                serde_json::to_string(&launch.arguments)
                    .map_err(|_| "Native argv could not be encoded")?,
                dispatch.token.node_id().to_owned(),
                dispatch.launch.profile().unwrap_or("").to_owned(),
                task.to_owned(),
                dispatch.title.clone(),
                dispatch.prompt.text().to_owned(),
                account.clone(),
            ],
            dispatch.context.caller(),
        );
        invocation.target = Some(dispatch.context.binding().clone());
        let outcome =
            self.commands
                .execute_pending(invocation, self.deadline, guard.cancellation());
        let CommandOutcome::Success {
            value,
            mut warnings,
        } = outcome
        else {
            return Ok(outcome);
        };
        let record: NativeSessionRecord = serde_json::from_value(
            value
                .get("native")
                .cloned()
                .ok_or("Created native conversation omitted its exact result")?,
        )
        .map_err(|_| "Created native conversation returned an invalid result")?;
        let target = record.target();
        let error = native_creation_error(dispatch, &destination.binding_id, &record, &value);
        let accepted = match (&error, &record.snapshot.first_turn) {
            (None, Some(receipt)) => self.runs.accept_native(
                &dispatch.token,
                dispatch.context.binding(),
                target.clone(),
                receipt,
            ),
            (Some(error), _) => self.runs.reject_created_native(
                &dispatch.token,
                dispatch.context.binding(),
                target.clone(),
                error.chars().take(1024).collect(),
            ),
            _ => self.runs.reject_created_native(
                &dispatch.token,
                dispatch.context.binding(),
                target.clone(),
                "The first prompt has no accepted turn receipt".to_owned(),
            ),
        };
        if accepted.is_err() {
            warnings.push(warning("run_tracking_failed", "Conversation was created but its exact run attempt could not be saved; its native ID is retained"));
            self.abort(dispatch, &target, &mut warnings);
        } else if error.is_some() {
            warnings.push(warning("run_first_prompt_failed", "Conversation was created, but its first prompt could not be accepted; inspect the saved conversation"));
        }
        Ok(response(
            json!({"native": target, "task_identity": task, "first_turn": record.snapshot.first_turn, "first_prompt_error": error}),
            warnings,
        ))
    }

    fn abort(
        &self,
        dispatch: &OrchestrationDispatch,
        target: &CommandTarget,
        warnings: &mut Vec<CommandWarning>,
    ) {
        let (command, arguments) = if dispatch.launch.task_identity().is_some() {
            (
                "agents.native.interrupt".to_owned(),
                vec![target.handle.clone(), target.generation.to_string()],
            )
        } else {
            (
                format!("agents.{}.abort", dispatch.launch.provider()),
                Vec::new(),
            )
        };
        let mut invocation = CommandInvocation::new(command, arguments, dispatch.context.caller());
        invocation.target = Some(target.clone());
        // Explicit run cancellation authorizes interruption of only its exact owned attempt.
        invocation.confirmation = Some(invocation.confirmation());
        if !matches!(
            self.commands
                .execute(invocation, self.deadline, CommandCancellation::new()),
            CommandOutcome::Success { .. }
        ) {
            warnings.push(warning(
                "run_interrupt_failed",
                "Run state committed, but its exact terminal could not be interrupted",
            ));
        }
    }
}

fn native_creation_error(
    dispatch: &OrchestrationDispatch,
    binding: &str,
    record: &NativeSessionRecord,
    value: &Value,
) -> Option<String> {
    if let Some(error) = value.get("first_prompt_error").and_then(Value::as_str) {
        return Some(error.to_owned());
    }
    let launch = dispatch.launch.launch();
    if record.binding_id != binding
        || record.task_identity.as_deref() != dispatch.launch.task_identity()
        || record.config.provider != dispatch.launch.provider()
        || record.config.account_directory != launch.account_directory
        || record.config.program != launch.program
        || record.config.arguments != launch.retained(dispatch.launch.provider()).arguments
        || launch.cwd.as_deref().map(std::path::Path::new) != Some(record.config.cwd.as_path())
    {
        return Some(
            "The created native conversation does not match its captured destination".to_owned(),
        );
    }
    record
        .snapshot
        .first_turn
        .is_none()
        .then(|| "The first prompt did not return an accepted turn receipt".to_owned())
}

fn find_run(service: &OrchestrationService, id: &str) -> Result<OrchestrationRun, String> {
    service
        .snapshot_arc()
        .runs
        .iter()
        .find(|run| run.id == id)
        .cloned()
        .ok_or_else(|| "Unknown run".to_owned())
}

fn exact_run(
    service: &OrchestrationService,
    id: &str,
    binding: Option<&CommandTarget>,
) -> Result<(), String> {
    if binding != Some(find_run(service, id)?.context.binding()) {
        return Err("Run mutation requires its exact captured Binding".to_owned());
    }
    Ok(())
}

fn run_view(run: &OrchestrationRun) -> Value {
    json!({"id":run.id,"generation":run.generation,"binding":run.context.binding(),"cancelled":run.cancelled,"nodes":run.nodes.iter().map(|node| json!({"id":node.spec.id,"title":node.spec.title,"provider":node.spec.launch.provider(),"profile":node.spec.launch.profile(),"dependencies":node.spec.dependencies,"attempt":node.attempt,"state":node.state,"task_identity":node.spec.launch.task_identity()})).collect::<Vec<_>>()})
}

fn response(value: Value, warnings: Vec<CommandWarning>) -> CommandOutcome {
    // Leave shared transport envelope headroom; increase only with a bounded transport upgrade.
    if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= 768 * 1024) {
        CommandOutcome::Success { value, warnings }
    } else {
        failure("Run response exceeds the shared transport bound")
    }
}

fn warning(code: &str, message: &str) -> CommandWarning {
    CommandWarning {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}
fn failure(message: &str) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "run_failed".to_owned(),
        message: message.to_owned(),
    }
}
