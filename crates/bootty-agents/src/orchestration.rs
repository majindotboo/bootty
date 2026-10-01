//! Durable coordination over existing agent sessions. This service never owns a process.

use std::{
    collections::BTreeSet,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use bootty_control::{
    ArgumentSchema, CommandDescriptor, CommandOutcome, CommandTarget, CommandWarning,
    CompactSchema, MutationClass, ResourceKind, ValueType,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AgentCommandExecutor, AgentInvocation, AgentKind,
    commands::{failed, nested_invocation, success},
};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_TEXT: usize = 16 * 1024;
const MAX_RUNS: usize = 64;
const MAX_TASKS: usize = 128;
const MAX_WORKERS: usize = 16;
const MAX_MESSAGES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationTaskState {
    Pending,
    Dispatching,
    /// The provider accepted the prompt; completion still requires an explicit report.
    Running,
    Completed,
    Failed,
    /// Bootty restarted before it could prove a terminal result. Retry is explicit.
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationTask {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub state: OrchestrationTaskState,
    pub worker: Option<String>,
    pub dispatch: Option<String>,
    pub target: Option<CommandTarget>,
    pub report: Option<String>,
    pub outcome: Option<CommandOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationWorker {
    pub id: String,
    pub name: String,
    pub provider: AgentKind,
    pub target: CommandTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationMessageState {
    Sending,
    Delivered,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationMessage {
    pub id: String,
    pub worker: String,
    pub body: String,
    pub from: Option<CommandTarget>,
    pub state: OrchestrationMessageState,
    pub outcome: Option<CommandOutcome>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationRun {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub finished: bool,
    pub tasks: Vec<OrchestrationTask>,
    pub workers: Vec<OrchestrationWorker>,
    pub messages: Vec<OrchestrationMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Store {
    version: u32,
    next_id: u64,
    runs: Vec<OrchestrationRun>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            next_id: 1,
            runs: Vec::new(),
        }
    }
}

impl Store {
    fn validate(&self) -> Result<(), String> {
        if self.next_id == 0 || self.runs.len() > MAX_RUNS {
            return Err("invalid orchestration run count or identifier sequence".to_owned());
        }
        let mut identifiers = BTreeSet::new();
        for run in &self.runs {
            if run.tasks.len() > MAX_TASKS
                || run.workers.len() > MAX_WORKERS
                || run.messages.len() > MAX_MESSAGES
            {
                return Err("saved orchestration run exceeds retention limits".to_owned());
            }
            let ids = std::iter::once((&run.id, "run"))
                .chain(run.tasks.iter().map(|task| (&task.id, "task")))
                .chain(run.workers.iter().map(|worker| (&worker.id, "worker")))
                .chain(run.messages.iter().map(|message| (&message.id, "message")))
                .chain(
                    run.tasks
                        .iter()
                        .filter_map(|task| task.dispatch.as_ref().map(|id| (id, "dispatch"))),
                );
            for (id, prefix) in ids {
                let (label, suffix) = id
                    .rsplit_once('-')
                    .ok_or("invalid saved orchestration identifier")?;
                let sequence: u64 = suffix
                    .parse()
                    .map_err(|_| "invalid saved orchestration sequence")?;
                if label != prefix
                    || sequence == 0
                    || sequence >= self.next_id
                    || !identifiers.insert(sequence)
                {
                    return Err("saved orchestration identifiers are inconsistent".to_owned());
                }
            }
            if run.tasks.iter().any(|task| {
                task.worker
                    .as_ref()
                    .is_some_and(|id| !run.workers.iter().any(|worker| &worker.id == id))
            }) || run
                .messages
                .iter()
                .any(|message| !run.workers.iter().any(|worker| worker.id == message.worker))
            {
                return Err("saved orchestration work references an unknown worker".to_owned());
            }
            if run.finished
                && run
                    .tasks
                    .iter()
                    .any(|task| task.state != OrchestrationTaskState::Completed)
            {
                return Err("finished orchestration run has unfinished tasks".to_owned());
            }
        }
        Ok(())
    }

    fn id(&mut self, prefix: &str) -> Result<String, String> {
        let id = format!("{prefix}-{}", self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("orchestration identifier limit reached")?;
        Ok(id)
    }

    fn run(&self, id: &str) -> Result<&OrchestrationRun, String> {
        self.runs
            .iter()
            .find(|run| run.id == id)
            .ok_or_else(|| format!("unknown run `{id}`"))
    }

    fn run_mut(&mut self, id: &str) -> Result<&mut OrchestrationRun, String> {
        let run = self
            .runs
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or_else(|| format!("unknown run `{id}`"))?;
        if run.finished {
            return Err("run is finished".to_owned());
        }
        Ok(run)
    }
}

impl OrchestrationRun {
    fn task_mut(&mut self, id: &str) -> Result<&mut OrchestrationTask, String> {
        self.tasks
            .iter_mut()
            .find(|task| task.id == id)
            .ok_or_else(|| format!("unknown task `{id}`"))
    }

    fn worker(&self, id: &str) -> Result<&OrchestrationWorker, String> {
        self.workers
            .iter()
            .find(|worker| worker.id == id)
            .ok_or_else(|| format!("unknown worker `{id}`"))
    }
}

/// One application-owned service per identity-specific state file. Call it on a worker thread:
/// mutations commit synchronously before the accepted snapshot or any prompt is published.
pub struct OrchestrationService {
    path: PathBuf,
    cli: Option<PathBuf>,
    commands: Arc<dyn AgentCommandExecutor>,
    store: Mutex<Store>,
}

impl OrchestrationService {
    /// Restore coordination without assuming that an in-flight command or agent finished.
    ///
    /// # Errors
    /// Returns unreadable, oversized, incompatible, or invalid state and commit failures.
    pub fn open(path: &Path, commands: Arc<dyn AgentCommandExecutor>) -> Result<Self, String> {
        let mut store: Store = match fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(u64::try_from(MAX_BYTES + 1).map_err(|error| error.to_string())?)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() > MAX_BYTES {
                    return Err("orchestration state exceeds 2 MiB".to_owned());
                }
                serde_json::from_slice(&bytes)
                    .map_err(|e| format!("read orchestration state: {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(e) => return Err(e.to_string()),
        };
        if store.version != 1 {
            return Err("unsupported orchestration state version".to_owned());
        }
        store.validate()?;
        let mut recovered = false;
        for run in &mut store.runs {
            for task in &mut run.tasks {
                if matches!(
                    task.state,
                    OrchestrationTaskState::Dispatching | OrchestrationTaskState::Running
                ) {
                    task.state = OrchestrationTaskState::Interrupted;
                    recovered = true;
                }
            }
            for message in &mut run.messages {
                if message.state == OrchestrationMessageState::Sending {
                    message.state = OrchestrationMessageState::Interrupted;
                    recovered = true;
                }
            }
        }
        let service = Self {
            path: path.to_owned(),
            cli: None,
            commands,
            store: Mutex::new(store.clone()),
        };
        if recovered {
            service.write(&store)?;
        }
        Ok(service)
    }

    /// Supply this identity's executable so native workers report to the same Bootty owner.
    #[must_use]
    pub fn with_cli(mut self, executable: impl Into<PathBuf>) -> Self {
        self.cli = Some(executable.into());
        self
    }

    /// A committed projection for the UI. Process liveness remains owned by native providers.
    ///
    /// # Errors
    /// Returns an error when the state lock is poisoned.
    pub fn runs(&self) -> Result<Vec<OrchestrationRun>, String> {
        self.store
            .lock()
            .map(|store| store.runs.clone())
            .map_err(|_| "orchestration state lock poisoned".to_owned())
    }

    /// Execute the same catalog commands from the UI, CLI, socket, and native agents.
    #[must_use]
    pub fn invoke(&self, request: &AgentInvocation) -> CommandOutcome {
        if request.cancellation.is_cancelled() {
            return CommandOutcome::cancelled();
        }
        if Instant::now() >= request.deadline {
            return CommandOutcome::deadline_exceeded();
        }
        let command = request.invocation.command.as_str();
        let Some(spec) = SPECS.iter().find(|spec| command == spec.id) else {
            return failed("unknown_command", "unknown orchestration command");
        };
        let args = &request.invocation.arguments;
        if args.len() != spec.arguments.len()
            || args
                .iter()
                .any(|arg| arg.trim().is_empty() || arg.len() > MAX_TEXT)
        {
            return failed(
                "invalid_arguments",
                format!(
                    "expected {} nonempty argument(s), at most {MAX_TEXT} bytes each",
                    spec.arguments.len()
                ),
            );
        }
        if spec.mutation == MutationClass::Read {
            return self.read(command, args);
        }
        // Hosts may already have claimed the cancellation token before calling the service.
        if !request.cancellation.try_start() && request.cancellation.is_cancelled() {
            return CommandOutcome::cancelled();
        }
        match command {
            "orchestration.task.dispatch" => self.dispatch(request),
            "orchestration.message.send" => self.send(request),
            _ => self.mutate(|store| apply(store, command, args, request)),
        }
    }

    fn read(&self, command: &str, args: &[String]) -> CommandOutcome {
        let Ok(store) = self.store.lock() else {
            return failed("state_unavailable", "orchestration state lock poisoned");
        };
        let value = match (command, args) {
            ("orchestration.run.list", []) => Ok(json!({"runs": store.runs})),
            ("orchestration.run.show", [run]) => store.run(run).map(|run| json!({"run": run})),
            ("orchestration.task.list", [run]) => store.run(run).map(|run| json!({"tasks": run.tasks})),
            ("orchestration.worker.list", [run]) => store.run(run).map(|run| json!({"workers": run.workers})),
            ("orchestration.message.inbox", [run, worker]) => store.run(run).and_then(|run| {
                run.worker(worker)?;
                Ok(json!({"messages": run.messages.iter().filter(|message| &message.worker == worker).collect::<Vec<_>>()}))
            }),
            _ => Err("unknown orchestration read command".to_owned()),
        };
        value.map_or_else(|error| failed("invalid_state", error), success)
    }

    fn mutate(&self, mutation: impl FnOnce(&mut Store) -> Result<Value, String>) -> CommandOutcome {
        let Ok(mut store) = self.store.lock() else {
            return failed("state_unavailable", "orchestration state lock poisoned");
        };
        let mut candidate = store.clone();
        let value = match mutation(&mut candidate) {
            Ok(value) => value,
            Err(error) => return failed("invalid_state", error),
        };
        match self.write(&candidate) {
            Ok(warnings) => {
                *store = candidate;
                CommandOutcome::Success { value, warnings }
            }
            Err(error) => failed("persistence_failed", error),
        }
    }

    fn write(&self, store: &Store) -> Result<Vec<CommandWarning>, String> {
        let bytes = serde_json::to_vec(store).map_err(|e| e.to_string())?;
        // Deliberate bound: remove settled runs before retaining more history. Grow this only
        // with paged storage; unbounded prompts must never become an application memory queue.
        if bytes.len() > MAX_BYTES {
            return Err("orchestration state exceeds 2 MiB; remove settled runs".to_owned());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        if let Ok(metadata) = fs::metadata(&self.path) {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                    .map_err(|e| e.to_string())?;
            }
        }
        let committed = bootty_write::WriteTarget::resolve(&self.path)
            .map_err(|e| e.into_io().to_string())?
            .lock()
            .map_err(|e| e.to_string())?
            .replace(&bytes, bootty_write::NewFileMode::Private)
            .map_err(|e| e.into_io().to_string())?;
        Ok(match committed {
            bootty_write::CommitOutcome::Confirmed => Vec::new(),
            bootty_write::CommitOutcome::CommittedWithDurabilityWarning(error) => {
                vec![CommandWarning {
                    code: "durability_warning".to_owned(),
                    message: error.to_string(),
                }]
            }
        })
    }

    fn dispatch(&self, request: &AgentInvocation) -> CommandOutcome {
        let [run_id, task_id, worker_id] = request.invocation.arguments.as_slice() else {
            return failed("invalid_arguments", "dispatch requires run, task, worker");
        };
        let mut delivery = None;
        let prepared = self.mutate(|store| {
            let dispatch = store.id("dispatch")?;
            let worker = store.run(run_id)?.worker(worker_id)?.clone();
            if store.runs.iter().flat_map(|run| &run.tasks).any(|task|
                task.target.as_ref() == Some(&worker.target) && matches!(task.state, OrchestrationTaskState::Dispatching | OrchestrationTaskState::Running)) {
                return Err("worker already has an active task".to_owned());
            }
            let run = store.run_mut(run_id)?;
            let run_id = run.id.clone();
            let goal = run.goal.clone();
            let task = run.task_mut(task_id)?;
            if task.state != OrchestrationTaskState::Pending { return Err("task must be pending; retry a failed or interrupted task first".to_owned()); }
            task.state = OrchestrationTaskState::Dispatching;
            task.worker = Some(worker.id.clone());
            task.dispatch = Some(dispatch.clone());
            task.target = Some(worker.target.clone());
            let mut prompt = format!("Run goal: {goal}\nTask: {}\n\n{}\n\nBootty task: {run_id} / {} / {dispatch}\nReport completion through orchestration.task.complete or failure through orchestration.task.fail with run, task, dispatch, report and this exact session target.", task.title, task.prompt, task.id);
            if let Some(cli) = &self.cli {
                let invocation = report_command(cli, &run_id, &task.id, &dispatch, &worker.target);
                prompt.push_str("\nAfter verifying the result, execute the following command and replace REPORT with a short result. Use orchestration.task.fail instead if the task failed.\n");
                prompt.push_str(&invocation);
            }
            delivery = Some((worker, dispatch, prompt));
            Ok(json!({"task": task}))
        });
        let Some((worker, dispatch, prompt)) = delivery else {
            return prepared;
        };
        if !matches!(prepared, CommandOutcome::Success { .. }) {
            return prepared;
        }
        let outcome = self.deliver(&worker, prompt, request);
        let settled = self.mutate(|store| {
            let task = store.run_mut(run_id)?.task_mut(task_id)?;
            if task.dispatch.as_ref() != Some(&dispatch)
                || task.state != OrchestrationTaskState::Dispatching
            {
                // A native worker can report completion before the prompt call returns.
                return Ok(json!({"task": task}));
            }
            task.state = if matches!(outcome, CommandOutcome::Success { .. }) {
                OrchestrationTaskState::Running
            } else {
                OrchestrationTaskState::Failed
            };
            task.outcome = Some(outcome.clone());
            Ok(json!({"task": task}))
        });
        delivery_result(prepared, settled, outcome)
    }

    fn send(&self, request: &AgentInvocation) -> CommandOutcome {
        let [run_id, worker_id, body] = request.invocation.arguments.as_slice() else {
            return failed(
                "invalid_arguments",
                "message send requires run, worker, body",
            );
        };
        let mut delivery = None;
        let prepared = self.mutate(|store| {
            let id = store.id("message")?;
            let run = store.run_mut(run_id)?;
            if run.messages.len() >= MAX_MESSAGES {
                return Err("run message limit reached".to_owned());
            }
            let worker = run.worker(worker_id)?.clone();
            let message = OrchestrationMessage {
                id: id.clone(),
                worker: worker.id.clone(),
                body: body.clone(),
                from: request.invocation.target.clone(),
                state: OrchestrationMessageState::Sending,
                outcome: None,
            };
            let value = json!({"message": message});
            run.messages.push(message);
            delivery = Some((worker, id));
            Ok(value)
        });
        let Some((worker, id)) = delivery else {
            return prepared;
        };
        if !matches!(prepared, CommandOutcome::Success { .. }) {
            return prepared;
        }
        let outcome = self.deliver(
            &worker,
            format!("Bootty message {id} in {run_id}:\n{body}"),
            request,
        );
        let settled = self.mutate(|store| {
            let message = store
                .run_mut(run_id)?
                .messages
                .iter_mut()
                .find(|message| message.id == id)
                .ok_or("message disappeared")?;
            message.state = if matches!(outcome, CommandOutcome::Success { .. }) {
                OrchestrationMessageState::Delivered
            } else {
                OrchestrationMessageState::Failed
            };
            message.outcome = Some(outcome.clone());
            Ok(json!({"message": message}))
        });
        delivery_result(prepared, settled, outcome)
    }

    fn deliver(
        &self,
        worker: &OrchestrationWorker,
        prompt: String,
        request: &AgentInvocation,
    ) -> CommandOutcome {
        if Instant::now() >= request.deadline {
            return CommandOutcome::deadline_exceeded();
        }
        self.commands.execute(
            nested_invocation(
                &format!("agents.{}.prompt", worker.provider),
                vec![prompt],
                Some(worker.target.clone()),
            ),
            request.deadline,
            request.cancellation.clone(),
        )
    }
}

fn delivery_result(
    prepared: CommandOutcome,
    mut settled: CommandOutcome,
    delivered: CommandOutcome,
) -> CommandOutcome {
    if let CommandOutcome::Failed { code, message } = &settled
        && code == "persistence_failed"
    {
        return failed(
            "delivery_unrecorded",
            format!(
                "Agent delivery returned {delivered:?}, but its result could not be committed: {message}. Inspect the session before retrying."
            ),
        );
    }
    if let CommandOutcome::Success { warnings, .. } = &mut settled {
        if let CommandOutcome::Success {
            warnings: first, ..
        } = prepared
        {
            warnings.extend(first);
        }
        if !matches!(delivered, CommandOutcome::Success { .. }) {
            return delivered;
        }
    }
    settled
}

fn report_command(
    cli: &Path,
    run: &str,
    task: &str,
    dispatch: &str,
    target: &CommandTarget,
) -> String {
    let arguments = [
        cli.to_string_lossy().into_owned(),
        "orchestration.task.complete".to_owned(),
        "--run".to_owned(),
        run.to_owned(),
        "--task".to_owned(),
        task.to_owned(),
        "--dispatch".to_owned(),
        dispatch.to_owned(),
        "--report".to_owned(),
        "REPORT".to_owned(),
        "--target".to_owned(),
        format!("{}@{}", target.handle, target.generation),
    ];
    let command = arguments
        .iter()
        .map(|argument| {
            let escaped = if cfg!(windows) {
                argument.replace('\'', "''")
            } else {
                argument.replace('\'', "'\\''")
            };
            format!("'{escaped}'")
        })
        .collect::<Vec<_>>()
        .join(" ");
    if cfg!(windows) {
        format!("& {command}")
    } else {
        command
    }
}

fn apply(
    store: &mut Store,
    command: &str,
    args: &[String],
    request: &AgentInvocation,
) -> Result<Value, String> {
    match (command, args) {
        (
            "orchestration.run.create" | "orchestration.run.finish" | "orchestration.run.remove",
            _,
        ) => apply_run(store, command, args),
        ("orchestration.task.create", [run_id, title, prompt]) => {
            let id = store.id("task")?;
            let run = store.run_mut(run_id)?;
            if run.tasks.len() >= MAX_TASKS {
                return Err("run task limit reached".to_owned());
            }
            let task = OrchestrationTask {
                id,
                title: title.clone(),
                prompt: prompt.clone(),
                state: OrchestrationTaskState::Pending,
                worker: None,
                dispatch: None,
                target: None,
                report: None,
                outcome: None,
            };
            let value = json!({"task": task});
            run.tasks.push(task);
            Ok(value)
        }
        ("orchestration.worker.attach", [run_id, name, provider_name]) => {
            attach_worker(store, run_id, name, provider_name, request)
        }
        (
            "orchestration.task.complete" | "orchestration.task.fail",
            [run_id, task_id, dispatch, report],
        ) => {
            let task = store.run_mut(run_id)?.task_mut(task_id)?;
            if !matches!(
                task.state,
                OrchestrationTaskState::Dispatching | OrchestrationTaskState::Running
            ) {
                return Err("task has no active dispatch".to_owned());
            }
            if !request.target_supplied
                || request.invocation.target.as_ref() != task.target.as_ref()
                || task.dispatch.as_ref() != Some(dispatch)
            {
                return Err(
                    "report must match the exact dispatched session, generation, and dispatch id"
                        .to_owned(),
                );
            }
            task.state = if command.ends_with(".complete") {
                OrchestrationTaskState::Completed
            } else {
                OrchestrationTaskState::Failed
            };
            task.report = Some(report.clone());
            Ok(json!({"task": task}))
        }
        ("orchestration.task.retry", [run_id, task_id]) => {
            let task = store.run_mut(run_id)?.task_mut(task_id)?;
            if !matches!(
                task.state,
                OrchestrationTaskState::Failed | OrchestrationTaskState::Interrupted
            ) {
                return Err("retry requires a failed or interrupted task".to_owned());
            }
            task.state = OrchestrationTaskState::Pending;
            task.worker = None;
            task.dispatch = None;
            task.target = None;
            task.report = None;
            task.outcome = None;
            Ok(json!({"task": task}))
        }
        _ => Err("unknown orchestration mutation".to_owned()),
    }
}

fn apply_run(store: &mut Store, command: &str, args: &[String]) -> Result<Value, String> {
    match (command, args) {
        ("orchestration.run.create", [title, goal]) => {
            if store.runs.len() >= MAX_RUNS {
                return Err("run limit reached; remove settled runs".to_owned());
            }
            let run = OrchestrationRun {
                id: store.id("run")?,
                title: title.clone(),
                goal: goal.clone(),
                finished: false,
                tasks: Vec::new(),
                workers: Vec::new(),
                messages: Vec::new(),
            };
            let value = json!({"run": run});
            store.runs.push(run);
            Ok(value)
        }
        ("orchestration.run.finish", [run_id]) => {
            let run = store.run_mut(run_id)?;
            if run.tasks.is_empty()
                || run
                    .tasks
                    .iter()
                    .any(|task| task.state != OrchestrationTaskState::Completed)
                || run
                    .messages
                    .iter()
                    .any(|message| message.state == OrchestrationMessageState::Sending)
            {
                return Err(
                    "finish requires completed tasks and no message delivery in progress"
                        .to_owned(),
                );
            }
            run.finished = true;
            Ok(json!({"run": run}))
        }
        ("orchestration.run.remove", [run_id]) => {
            let run = store.run(run_id)?;
            if run.tasks.iter().any(|task| {
                matches!(
                    task.state,
                    OrchestrationTaskState::Dispatching | OrchestrationTaskState::Running
                )
            }) || run
                .messages
                .iter()
                .any(|message| message.state == OrchestrationMessageState::Sending)
            {
                return Err("cannot remove a run with active work".to_owned());
            }
            store.runs.retain(|run| &run.id != run_id);
            Ok(json!({"removed": run_id}))
        }
        _ => Err("invalid run arguments".to_owned()),
    }
}

fn attach_worker(
    store: &mut Store,
    run_id: &str,
    name: &str,
    provider_name: &str,
    request: &AgentInvocation,
) -> Result<Value, String> {
    let target = request
        .invocation
        .target
        .clone()
        .filter(|target| {
            request.target_supplied
                && target.generation != 0
                && !target.handle.is_empty()
                && matches!(target.kind, ResourceKind::Terminal | ResourceKind::Session)
        })
        .ok_or("attach requires an explicit live terminal or native session target")?;
    let provider = AgentKind::ALL
        .into_iter()
        .find(|provider| provider.to_string() == provider_name)
        .ok_or("unknown agent provider")?;
    let id = store.id("worker")?;
    let run = store.run_mut(run_id)?;
    if run
        .workers
        .iter()
        .any(|worker| worker.target == target && worker.name != name)
    {
        return Err("worker target is already attached".to_owned());
    }
    if let Some(worker) = run.workers.iter_mut().find(|worker| worker.name == name) {
        if run.tasks.iter().any(|task| {
            task.worker.as_ref() == Some(&worker.id)
                && matches!(
                    task.state,
                    OrchestrationTaskState::Dispatching | OrchestrationTaskState::Running
                )
        }) {
            return Err("cannot replace a worker with active work".to_owned());
        }
        worker.target = target;
        worker.provider = provider;
        return Ok(json!({"worker": worker}));
    }
    if run.workers.len() >= MAX_WORKERS {
        return Err("run worker limit reached".to_owned());
    }
    let worker = OrchestrationWorker {
        id,
        name: name.to_owned(),
        provider,
        target,
    };
    let value = json!({"worker": worker});
    run.workers.push(worker);
    Ok(value)
}

struct Spec {
    id: &'static str,
    title: &'static str,
    arguments: &'static [&'static str],
    mutation: MutationClass,
}

const SPECS: &[Spec] = &[
    Spec {
        id: "orchestration.run.create",
        title: "Create coordination run",
        arguments: &["title", "goal"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.run.list",
        title: "List coordination runs",
        arguments: &[],
        mutation: MutationClass::Read,
    },
    Spec {
        id: "orchestration.run.show",
        title: "Inspect coordination run",
        arguments: &["run"],
        mutation: MutationClass::Read,
    },
    Spec {
        id: "orchestration.run.finish",
        title: "Finish coordination run",
        arguments: &["run"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.run.remove",
        title: "Remove settled coordination run",
        arguments: &["run"],
        mutation: MutationClass::Destructive,
    },
    Spec {
        id: "orchestration.task.create",
        title: "Create coordination task",
        arguments: &["run", "title", "prompt"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.task.list",
        title: "List coordination tasks",
        arguments: &["run"],
        mutation: MutationClass::Read,
    },
    Spec {
        id: "orchestration.task.dispatch",
        title: "Dispatch coordination task",
        arguments: &["run", "task", "worker"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.task.complete",
        title: "Report task completion",
        arguments: &["run", "task", "dispatch", "report"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.task.fail",
        title: "Report task failure",
        arguments: &["run", "task", "dispatch", "report"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.task.retry",
        title: "Retry coordination task",
        arguments: &["run", "task"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.worker.attach",
        title: "Attach agent as worker",
        arguments: &["run", "name", "provider"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.worker.list",
        title: "List coordination workers",
        arguments: &["run"],
        mutation: MutationClass::Read,
    },
    Spec {
        id: "orchestration.message.send",
        title: "Send worker message",
        arguments: &["run", "worker", "body"],
        mutation: MutationClass::Write,
    },
    Spec {
        id: "orchestration.message.inbox",
        title: "Inspect worker messages",
        arguments: &["run", "worker"],
        mutation: MutationClass::Read,
    },
];

#[must_use]
pub fn orchestration_command_descriptors() -> Vec<CommandDescriptor> {
    SPECS
        .iter()
        .map(|spec| CommandDescriptor {
            id: spec.id.to_owned(),
            title: spec.title.to_owned(),
            description: String::new(),
            mutation: spec.mutation,
            arguments: CompactSchema {
                arguments: spec
                    .arguments
                    .iter()
                    .map(|name| ArgumentSchema {
                        name: (*name).to_owned(),
                        value_type: ValueType::String,
                        required: true,
                        choices: Vec::new(),
                        minimum: None,
                        maximum: None,
                    })
                    .collect(),
            },
            target: matches!(
                spec.id,
                "orchestration.worker.attach"
                    | "orchestration.task.complete"
                    | "orchestration.task.fail"
            )
            .then_some(ResourceKind::Session),
            palette: false,
        })
        .collect()
}
