use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use bootty_control::{
    CommandCancellation, CommandCatalogSource, CommandOutcome, CommandTarget, ControlEventSender,
};
use serde_json::{Value, json};

use crate::{
    commands::{
        AgentCommandExecutor, AgentInvocation, command_descriptors, failed, nested_invocation,
        success,
    },
    events::{AgentEvent, AgentEventPublisher},
    provider::{AgentKind, AgentPaneKey, AgentSource, AgentState, AgentStatus},
};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
// Bound retained hook state even when a provider disappears without sending its shutdown event.
const PANE_STATE_LIMIT: usize = 1024;

#[derive(Default)]
struct AgentStateStore {
    attention_sequence: u64,
    panes: BTreeMap<AgentKind, BTreeMap<AgentPaneKey, AgentState>>,
    last_claude_pane: Option<AgentPaneKey>,
}

/// Resolves a hook's backend-local pane label to the active Space or binding scope. The app host
/// supplies this from its existing session/binding lookup; agents do not depend on mux types.
pub trait AgentPaneResolver: Send + Sync {
    fn scope_for_pane(&self, pane: &str) -> Option<String>;
}

impl<F> AgentPaneResolver for F
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    fn scope_for_pane(&self, pane: &str) -> Option<String> {
        self(pane)
    }
}

struct ServiceScope;

impl AgentPaneResolver for ServiceScope {
    fn scope_for_pane(&self, _pane: &str) -> Option<String> {
        None
    }
}

/// Native owner for Pi, Codex, and Claude state, command forwarding, and event publication.
/// Vendor adapters remain static assets in `integration`; no user source is loaded here.
pub struct AgentService {
    commands: Arc<dyn AgentCommandExecutor>,
    events: Arc<dyn AgentEventPublisher>,
    resolver: Arc<dyn AgentPaneResolver>,
    state: Mutex<AgentStateStore>,
    /// Serializes retirement with topic publication, but never the control mailbox round trip.
    publication: Mutex<()>,
    generation: u64,
    active: AtomicBool,
}

impl Drop for AgentService {
    fn drop(&mut self) {
        self.retire();
    }
}

impl AgentService {
    #[must_use]
    pub fn new(
        commands: Arc<dyn AgentCommandExecutor>,
        events: Arc<dyn AgentEventPublisher>,
    ) -> Self {
        Self {
            commands,
            events,
            resolver: Arc::new(ServiceScope),
            state: Mutex::new(AgentStateStore::default()),
            publication: Mutex::new(()),
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            active: AtomicBool::new(true),
        }
    }

    #[must_use]
    pub fn new_with_resolver(
        commands: Arc<dyn AgentCommandExecutor>,
        events: Arc<dyn AgentEventPublisher>,
        resolver: Arc<dyn AgentPaneResolver>,
    ) -> Self {
        let mut service = Self::new(commands, events);
        service.resolver = resolver;
        service
    }

    #[must_use]
    pub fn with_control(
        commands: Arc<dyn AgentCommandExecutor>,
        events: ControlEventSender,
    ) -> Self {
        Self::new(commands, Arc::new(events))
    }

    #[must_use]
    pub fn with_control_and_resolver(
        commands: Arc<dyn AgentCommandExecutor>,
        events: ControlEventSender,
        resolver: Arc<dyn AgentPaneResolver>,
    ) -> Self {
        Self::new_with_resolver(commands, Arc::new(events), resolver)
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Retire this incarnation. In-flight commands and event publications check this bit before
    /// each externally visible operation, so a restarted service cannot receive stale state.
    pub fn retire(&self) {
        // Close the publication boundary before clearing state. A topic callback that already
        // holds the gate completes its in-memory publication first; no callback can start after
        // this point. Do not wait for an agent event's ControlPlane response here: the response
        // may require this UI owner to drain its mailbox, and retirement must stay non-blocking.
        let Ok(_gate) = self.publication.lock() else {
            self.active.store(false, Ordering::Release);
            return;
        };
        self.active.store(false, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.panes.clear();
            state.last_claude_pane = None;
        }
    }

    /// Execute one provider command against injected host commands.
    #[must_use]
    pub fn invoke(&self, request: &AgentInvocation) -> CommandOutcome {
        if !self.ready(&request.cancellation, request.deadline) {
            return self.lifecycle_failure(&request.cancellation, request.deadline);
        }
        let command = request.invocation.command.as_str();
        let Some((provider, operation)) = parse_command(command) else {
            return failed(
                "unknown_command",
                format!("unknown agent command `{command}`"),
            );
        };
        if !provider_command_exists(command) {
            return failed(
                "unknown_command",
                format!("unknown agent command `{command}`"),
            );
        }
        let args = &request.invocation.arguments;
        if let Some(error) = validate_arguments(operation, args) {
            return failed("invalid_arguments", error);
        }
        match operation {
            Operation::Start => self.start(provider, request, None),
            Operation::Resume => self.start(provider, request, Some(false)),
            Operation::Fork => self.start(provider, request, Some(true)),
            Operation::Prompt | Operation::Steer | Operation::FollowUp => {
                self.submit(request, args.first(), "message")
            }
            Operation::Abort | Operation::Interrupt => self.write_control(request),
            Operation::Stop => self.stop(request),
            Operation::State => {
                if args.len() > 1 {
                    failed(
                        "invalid_arguments",
                        "state accepts at most one pane argument",
                    )
                } else {
                    let pane = args
                        .first()
                        .filter(|pane| !pane.is_empty())
                        .map(String::as_str);
                    let scope = request
                        .scope
                        .clone()
                        .or_else(|| pane.and_then(|pane| self.resolver.scope_for_pane(pane)));
                    success(
                        self.snapshot_scoped(provider, scope.as_deref(), pane)
                            .to_value(),
                    )
                }
            }
            Operation::Ingest => self.ingest_command(provider, request),
            Operation::Acknowledge => self.acknowledge(provider, request),
        }
    }

    /// Apply one raw provider event, publish it on its stable topic, and return the new snapshot.
    #[must_use]
    pub fn ingest(
        &self,
        provider: AgentKind,
        pane: Option<&str>,
        payload: Value,
        deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> CommandOutcome {
        self.ingest_scoped(provider, None, pane, payload, deadline, cancellation)
    }

    /// Apply an event with a host captured Space or binding scope. A missing scope falls back to
    /// the injected resolver, then to this service incarnation's private scope.
    #[must_use]
    pub fn ingest_scoped(
        &self,
        provider: AgentKind,
        scope: Option<&str>,
        pane: Option<&str>,
        payload: Value,
        deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> CommandOutcome {
        if !self.ready(cancellation, deadline) {
            return self.lifecycle_failure(cancellation, deadline);
        }
        let pane = pane
            .filter(|pane| !pane.is_empty())
            .unwrap_or("unknown")
            .to_owned();
        let scope = self.scope_for_pane(&pane, scope);
        let key = AgentPaneKey::new(scope.clone(), pane.clone());
        if pane.len() > 8192 || scope.len() > 8192 {
            return failed(
                "invalid_event",
                "Agent pane and scope must be at most 8192 bytes",
            );
        }
        let snapshot = {
            let Ok(mut store) = self.state.lock() else {
                return failed("state_unavailable", "agent state lock is poisoned");
            };
            if !self.is_active() {
                return self.lifecycle_failure(cancellation, deadline);
            }
            let next_sequence = store.attention_sequence.saturating_add(1);
            let (snapshot, remove) = {
                let provider_states = store.panes.entry(provider).or_default();
                if provider_states.len() >= PANE_STATE_LIMIT && !provider_states.contains_key(&key)
                {
                    return failed("state_limit", "Agent provider pane state limit reached");
                }
                let state = provider_states
                    .entry(key.clone())
                    .or_insert_with(|| AgentState::new(provider));
                update_agent_state(state, &payload, next_sequence);
                (state.clone(), state.status == AgentStatus::Stopped)
            };
            store.attention_sequence = store.attention_sequence.max(snapshot.attention_sequence);
            if provider == AgentKind::Claude && !remove {
                store.last_claude_pane = Some(key.clone());
            }
            // Remove under the same lock as the transition, so a concurrent SessionStart cannot
            // recreate the pane between applying SessionEnd and removing its old state.
            if remove {
                if let Some(states) = store.panes.get_mut(&provider) {
                    states.remove(&key);
                    if states.is_empty() {
                        store.panes.remove(&provider);
                    }
                }
                if provider == AgentKind::Claude && store.last_claude_pane.as_ref() == Some(&key) {
                    store.last_claude_pane = None;
                }
            }
            snapshot
        };
        if !self.ready(cancellation, deadline) {
            return self.lifecycle_failure(cancellation, deadline);
        }
        let event = AgentEvent {
            provider,
            scope,
            pane,
            kind: provider.event_kind(),
            state: snapshot.clone(),
            payload,
        };
        if !self.begin_publication() {
            return self.lifecycle_failure(cancellation, deadline);
        }
        let mut warnings = Vec::new();
        if let Err(error) = self.events.publish(
            provider.module(),
            self.generation,
            provider.topic(),
            event.to_value(),
            deadline,
            cancellation,
        ) {
            warnings.push(bootty_control::CommandWarning {
                code: "event_publish_failed".to_owned(),
                message: error,
            });
        }
        CommandOutcome::Success {
            value: snapshot.to_value(),
            warnings,
        }
    }

    #[must_use]
    pub fn snapshot(&self, provider: AgentKind, pane: Option<&str>) -> AgentState {
        let scope = pane.and_then(|pane| self.resolver.scope_for_pane(pane));
        self.snapshot_scoped(provider, scope.as_deref(), pane)
    }

    fn scope_for_pane(&self, pane: &str, captured: Option<&str>) -> String {
        captured
            .map(str::to_owned)
            .or_else(|| self.resolver.scope_for_pane(pane))
            .unwrap_or_else(|| format!("service:{}", self.generation))
    }

    #[must_use]
    pub fn snapshot_scoped(
        &self,
        provider: AgentKind,
        scope: Option<&str>,
        pane: Option<&str>,
    ) -> AgentState {
        let Ok(store) = self.state.lock() else {
            return AgentState::new(provider);
        };
        let Some(states) = store.panes.get(&provider) else {
            return AgentState::new(provider);
        };
        if let Some(pane) = pane {
            if let Some(scope) = scope {
                return states
                    .get(&AgentPaneKey::new(scope, pane))
                    .cloned()
                    .unwrap_or_else(|| AgentState::new(provider));
            }
            let mut matching = states.iter().filter(|(key, _)| key.pane == pane);
            let Some((_, state)) = matching.next() else {
                return AgentState::new(provider);
            };
            return matching
                .next()
                .map_or_else(|| state.clone(), |_| AgentState::new(provider));
        }
        if provider == AgentKind::Claude {
            if let Some(pane) = store.last_claude_pane.as_ref()
                && scope.is_none_or(|scope| scope == pane.scope.as_str())
            {
                return states
                    .get(pane)
                    .cloned()
                    .unwrap_or_else(|| AgentState::new(provider));
            }
            return states
                .iter()
                .find(|(key, _)| scope.is_none_or(|scope| scope == key.scope.as_str()))
                .map_or_else(|| AgentState::new(provider), |(_, state)| state.clone());
        }
        states
            .iter()
            .find(|(key, _)| scope.is_none_or(|scope| scope == key.scope.as_str()))
            .map_or_else(|| AgentState::new(provider), |(_, state)| state.clone())
    }

    #[must_use]
    pub fn pane_states(&self, provider: AgentKind) -> Vec<(AgentPaneKey, AgentState)> {
        let Ok(store) = self.state.lock() else {
            return Vec::new();
        };
        store
            .panes
            .get(&provider)
            .map(|states| {
                states
                    .iter()
                    .map(|(pane, state)| (pane.clone(), state.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn acknowledge(&self, provider: AgentKind, request: &AgentInvocation) -> CommandOutcome {
        let Some(sequence) = request
            .invocation
            .arguments
            .first()
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return failed("invalid_arguments", "An attention sequence is required");
        };
        let (Some(scope), Some(pane)) = (&request.scope, &request.launch_context.pane) else {
            return failed("target_required", "Acknowledgement needs a live agent pane");
        };
        let key = AgentPaneKey::new(scope, pane);
        let Ok(mut store) = self.state.lock() else {
            return failed("state_unavailable", "agent state lock is poisoned");
        };
        let Some(state) = store
            .panes
            .get_mut(&provider)
            .and_then(|panes| panes.get_mut(&key))
        else {
            return failed(
                "agent_unavailable",
                "Agent is no longer reported on this pane",
            );
        };
        state.acknowledged_sequence = state
            .acknowledged_sequence
            .max(sequence.min(state.attention_sequence));
        success(state.to_value())
    }

    fn prepare_launch(
        &self,
        provider: AgentKind,
        request: &AgentInvocation,
        fork: Option<bool>,
    ) -> Result<crate::AgentLaunch, CommandOutcome> {
        let state = request.launch_context.pane.as_deref().map_or_else(
            || AgentState::new(provider),
            |pane| self.snapshot_scoped(provider, request.scope.as_deref(), Some(pane)),
        );
        let args = &request.invocation.arguments;
        let offset = usize::from(fork.is_some());
        let saved = fork.and(state.launch.as_ref());
        let cwd = args
            .get(offset)
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| saved.and_then(|launch| launch.cwd.clone()))
            .or_else(|| fork.and_then(|_| state.cwd.clone()))
            .or_else(|| request.launch_context.cwd.clone());
        let program = args
            .get(offset.saturating_add(1))
            .filter(|value| !value.is_empty())
            .cloned()
            .or_else(|| saved.map(|launch| launch.program.clone()))
            .unwrap_or_else(|| provider.default_program().to_owned());
        let arguments = if let Some(encoded) = args
            .get(offset.saturating_add(2))
            .filter(|value| !value.is_empty())
        {
            if encoded.len() > 64 * 1024 {
                return Err(failed("invalid_arguments", "Agent argv exceeds 64 KiB"));
            }
            match serde_json::from_str::<Vec<String>>(encoded) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return Err(failed(
                        "invalid_arguments",
                        format!("argv must be a JSON string array: {error}"),
                    ));
                }
            }
        } else {
            saved
                .map(|launch| launch.arguments.clone())
                .unwrap_or_default()
        };
        let mut launch = crate::AgentLaunch {
            program,
            cwd,
            arguments,
            ephemeral: saved.is_some_and(|launch| launch.ephemeral),
        };
        if let Err(error) = launch.validate() {
            return Err(failed("invalid_arguments", error));
        }
        if let Some(fork) = fork {
            let session = args
                .first()
                .filter(|value| !value.is_empty())
                .map(String::as_str)
                .or_else(|| match provider {
                    AgentKind::Pi => state
                        .session_file
                        .as_deref()
                        .or(state.session_id.as_deref()),
                    AgentKind::Codex => state.thread_id.as_deref(),
                    AgentKind::Claude => state.session_id.as_deref(),
                });
            let Some(session) = session else {
                return Err(failed(
                    "session_required",
                    "No reported session ID; supply one explicitly",
                ));
            };
            match launch
                .retained(provider)
                .session_arguments(provider, session, fork)
            {
                Ok(arguments) => launch.arguments = arguments,
                Err(error) => return Err(failed("session_unavailable", error)),
            }
        }
        Ok(launch)
    }

    fn start(
        &self,
        provider: AgentKind,
        request: &AgentInvocation,
        fork: Option<bool>,
    ) -> CommandOutcome {
        let launch = match self.prepare_launch(provider, request, fork) {
            Ok(launch) => launch,
            Err(outcome) => return outcome,
        };
        let command = match launch.shell_command(provider, request.launch_context.shell) {
            Ok(command) => command,
            Err(error) => return failed("invalid_arguments", error),
        };
        // Resume/fork always creates a new tab: never type a launch command into a live agent.
        let target = if request.target_supplied && fork.is_none() {
            request.invocation.target.clone()
        } else {
            let value = match self.nested(
                request,
                "new_tab",
                Vec::new(),
                request.launch_context.new_tab.clone(),
                false,
            ) {
                Ok(value) => value,
                Err(outcome) => return outcome,
            };
            match serde_json::from_value::<CommandTarget>(
                value.get("created").cloned().unwrap_or(Value::Null),
            ) {
                Ok(target) => Some(target),
                Err(_) => {
                    return failed(
                        "new_tab_failed",
                        "new tab did not return its terminal target",
                    );
                }
            }
        };
        let Some(target) = target else {
            return failed("target_required", "agent start needs a terminal target");
        };
        if let Err(outcome) = self.nested(
            request,
            "terminal.paste",
            vec![command],
            Some(target.clone()),
            false,
        ) {
            return outcome;
        }
        match self.nested(
            request,
            "terminal.submit",
            Vec::new(),
            Some(target.clone()),
            false,
        ) {
            Ok(_) => success(
                json!({"started": true, "submitted": true, "target": target, "launch": launch.retained(provider)}),
            ),
            Err(outcome) => outcome,
        }
    }

    fn submit(
        &self,
        request: &AgentInvocation,
        message: Option<&String>,
        name: &str,
    ) -> CommandOutcome {
        let Some(message) = message else {
            return failed("invalid_arguments", format!("{name} is required"));
        };
        let target = request.invocation.target.clone();
        if let Err(outcome) = self.nested(
            request,
            "terminal.paste",
            vec![message.clone()],
            target.clone(),
            false,
        ) {
            return outcome;
        }
        match self.nested(request, "terminal.submit", Vec::new(), target, false) {
            Ok(_) => success(json!({"sent": true})),
            Err(outcome) => outcome,
        }
    }

    fn write_control(&self, request: &AgentInvocation) -> CommandOutcome {
        let target = request.invocation.target.clone();
        match self.nested(
            request,
            "terminal.write",
            vec!["\u{3}".to_owned()],
            target,
            false,
        ) {
            Ok(_) => success(json!({"sent": true})),
            Err(outcome) => outcome,
        }
    }

    fn stop(&self, request: &AgentInvocation) -> CommandOutcome {
        let Some(target) = request.invocation.target.clone() else {
            return failed("target_required", "agent stop needs a pane target");
        };
        match self.nested(request, "kill_pane", Vec::new(), Some(target), true) {
            Ok(_) => success(json!({"stopped": true})),
            Err(outcome) => outcome,
        }
    }

    fn ingest_command(&self, provider: AgentKind, request: &AgentInvocation) -> CommandOutcome {
        let Some(event) = request.invocation.arguments.first() else {
            return failed("invalid_arguments", "event is required");
        };
        let mut payload = match serde_json::from_str::<Value>(event) {
            Ok(payload) => payload,
            Err(error) => return failed("invalid_event", error.to_string()),
        };
        if let Some(source) = request
            .invocation
            .arguments
            .get(2)
            .filter(|value| !value.is_empty())
        {
            if source.len() > 64 * 1024 {
                return failed("invalid_launch", "Launch context exceeds 64 KiB");
            }
            let launch = match serde_json::from_str::<crate::AgentLaunch>(source) {
                Ok(launch) => launch,
                Err(error) => return failed("invalid_launch", error.to_string()),
            };
            if let Err(error) = launch.validate() {
                return failed("invalid_launch", error);
            }
            if let Some(payload) = payload.as_object_mut() {
                payload.insert(
                    "bootty_launch".to_owned(),
                    launch.retained(provider).to_value(),
                );
            }
        }
        let pane = request
            .invocation
            .arguments
            .get(1)
            .filter(|pane| !pane.is_empty())
            .map_or("unknown", String::as_str)
            .to_owned();
        let scope = self.scope_for_pane(&pane, request.scope.as_deref());
        self.ingest_scoped(
            provider,
            Some(scope.as_str()),
            Some(pane.as_str()),
            payload,
            request.deadline,
            &request.cancellation,
        )
    }

    fn nested(
        &self,
        request: &AgentInvocation,
        command: &str,
        arguments: Vec<String>,
        target: Option<CommandTarget>,
        confirmation: bool,
    ) -> Result<Value, CommandOutcome> {
        if !self.ready(&request.cancellation, request.deadline) {
            return Err(self.lifecycle_failure(&request.cancellation, request.deadline));
        }
        let mut invocation = nested_invocation(command, arguments, target);
        if confirmation {
            invocation.confirmation = Some(invocation.confirmation());
        }
        let outcome =
            self.commands
                .execute(invocation, request.deadline, request.cancellation.clone());
        match outcome {
            CommandOutcome::Success { value, .. } => Ok(value),
            outcome => Err(outcome),
        }
    }

    fn ready(&self, cancellation: &CommandCancellation, deadline: Instant) -> bool {
        self.is_active() && !cancellation.is_cancelled() && Instant::now() < deadline
    }

    fn begin_publication(&self) -> bool {
        let Ok(_gate) = self.publication.lock() else {
            return false;
        };
        self.is_active()
    }

    fn lifecycle_failure(
        &self,
        cancellation: &CommandCancellation,
        deadline: Instant,
    ) -> CommandOutcome {
        if !self.is_active() {
            failed(
                "stale_agent_incarnation",
                "agent service incarnation is no longer active",
            )
        } else if cancellation.is_cancelled() {
            CommandOutcome::cancelled()
        } else if Instant::now() >= deadline {
            CommandOutcome::deadline_exceeded()
        } else {
            failed("stale_agent_incarnation", "agent service is unavailable")
        }
    }
}

impl CommandCatalogSource for AgentService {
    fn list(&self) -> Vec<bootty_control::CommandDescriptor> {
        command_descriptors()
    }

    fn describe(&self, id: &str) -> Option<bootty_control::CommandDescriptor> {
        crate::commands::descriptors()
            .iter()
            .find(|command| command.id == id)
            .cloned()
    }

    fn topics(&self) -> BTreeSet<String> {
        if !self.is_active() {
            return BTreeSet::new();
        }
        AgentKind::ALL
            .into_iter()
            .map(|provider| provider.topic().to_owned())
            .collect()
    }

    fn with_active_topic(
        &self,
        module: &str,
        generation: u64,
        topic: &str,
        publish: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let Ok(_gate) = self.publication.lock() else {
            return Err("agent service publication gate is unavailable".to_owned());
        };
        if !self.is_active() {
            return Err("agent service incarnation is no longer active".to_owned());
        }
        if generation != self.generation {
            return Err("agent service incarnation is no longer active".to_owned());
        }
        if !AgentKind::ALL
            .iter()
            .any(|provider| provider.module() == module && provider.topic() == topic)
        {
            return Err(format!(
                "agent event topic `{topic}` is not registered by `{module}`"
            ));
        }
        publish();
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Start,
    Resume,
    Fork,
    Prompt,
    Steer,
    FollowUp,
    Abort,
    Interrupt,
    State,
    Stop,
    Ingest,
    Acknowledge,
}

fn parse_command(command: &str) -> Option<(AgentKind, Operation)> {
    let (provider, operation) = command.strip_prefix("agents.")?.split_once('.')?;
    let provider = match provider {
        "pi" => AgentKind::Pi,
        "codex" => AgentKind::Codex,
        "claude" => AgentKind::Claude,
        _ => return None,
    };
    let operation = match operation {
        "acknowledge" => Operation::Acknowledge,
        "start" => Operation::Start,
        "resume" => Operation::Resume,
        "fork" => Operation::Fork,
        "prompt" => Operation::Prompt,
        "steer" => Operation::Steer,
        "follow_up" => Operation::FollowUp,
        "abort" => Operation::Abort,
        "interrupt" => Operation::Interrupt,
        "state" => Operation::State,
        "stop" => Operation::Stop,
        "ingest" => Operation::Ingest,
        _ => return None,
    };
    Some((provider, operation))
}

fn provider_command_exists(command: &str) -> bool {
    crate::commands::descriptors()
        .iter()
        .any(|descriptor| descriptor.id == command)
}

fn validate_arguments(operation: Operation, arguments: &[String]) -> Option<String> {
    let maximum = match operation {
        Operation::Start | Operation::Ingest => 3,
        Operation::Resume | Operation::Fork => 4,
        Operation::Prompt
        | Operation::Steer
        | Operation::FollowUp
        | Operation::State
        | Operation::Acknowledge => 1,
        Operation::Abort | Operation::Interrupt | Operation::Stop => 0,
    };
    if arguments.len() > maximum {
        return Some(format!("command accepts at most {maximum} argument(s)"));
    }
    let required = matches!(
        operation,
        Operation::Prompt
            | Operation::Steer
            | Operation::FollowUp
            | Operation::Ingest
            | Operation::Acknowledge
    );
    if required && arguments.is_empty() {
        return Some("required argument is missing".to_owned());
    }
    None
}

fn apply_event(state: &mut AgentState, provider: AgentKind, event: &Value) {
    if let Some(cwd) =
        string(event, "cwd").filter(|cwd| cwd.len() <= 8192 && !cwd.chars().any(char::is_control))
    {
        state.cwd = Some(cwd.to_owned());
    }
    if let Some(value) = event.get("bootty_launch")
        && let Ok(launch) = serde_json::from_value::<crate::AgentLaunch>(value.clone())
        && launch.validate().is_ok()
    {
        state.launch = Some(launch.retained(provider));
    }

    match provider {
        AgentKind::Pi => apply_pi(state, event),
        AgentKind::Codex => apply_codex(state, event),
        AgentKind::Claude => apply_claude(state, event),
    }
}

fn apply_pi(state: &mut AgentState, event: &Value) {
    let event_name = string(event, "type")
        .or_else(|| string(event, "hook_event_name"))
        .unwrap_or("unknown");
    state.last_event = Some(event_name.to_owned());
    if let Some(file) = string(event, "sessionFile") {
        state.session_file = Some(file.to_owned());
    }
    state.session_id = string(event, "sessionId")
        .or_else(|| string(event, "session_id"))
        .map(str::to_owned)
        .or_else(|| state.session_id.clone());
    if let Some(session_name) = string(event, "sessionName") {
        state.session_name = Some(session_name.to_owned());
    }
    match event_name {
        "session_start" | "agent_settled" => state.status = AgentStatus::Idle,
        "agent_start" | "turn_start" => state.status = AgentStatus::Working,
        "tool_execution_start" => {
            state.status =
                AgentStatus::Tool(string(event, "toolName").unwrap_or("unknown").to_owned());
        }
        "extension_error" => {
            state.status = AgentStatus::Error;
            state.error = Some(
                string(event, "error")
                    .unwrap_or("Pi extension error")
                    .to_owned(),
            );
        }
        "session_shutdown" => state.status = AgentStatus::Stopped,
        _ => {}
    }
}

fn apply_codex(state: &mut AgentState, event: &Value) {
    let event_name = string(event, "hook_event_name").unwrap_or("unknown");
    state.last_event = Some(event_name.to_owned());
    state.thread_id = string(event, "session_id")
        .map(str::to_owned)
        .or_else(|| state.thread_id.clone());
    state.turn_id = string(event, "turn_id")
        .map(str::to_owned)
        .or_else(|| state.turn_id.clone());
    match event_name {
        "SessionStart" | "Stop" | "Interrupt" => state.status = AgentStatus::Idle,
        "UserPromptSubmit" | "PostToolUse" => state.status = AgentStatus::Working,
        "PermissionRequest" => state.status = AgentStatus::Waiting,
        "PreToolUse" => {
            state.status =
                AgentStatus::Tool(string(event, "tool_name").unwrap_or("unknown").to_owned());
        }
        "SessionEnd" => state.status = AgentStatus::Stopped,
        _ => {}
    }
}

fn apply_claude(state: &mut AgentState, event: &Value) {
    let event_name = string(event, "hook_event_name").unwrap_or("unknown");
    state.last_event = Some(event_name.to_owned());
    state.session_id = string(event, "session_id")
        .map(str::to_owned)
        .or_else(|| state.session_id.clone());
    match event_name {
        "SessionStart" | "Stop" => state.status = AgentStatus::Idle,
        "UserPromptSubmit" | "PostToolUse" => state.status = AgentStatus::Working,
        "PreToolUse" => {
            state.status =
                AgentStatus::Tool(string(event, "tool_name").unwrap_or("unknown").to_owned());
        }
        "Notification" | "PermissionRequest" => state.status = AgentStatus::Waiting,
        "SessionEnd" => state.status = AgentStatus::Stopped,
        _ => {}
    }
}

fn string<'a>(event: &'a Value, key: &str) -> Option<&'a str> {
    event
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 8192)
}

fn update_agent_state(state: &mut AgentState, payload: &Value, next_sequence: u64) {
    let previous_status = state.status.clone();
    let previous_error = state.error.clone();
    state.source = AgentSource::Existing;
    apply_event(state, state.provider, payload);
    if state.status != AgentStatus::Error {
        state.error = None;
    }
    let attention = match &state.status {
        AgentStatus::Idle
            if matches!(previous_status, AgentStatus::Working | AgentStatus::Tool(_)) =>
        {
            Some(crate::AgentAttention::Complete)
        }
        AgentStatus::Waiting if previous_status != AgentStatus::Waiting => {
            Some(crate::AgentAttention::Waiting)
        }
        AgentStatus::Error
            if previous_status != AgentStatus::Error || previous_error != state.error =>
        {
            Some(crate::AgentAttention::Error)
        }
        _ => None,
    };
    if let Some(attention) = attention {
        state.attention = Some(attention);
        state.attention_sequence = next_sequence;
    }
}
