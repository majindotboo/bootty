use bootty_control::CommandTarget;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AgentKind, AgentLaunch, NativeAgentRequest, NativeSessionSnapshot, NativeSessionStatus,
    native_protocol::{NativeTurnOutcome, bounded_text, field, string},
};

const FRAME_LIMIT: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
type PendingReplies = Arc<Mutex<BTreeMap<String, PendingReply>>>;
struct PendingReply {
    method: String,
    thread: Option<String>,
    waiting_for_input: bool,
    sender: mpsc::SyncSender<ReplyEvent>,
}
enum ReplyEvent {
    Reply(Result<Value, String>),
    InputWait { waiting: bool, at: Instant },
}
struct PendingRequest {
    id: String,
    receiver: mpsc::Receiver<ReplyEvent>,
}
struct ReplyWait {
    remaining: Duration,
    deadline: Instant,
    waiting: bool,
}
pub type NativeChangeHandler = Arc<dyn Fn() + Send + Sync>;
type ChangePublisher = Arc<Mutex<Option<NativeChangeHandler>>>;
type NativeWorkers = Arc<Mutex<Vec<NativeWorker>>>;

struct NativeWorker {
    handle: thread::JoinHandle<()>,
    completed: mpsc::Receiver<()>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRemote {
    pub host: bootty_config::config::RemoteConfig,
    pub daemon: String,
}

struct NativeLaunchResources {
    _permission_extension: Option<tempfile::NamedTempFile>,
    _remote_tools: Option<bootty_host::private_stdio::relay::RemoteStdioRelay>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeSessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<NativeRemote>,
    pub provider: AgentKind,
    pub program: String,
    pub cwd: PathBuf,
    pub arguments: Vec<String>,
    pub session_id: Option<String>,
    /// Persisted fresh Claude UUID intent; observed/resume identity stays separate.
    #[serde(default)]
    pub fresh_session_id: Option<String>,
    /// Exact provider-owned resume path, separate from Pi's persistent UUID.
    #[serde(default)]
    pub session_file: Option<String>,
    pub model: Option<String>,
    /// Persisted explicit reasoning selection for subsequent prompts.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default)]
    pub permissions: crate::NativePermissionMode,
    /// Captured Bootty profile ID; older records retain their account without inferring an alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Frozen absolute provider account store; credentials remain provider-owned.
    pub account_directory: Option<String>,
}

impl NativeSessionConfig {
    #[must_use]
    pub const fn supports_provider(provider: AgentKind) -> bool {
        matches!(
            provider,
            AgentKind::Codex | AgentKind::Pi | AgentKind::Claude
        )
    }

    #[must_use]
    pub fn new(provider: AgentKind, cwd: impl Into<PathBuf>) -> Self {
        let account_directory = std::env::var(provider.account_directory_variable())
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("HOME").ok().map(|home| {
                    format!(
                        "{home}/{}",
                        match provider {
                            AgentKind::Pi => ".pi/agent",
                            AgentKind::Claude => ".claude",
                            AgentKind::Codex => ".codex",
                        }
                    )
                })
            });
        Self {
            remote: None,
            provider,
            program: provider.default_program().to_owned(),
            cwd: cwd.into(),
            arguments: Vec::new(),
            session_id: None,
            fresh_session_id: None,
            session_file: None,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            permissions: crate::NativePermissionMode::default(),
            profile: None,
            account_directory,
        }
    }

    /// Capture a resolved launch once. Resume never re-reads the selected profile or account.
    /// # Errors
    /// Returns unsupported providers, missing/relative directories, or invalid launch options.
    pub fn from_launch(provider: AgentKind, launch: AgentLaunch) -> Result<Self, String> {
        launch.validate()?;
        if launch.ephemeral {
            return Err("Native conversations require provider session persistence".to_owned());
        }
        let retained = launch.retained(provider);
        let cwd = launch
            .cwd
            .ok_or("Native conversation requires a captured directory")?;
        let mut config = Self::new(provider, cwd);
        if retained.ephemeral {
            return Err("Native conversations require provider session persistence".to_owned());
        }
        config.program = retained.program;
        config.arguments = retained.arguments;
        if let Some(directory) = retained.account_directory {
            config.account_directory = Some(directory);
        }
        config.prepare_fresh_identity()?;
        config.validate()?;
        Ok(config)
    }

    /// Freeze the executable and account from the owning host before any provider launch.
    /// # Errors
    /// Returns remote discovery, directory, or provider configuration errors.
    pub fn capture_remote(
        provider: AgentKind,
        mut launch: AgentLaunch,
        host: bootty_config::config::RemoteConfig,
    ) -> Result<Self, String> {
        use bootty_host::CommandRunner as _;
        launch.validate()?;
        let remote = bootty_host::remote::RemoteHost::new(host.clone());
        let runner =
            bootty_host::remote::RemoteCommandRunner::new(remote, bootty_host::SystemCommandRunner);
        let suffix = match provider {
            AgentKind::Codex => ".codex",
            AgentKind::Claude => ".claude",
            AgentKind::Pi => ".pi/agent",
        };
        let cwd = launch
            .cwd
            .as_deref()
            .ok_or("Remote provider requires its captured directory")?;
        let output = runner.run_in(cwd, "/bin/sh", &[
            "-c".into(),
            r#"program=$(command -v "$1") || exit 127; home=$HOME; [ -n "$home" ] || exit 1; account=$3; if [ -z "$account" ]; then account=$(printenv "$2" || :); fi; if [ -z "$account" ]; then account=$home/$4; fi; printf '%s\n' "$program" "$account" "$home"; pwd -P"#.into(),
            "bootty-provider-profile".into(), launch.program.clone(), provider.account_directory_variable().into(),
            launch.account_directory.clone().unwrap_or_default(), suffix.into(),
        ]).map_err(|error| error.to_string())?;
        if !output.success {
            return Err(format!(
                "Remote {provider} executable or account is unavailable"
            ));
        }
        let values = output.stdout.lines().collect::<Vec<_>>();
        let [program, account, home, cwd] = values.as_slice() else {
            return Err("Remote provider profile returned invalid paths".into());
        };
        launch.program = (*program).into();
        launch.cwd = Some((*cwd).into());
        launch.account_directory = Some((*account).into());
        let mut config = Self::from_launch(provider, launch)?;
        config.remote = Some(NativeRemote {
            host,
            daemon: format!(
                "{home}/{}",
                bootty_host::remote_exec_program().trim_start_matches("./")
            ),
        });
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.validate_stored()?;
        if self.account_directory.is_none() {
            return Err(
                "Native conversation requires a captured absolute account directory".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn validate_stored(&self) -> Result<(), String> {
        if self.profile.as_ref().is_some_and(|profile| {
            profile.is_empty()
                || profile.len() > 64
                || !profile
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        }) {
            return Err("Captured profile requires a simple bounded ID".into());
        }
        if self.remote.as_ref().is_some_and(|remote| {
            !Path::new(&remote.daemon).is_absolute()
                || !Path::new(&self.program).is_absolute()
                || remote.daemon.len() > 8192
                || remote.daemon.chars().any(char::is_control)
        }) {
            return Err("Remote provider requires its captured absolute daemon path".into());
        }
        if !self.permissions.supports(self.provider) {
            return Err("This provider does not support the selected permission mode".into());
        }
        if !Self::supports_provider(self.provider) {
            return Err("Unsupported native conversation provider".to_owned());
        }
        AgentLaunch {
            program: self.program.clone(),
            cwd: Some(self.cwd.to_string_lossy().into_owned()),
            arguments: self.arguments.clone(),
            ephemeral: false,
            account_directory: self.account_directory.clone(),
        }
        .validate()?;
        if !self.cwd.is_absolute() {
            return Err("Native agent directory must be absolute".to_owned());
        }
        if self
            .account_directory
            .as_deref()
            .is_some_and(|directory| !std::path::Path::new(directory).is_absolute())
        {
            return Err(
                "Native conversation requires a captured absolute account directory".to_owned(),
            );
        }
        if self
            .session_id
            .iter()
            .chain(&self.fresh_session_id)
            .chain(&self.session_file)
            .chain(&self.model)
            .chain(&self.reasoning_effort)
            .any(|value| {
                value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control)
            })
        {
            return Err("Invalid native agent session or model selector".to_owned());
        }
        if self.provider == AgentKind::Claude {
            crate::native_claude::validate_configuration(self)?;
        }
        if self.provider == AgentKind::Pi {
            if self
                .session_file
                .as_ref()
                .is_some_and(|file| !std::path::Path::new(file).is_absolute())
            {
                return Err("Pi resume requires its exact absolute session file".to_owned());
            }
            if self.session_id.is_some() != self.session_file.is_some() {
                return Err(
                    "Pi resume requires the captured UUID and absolute session file together"
                        .to_owned(),
                );
            }
            crate::PiAccountSelector::from_arguments(&self.arguments)?;
            let retained = AgentLaunch {
                program: self.program.clone(),
                cwd: None,
                arguments: self.arguments.clone(),
                ephemeral: false,
                account_directory: None,
            }
            .retained(AgentKind::Pi);
            if retained.arguments != self.arguments || retained.ephemeral {
                return Err("Native Pi requires reusable configuration options; RPC mode and session identity are host-owned".to_owned());
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_fresh_identity(&mut self) -> Result<(), String> {
        if self.provider == AgentKind::Claude
            && self.session_id.is_none()
            && self.fresh_session_id.is_none()
        {
            self.fresh_session_id = Some(crate::terminal_session_id()?);
        }
        Ok(())
    }

    pub(crate) fn capture_session(&mut self, snapshot: &NativeSessionSnapshot) {
        // Readiness can precede the provider's identity event; absence cannot erase a resume selector.
        if snapshot.session_id.is_some() {
            self.session_id.clone_from(&snapshot.session_id);
        }
        if snapshot.session_file.is_some() {
            self.session_file.clone_from(&snapshot.session_file);
        }
    }
}

/// A bidirectional provider child. Spawn and requests run on a host worker; snapshots never wait
/// for provider I/O. Dropping this owner terminates and reaps its process tree.
pub struct NativeAgentSession {
    pub(crate) config: NativeSessionConfig,
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pub(crate) snapshot: Arc<Mutex<NativeSessionSnapshot>>,
    pending: PendingReplies,
    next_request: Arc<AtomicU64>,
    cancel_requested: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    pub(crate) change_handler: ChangePublisher,
    workers: NativeWorkers,
    pub(crate) clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    tools: Option<Arc<crate::ToolBridge>>,
    claude: Option<Arc<Mutex<crate::native_claude::ClaudeProtocol>>>,
    pub(crate) completion_catalog: Mutex<Option<crate::NativeCompletionCatalog>>,
    pub(crate) claude_models: Mutex<Option<Vec<crate::NativeModelOption>>>,
    pub(crate) history_window: Mutex<Option<crate::native_history::HistoryPosition>>,
    pub(crate) history_quotes: Mutex<NativeSessionSnapshot>,
    _launch_resources: NativeLaunchResources,
}

impl NativeAgentSession {
    /// # Errors
    /// Returns provider launch, handshake, resume or protocol errors after reaping the child.
    pub fn spawn(config: NativeSessionConfig) -> Result<Self, String> {
        let session = Self::start_with_clock(config, Arc::new(Instant::now))?;
        session.initialize()?;
        Ok(session)
    }

    pub(crate) fn start_with_clock(
        config: NativeSessionConfig,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Result<Self, String> {
        Self::start_with_tools(config, clock, None, None)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One constructor owns provider pipes, workers, and their cleanup"
    )]
    pub(crate) fn start_with_tools(
        config: NativeSessionConfig,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
        tools: Option<Arc<crate::ToolBridge>>,
        attachment_directory: Option<&Path>,
    ) -> Result<Self, String> {
        let claude = claude_protocol(&config)?;
        let (mut child, launch_resources) =
            spawn_native_child(&config, tools.as_deref(), attachment_directory)?;
        let stdin = child.stdin.take().ok_or("Agent stdin was not opened")?;
        let stdout = child.stdout.take().ok_or("Agent stdout was not opened")?;
        let stderr = child.stderr.take().ok_or("Agent stderr was not opened")?;
        let child = Arc::new(Mutex::new(child));
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let next_request = Arc::new(AtomicU64::new(1));
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let snapshot = Arc::new(Mutex::new(initial_snapshot(
            config.provider,
            tools.as_deref(),
        )));
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let closing = Arc::new(AtomicBool::new(false));
        let change_handler = Arc::new(Mutex::new(None));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let tail = Arc::new(Mutex::new(Vec::new()));
        lock(&workers).push(start_stderr_worker(stderr, Arc::clone(&tail), &child)?);
        let reader_snapshot = Arc::clone(&snapshot);
        let reader_pending = Arc::clone(&pending);
        let reader_child = Arc::clone(&child);
        let reader_closing = Arc::clone(&closing);
        let reader_handler = Arc::clone(&change_handler);
        let reader_input = Arc::clone(&stdin);
        let reader_next = Arc::clone(&next_request);
        let reader_cancel = Arc::clone(&cancel_requested);
        let reader_workers = Arc::clone(&workers);
        let (reader_tools, reader_claude) = (tools.clone(), claude.clone());
        let reader_remote = config.remote.is_some();
        let reader_worker = spawn_worker("native-agent-protocol", move || {
            let fault = read_frames(
                BufReader::new(stdout),
                &reader_snapshot,
                &reader_pending,
                &reader_handler,
                &reader_input,
                &reader_next,
                &reader_closing,
                &reader_cancel,
                &reader_workers,
                reader_claude.as_deref(),
            )
            .err();
            if let Some(tools) = &reader_tools {
                tools.stop();
            }
            // EOF ends the protocol lease even when a misbehaving child keeps running.
            if !reader_closing.load(Ordering::Acquire) {
                terminate(&reader_child);
            }
            let status = lock(&reader_child).wait();
            let mut snapshot = lock(&reader_snapshot);
            let intentional = reader_closing.load(Ordering::Acquire);
            // Only an established conversation can reconnect. Startup failures remain
            // visible instead of retrying an executable/configuration error forever.
            snapshot.transport_lost =
                reader_remote && !intentional && fault.is_none() && snapshot.session_id.is_some();
            let error = fault.unwrap_or_else(|| {
                format!(
                    "Agent process ended ({}): {}",
                    status.map_or_else(|error| error.to_string(), |status| status.to_string()),
                    String::from_utf8_lossy(&lock(&tail))
                )
            });
            if !intentional {
                snapshot.error = Some(bounded_text(error.clone()));
            }
            snapshot.status = if intentional {
                NativeSessionStatus::Stopped
            } else {
                NativeSessionStatus::Error
            };
            snapshot.working_since = None;
            snapshot.changed();
            drop(snapshot);
            publish_change(&reader_handler);
            let replies = std::mem::take(&mut *lock(&reader_pending));
            for (_, reply) in replies {
                let _ = reply.sender.send(ReplyEvent::Reply(Err(error.clone())));
            }
        })
        .inspect_err(|_| {
            terminate(&child);
            let _ = join_workers(&workers);
        })?;
        lock(&workers).push(reader_worker);
        Ok(Self {
            child,
            stdin,
            snapshot,
            pending,
            next_request,
            cancel_requested,
            closing,
            change_handler,
            workers,
            clock,
            tools,
            claude,
            completion_catalog: Mutex::new(None),
            claude_models: Mutex::new(None),
            history_window: Mutex::new(None),
            history_quotes: Mutex::new(NativeSessionSnapshot::new(config.provider)),
            _launch_resources: launch_resources,
            config,
        })
    }

    pub(crate) fn initialize(&self) -> Result<(), String> {
        self.initialize_with_empty_restore(false)
    }

    pub(crate) fn initialize_with_empty_restore(
        &self,
        can_recreate_empty_thread: bool,
    ) -> Result<(), String> {
        if self.config.provider == AgentKind::Pi {
            return self.initialize_pi();
        }
        if self.config.provider == AgentKind::Claude {
            let response = self.rpc("initialize", json!({}))?;
            if response.get("models").is_some() {
                let mut models = crate::native_models::decode_claude(&response)?;
                if let Ok(settings) = self.rpc("get_settings", json!({})) {
                    crate::native_models::apply_claude_effort(&mut models, &response, &settings);
                }
                *lock(&self.claude_models) = Some(models);
            }
            // Older transports omit optional commands; conversation initialization still works.
            if response.get("commands").is_some() {
                *lock(&self.completion_catalog) =
                    Some(crate::native_completions::decode_claude(&response)?);
            }
            let mut snapshot = lock(&self.snapshot);
            // The control handshake confirms readiness, not the lazily created session identity.
            if snapshot.status == NativeSessionStatus::Starting {
                snapshot.status = NativeSessionStatus::Idle;
                snapshot.changed();
            }
            drop(snapshot);
            return Ok(());
        }
        self.initialize_codex_transport()?;
        let method = if self.config.session_id.is_some() {
            "thread/resume"
        } else {
            "thread/start"
        };
        let params = self.config.session_id.as_ref().map_or_else(
            || json!({"cwd":self.config.cwd,"model":self.config.model}),
            |id| json!({"cwd":self.config.cwd,"model":self.config.model,"threadId":id,"excludeTurns":true}),
        );
        let (result, recreated) = match self.rpc(method, params) {
            Ok(result) => (result, false),
            Err(error)
                if can_recreate_empty_thread
                    && self.config.session_id.as_ref().is_some_and(|id| {
                        serde_json::from_str::<Value>(&error).is_ok_and(|error| {
                            field(&error, "code") == -32600
                                && field(&error, "message").as_str()
                                    == Some(format!("no rollout found for thread id {id}").as_str())
                        })
                    }) =>
            {
                // Codex does not save an empty thread until its first accepted turn.
                // Recreate only an empty reservation; observed provider history is never discarded.
                (
                    self.rpc(
                        "thread/start",
                        json!({"cwd":self.config.cwd,"model":self.config.model}),
                    )?,
                    true,
                )
            }
            Err(error) => return Err(error),
        };
        let id = string(field(field(&result, "thread"), "id"))
            .filter(|id| !id.is_empty() && id.len() <= 8192)
            .ok_or("Provider returned no valid thread identity")?;
        if !recreated
            && self
                .config
                .session_id
                .as_ref()
                .is_some_and(|expected| expected != &id)
        {
            return Err(
                "Provider resumed a different thread; captured identity was preserved".to_owned(),
            );
        }
        let mut snapshot = lock(&self.snapshot);
        snapshot.session_id = Some(id);
        if let Some(turns) = field(field(&result, "thread"), "turns").as_array() {
            for turn in turns {
                snapshot.history_turn(turn);
            }
        }
        snapshot.status = NativeSessionStatus::Idle;
        snapshot.working_since = None;
        snapshot.changed();
        drop(snapshot);
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> NativeSessionSnapshot {
        let mut snapshot = lock(&self.snapshot).clone();
        snapshot.browser_access = self.browser_access();
        snapshot
    }

    pub(crate) fn browser_access(&self) -> crate::NativeBrowserAccess {
        self.tools
            .as_ref()
            .map_or(crate::NativeBrowserAccess::Unavailable, |tools| {
                tools.lease().browser_access()
            })
    }

    pub(crate) fn initialize_codex_transport(&self) -> Result<(), String> {
        self.rpc("initialize", json!({"clientInfo":{"name":"bootty","title":"Bootty","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        self.write(&json!({"method":"initialized"}))
    }

    pub(crate) fn restore_recent_history(&self, history: &NativeSessionSnapshot) {
        let mut snapshot = lock(&self.snapshot);
        snapshot.completed_turn = history.completed_turn;
        snapshot.restore_image_references(&history.transcript);
        // Pi's successful get_messages is authoritative, including an empty history.
        if snapshot.transcript.is_empty() && self.config.provider != AgentKind::Pi {
            snapshot.transcript.clone_from(&history.transcript);
            snapshot.usage.clone_from(&history.usage);
            snapshot.changed();
        }
    }

    pub(crate) fn restore_fork_history(
        &self,
        history: &NativeSessionSnapshot,
        fork: &crate::NativeSideChat,
    ) {
        let mut snapshot = lock(&self.snapshot);
        snapshot.restore_image_references(&history.transcript);
        let prefix = fork
            .copied_transcript(&history.transcript)
            .unwrap_or_default();
        snapshot.restore_image_references(prefix);
        let mut provider = std::mem::take(&mut snapshot.transcript);
        if provider.is_empty() {
            provider.clone_from(&history.transcript);
        }
        snapshot.transcript = prefix.to_vec();
        snapshot.transcript.extend(
            provider
                .into_iter()
                .filter(|item| !prefix.iter().any(|old| old.id == item.id)),
        );
        snapshot.changed();
    }

    pub fn set_change_handler(&self, handler: NativeChangeHandler) {
        *lock(&self.change_handler) = Some(handler);
    }

    #[must_use]
    pub const fn config(&self) -> &NativeSessionConfig {
        &self.config
    }

    /// # Errors
    /// Returns validation, transport, timeout or first-hand provider rejection errors.
    pub fn send_prompt(&self, message: &str) -> Result<(), String> {
        self.send_prompt_input(&crate::NativePrompt::text(message)?)
    }

    /// Submit validated host/store images without a caller path or URL transport.
    /// # Errors
    /// Returns validation, transport, timeout or first-hand provider rejection errors.
    pub fn send_prompt_input(&self, prompt: &crate::NativePrompt) -> Result<(), String> {
        let selection = self
            .config
            .model
            .as_ref()
            .map(|model| crate::NativeModelSelection {
                model: model.clone(),
                reasoning_effort: self.config.reasoning_effort.clone(),
            });
        self.send_prompt_input_with_selection(prompt, selection.as_ref())
    }

    pub(crate) fn send_prompt_input_with_selection(
        &self,
        prompt: &crate::NativePrompt,
        selection: Option<&crate::NativeModelSelection>,
    ) -> Result<(), String> {
        if self.closing.load(Ordering::Acquire) {
            return Err("Native agent session is stopped".to_owned());
        }
        let resolved_prompt =
            if self.config.provider != AgentKind::Claude && prompt.needs_skill_catalog() {
                prompt.with_advertised_skills(&self.completions()?)
            } else {
                prompt.clone()
            };
        let prompt = &resolved_prompt;
        if self.config.provider == AgentKind::Codex
            && let Some(instructions) = prompt.compact_instructions()?
        {
            if !instructions.is_empty() {
                return Err("Codex compaction does not accept additional instructions".into());
            }
            return self.compact_codex();
        }
        if self.snapshot().status == NativeSessionStatus::Working {
            return self.send_active_prompt(prompt);
        }
        if self.config.provider == AgentKind::Pi {
            if let Some(selection) = selection {
                if self.snapshot().status != NativeSessionStatus::Idle {
                    return Err("Wait for the current Pi run before changing its model".to_owned());
                }
                self.apply_pi_selection(selection)?;
            }
            return self.send_prompt_pi(prompt);
        }
        if self.config.provider == AgentKind::Claude {
            if let Some(selection) = selection {
                self.apply_claude_selection(selection)?;
            }
            return self.send_prompt_claude(prompt);
        }
        self.send_prompt_codex(prompt, selection)
    }

    fn compact_codex(&self) -> Result<(), String> {
        let id = {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status != NativeSessionStatus::Idle {
                return Err("Wait for the current Codex run before compacting".into());
            }
            let id = snapshot
                .session_id
                .clone()
                .ok_or("Agent has no native thread")?;
            snapshot.status = NativeSessionStatus::Working;
            snapshot.working_since = Some((self.clock)());
            snapshot.completed_turn = false;
            snapshot.changed();
            id
        };
        publish_change(&self.change_handler);
        // The acknowledgement only starts compaction. Provider turn events own completion.
        let result = self.rpc("thread/compact/start", json!({"threadId":id}));
        if let Err(error) = &result {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status != NativeSessionStatus::Stopped {
                snapshot.status = NativeSessionStatus::Error;
                snapshot.working_since = None;
                snapshot.error = Some(bounded_text(error.clone()));
            }
            snapshot.changed();
            drop(snapshot);
            publish_change(&self.change_handler);
        }
        result.map(|_| ())
    }

    fn send_prompt_codex(
        &self,
        prompt: &crate::NativePrompt,
        selection: Option<&crate::NativeModelSelection>,
    ) -> Result<(), String> {
        let first_prompt = {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status != NativeSessionStatus::Idle {
                return Err(
                    "Agent is not idle; interrupt, resume, or wait for its current turn".to_owned(),
                );
            }
            let first_prompt = snapshot.first_turn.is_none();
            let id = format!("local-user-{}", snapshot.revision);
            snapshot.pending_user_item = Some(id.clone());
            let now = Some(chrono::Utc::now().timestamp_millis());
            snapshot
                .with_message_times((now, now), |snapshot| snapshot.prompt_message(&id, prompt))?;
            snapshot.status = NativeSessionStatus::Working;
            snapshot.working_since = Some((self.clock)());
            snapshot.completed_turn = false;
            snapshot.changed();
            first_prompt
        };
        publish_change(&self.change_handler);
        let id = self
            .snapshot()
            .session_id
            .ok_or("Agent has no native thread")?;
        let mut parameters = json!({"threadId":id,"input":prompt.codex_input()?,"model":selection.map(|selection| &selection.model),"effort":selection.and_then(|selection| selection.reasoning_effort.as_ref())});
        if self.config.fast_mode
            && let Some(parameters) = parameters.as_object_mut()
        {
            parameters.insert("serviceTier".to_owned(), json!("fast"));
        }
        if let (Some(parameters), Some(policy)) = (
            parameters.as_object_mut(),
            self.config.permissions.codex().as_object(),
        ) {
            parameters.extend(policy.clone());
        }
        let result = self.rpc_prompt("turn/start", parameters, prompt.requires_large_envelope());
        let result = result.and_then(|reply| {
            let turn = string(field(field(&reply, "turn"), "id"))
                .filter(|id| !id.is_empty() && id.len() <= 8192)
                .ok_or("Provider returned no valid turn identity")?;
            let mut snapshot = lock(&self.snapshot);
            if snapshot
                .turn_id
                .as_ref()
                .is_some_and(|active| active != &turn)
            {
                return Err(
                    "Provider turn acknowledgement does not match the active turn".to_owned(),
                );
            }
            if first_prompt
                && snapshot
                    .first_turn
                    .as_ref()
                    .is_some_and(|receipt| receipt.id != turn)
            {
                return Err(
                    "First provider turn acknowledgement does not match its accepted events"
                        .to_owned(),
                );
            }
            snapshot.accept_first_turn(&turn);
            // Notifications may already have completed this turn before its RPC reply arrives.
            if snapshot.status == NativeSessionStatus::Working && snapshot.turn_id.is_none() {
                snapshot.turn_id = Some(turn);
            }
            drop(snapshot);
            Ok(())
        });
        if result.is_ok()
            && self.snapshot().turn_id.is_some()
            && self.cancel_requested.swap(false, Ordering::AcqRel)
        {
            self.interrupt()?;
        }
        let mut snapshot = lock(&self.snapshot);
        // Stop wakes pending RPCs with an error; that reply cannot undo the accepted stop.
        if let Err(error) = &result
            && snapshot.status != NativeSessionStatus::Stopped
        {
            snapshot.status = NativeSessionStatus::Error;
            snapshot.working_since = None;
            snapshot.error = Some(bounded_text(error.clone()));
        }
        if let Some(item) = snapshot
            .transcript
            .iter_mut()
            .rev()
            .find(|item| item.role == "user" && !item.complete)
        {
            item.complete = result.is_ok();
        }
        snapshot.changed();
        drop(snapshot);
        publish_change(&self.change_handler);
        result
    }

    fn send_prompt_claude(&self, prompt: &crate::NativePrompt) -> Result<(), String> {
        let id = crate::terminal_session_id()?;
        let claude = self
            .claude
            .as_ref()
            .ok_or("Claude protocol owner is absent")?;
        let value = {
            let mut protocol = lock(claude);
            let mut snapshot = lock(&self.snapshot);
            let now = Some(chrono::Utc::now().timestamp_millis());
            let value = snapshot.with_message_times((now, now), |snapshot| {
                protocol.prompt(snapshot, &id, prompt)
            })?;
            drop(protocol);
            snapshot.working_since = Some((self.clock)());
            drop(snapshot);
            value
        };
        publish_change(&self.change_handler);
        if let Err(error) = write_frame_with_limit(
            &self.stdin,
            &self.closing,
            &value,
            if prompt.requires_large_envelope() {
                crate::MAX_NATIVE_IMAGE_ENVELOPE_BYTES
            } else {
                FRAME_LIMIT
            },
        ) {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status != NativeSessionStatus::Stopped {
                snapshot.status = NativeSessionStatus::Error;
                snapshot.error = Some(bounded_text(error.clone()));
                snapshot.working_since = None;
            }
            snapshot.changed();
            drop(snapshot);
            publish_change(&self.change_handler);
            return Err(error);
        }
        Ok(())
    }

    // Steer the exact active turn; never interrupt/restart on an ambiguous rejection.
    fn send_active_prompt(&self, prompt: &crate::NativePrompt) -> Result<(), String> {
        let (method, parameters) = match self.config.provider {
            AgentKind::Codex => ("turn/steer", json!({"input":prompt.codex_input()?})),
            AgentKind::Pi => {
                self.verify_pi_identity()?;
                ("steer", prompt.pi_parameters()?)
            }
            AgentKind::Claude => return Err("This provider cannot steer an active turn".to_owned()),
        };
        let mut parameters = parameters
            .as_object()
            .cloned()
            .ok_or("Invalid active prompt parameters")?;
        let (turn, local_id) = {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status != NativeSessionStatus::Working
                || snapshot.pending_user_item.is_some()
            {
                return Err("Wait for the previous message to reach the active turn".to_owned());
            }
            let turn = snapshot
                .turn_id
                .clone()
                .ok_or("The active turn changed before sending")?;
            if self.config.provider == AgentKind::Codex {
                let thread = snapshot
                    .session_id
                    .clone()
                    .ok_or("Agent has no native thread")?;
                parameters.insert("threadId".to_owned(), thread.into());
                parameters.insert("expectedTurnId".to_owned(), turn.clone().into());
            }
            let id = format!("local-user-{}", snapshot.revision);
            snapshot.pending_user_item = Some(id.clone());
            let now = Some(chrono::Utc::now().timestamp_millis());
            snapshot
                .with_message_times((now, now), |snapshot| snapshot.prompt_message(&id, prompt))?;
            snapshot.changed();
            drop(snapshot);
            (turn, id)
        };
        publish_change(&self.change_handler);
        let outcome = self
            .rpc_prompt(
                method,
                Value::Object(parameters),
                prompt.requires_large_envelope(),
            )
            .and_then(|reply| {
                if self.config.provider == AgentKind::Codex {
                    let acknowledged = field(&reply, "turnId")
                        .as_str()
                        .or_else(|| field(field(&reply, "turn"), "id").as_str());
                    if acknowledged != Some(turn.as_str()) {
                        return Err("Provider acknowledged a different active turn".to_owned());
                    }
                } else if !matches!(
                    field(&reply, "disposition").as_str(),
                    Some("queued" | "started" | "handled")
                ) {
                    return Err("Pi returned no recognized steer disposition".to_owned());
                }
                Ok(())
            });
        let mut snapshot = lock(&self.snapshot);
        if outcome.is_err() && snapshot.pending_user_item.as_ref() == Some(&local_id) {
            snapshot.transcript.retain(|item| item.id != local_id);
            snapshot.pending_user_item = None;
            snapshot.image_echo_count = 0;
        }
        // A rejected steer does not end the agent's existing work or alter its first-turn receipt.
        snapshot.changed();
        drop(snapshot);
        publish_change(&self.change_handler);
        outcome
    }

    /// # Errors
    /// Returns provider or transport errors; interrupt preserves the resumable session.
    pub fn interrupt(&self) -> Result<(), String> {
        if self.config.provider == AgentKind::Pi {
            return self.interrupt_pi();
        }
        if self.config.provider == AgentKind::Claude {
            if !matches!(
                self.snapshot().status,
                NativeSessionStatus::Working | NativeSessionStatus::Waiting
            ) {
                return Err("No active Claude turn".to_owned());
            }
            // Acknowledgement confirms delivery. Only the accepted turn's actual result settles it.
            self.rpc("interrupt", json!({}))?;
            return Ok(());
        }
        let snapshot = lock(&self.snapshot);
        if snapshot.turn_id.is_none() && snapshot.status == NativeSessionStatus::Working {
            // turn/start may still await its reply. Queue this turn's cancellation independently.
            self.cancel_requested.store(true, Ordering::Release);
            return Ok(());
        }
        let turn = snapshot.turn_id.clone().ok_or("No active agent turn")?;
        let params = json!({"threadId":snapshot.session_id,"turnId":turn});
        drop(snapshot);
        self.rpc("turn/interrupt", params)?;
        let mut snapshot = lock(&self.snapshot);
        snapshot.finish_first_turn(&turn, NativeTurnOutcome::Interrupted);
        snapshot.changed();
        drop(snapshot);
        publish_change(&self.change_handler);
        Ok(())
    }

    /// Reply only to a first-hand, currently pending provider request.
    /// # Errors
    /// Returns an error for unknown requests, invalid response types, or transport failure.
    pub fn respond(&self, id: &str, response: Value) -> Result<(), String> {
        let mut snapshot = lock(&self.snapshot);
        let request = snapshot
            .requests
            .iter()
            .find(|request| request.id == id)
            .cloned()
            .ok_or("Agent request is no longer pending")?;
        let value = if self.config.provider == AgentKind::Pi {
            crate::native_pi::ui_response(&request, &response)?
        } else if self.config.provider == AgentKind::Claude {
            crate::native_claude::permission_response(&request, &response)?
        } else {
            match request.method.as_str() {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    if !crate::NativeApprovalDecision::ALL
                        .into_iter()
                        .any(|decision| {
                            request.approval_response(decision).as_ref() == Some(&response)
                        })
                    {
                        return Err(
                            "Approval decision is not supported by this captured request"
                                .to_owned(),
                        );
                    }
                }
                "item/tool/requestUserInput" => validate_answers(&request.parameters, &response)?,
                "mcpServer/elicitation/request" => {
                    crate::native_elicitation::validate_response(&request.parameters, &response)?;
                }
                _ => return Err("Unsupported native provider request".to_owned()),
            }
            json!({"id":request.wire_id,"result":response})
        };
        if response.to_string().len() > 16 * 1024 {
            return Err("Provider response exceeds 16 KiB".to_owned());
        }
        drop(response);
        self.write(&value)?;
        snapshot.requests.retain(|item| item.id != request.id);
        if snapshot.requests.is_empty() {
            snapshot.status = if request.method == "mcpServer/elicitation/request"
                && snapshot.turn_id.is_none()
                && snapshot.pending_user_item.is_none()
            {
                NativeSessionStatus::Idle
            } else {
                NativeSessionStatus::Working
            };
        }
        snapshot.changed();
        drop(snapshot);
        self.publish_input_wait();
        publish_change(&self.change_handler);
        Ok(())
    }

    /// # Errors
    /// Returns an error if the request is not a command/file/tool permission request.
    pub fn approve(&self, id: &str, allow: bool) -> Result<(), String> {
        self.approve_decision(
            id,
            if allow {
                crate::NativeApprovalDecision::AllowOnce
            } else {
                crate::NativeApprovalDecision::Deny
            },
        )
    }

    /// # Errors
    /// Rejects unavailable reusable choices or a request that is no longer pending.
    pub fn approve_decision(
        &self,
        id: &str,
        decision: crate::NativeApprovalDecision,
    ) -> Result<(), String> {
        let request = self
            .snapshot()
            .requests
            .into_iter()
            .find(|request| request.id == id)
            .ok_or("Agent request is no longer pending")?;
        let allow = decision == crate::NativeApprovalDecision::AllowOnce;
        if !matches!(
            decision,
            crate::NativeApprovalDecision::AllowOnce | crate::NativeApprovalDecision::Deny
        ) {
            let response = request
                .approval_response(decision)
                .ok_or("This request does not support that approval scope")?;
            return self.respond(id, response);
        }
        if self.config.provider == AgentKind::Pi && request.method == "pi.confirm" {
            return self.respond(id, json!({"confirmed":allow}));
        }
        if request.method == "mcpServer/elicitation/request" {
            return self.respond(
                id,
                json!({"action":if allow {"accept"} else {"decline"}, "content":if allow {json!({})} else {Value::Null}}),
            );
        }
        if !matches!(
            request.method.as_str(),
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "claude.permission"
        ) {
            return Err("This request requires a structured answer".to_owned());
        }
        let response = json!({"decision":if allow {"accept"} else {"decline"}});
        self.respond(id, response)
    }

    /// Read native provider history. Call from a worker; the provider owns durable storage.
    /// # Errors
    /// Returns provider protocol errors or unsupported history operations.
    pub fn history(&self) -> Result<Value, String> {
        if self.config.provider == AgentKind::Pi {
            return self.rpc("get_messages", json!({}));
        }
        if self.config.provider == AgentKind::Claude {
            return Ok(json!({"recent_only":true,"snapshot":self.snapshot()}));
        }
        self.rpc(
            "thread/read",
            json!({"threadId":self.snapshot().session_id,"includeTurns":true}),
        )
    }

    /// # Errors
    /// Returns provider history errors or a mismatched thread identity.
    pub fn refresh_history(&self) -> Result<NativeSessionSnapshot, String> {
        if self.config.provider == AgentKind::Pi {
            return self.refresh_history_pi();
        }
        if self.config.provider == AgentKind::Claude {
            // The CLI has no live history RPC. The catalog owns the bounded observed history;
            // complete imported history requires the provider's supported session-reader API.
            return Ok(self.snapshot());
        }
        let history = self.history()?;
        let mut snapshot = lock(&self.snapshot);
        if field(field(&history, "thread"), "id").as_str() != snapshot.session_id.as_deref() {
            return Err("Provider history belongs to a different thread".to_owned());
        }
        if snapshot.status != NativeSessionStatus::Idle {
            return Err("Wait for the current turn before refreshing history".to_owned());
        }
        let previous = std::mem::take(&mut snapshot.transcript);
        for turn in field(field(&history, "thread"), "turns")
            .as_array()
            .into_iter()
            .flatten()
        {
            snapshot.history_turn(turn);
        }
        snapshot.restore_image_references(&previous);
        snapshot.changed();
        let result = snapshot.clone();
        drop(snapshot);
        publish_change(&self.change_handler);
        Ok(result)
    }

    /// Execute a provider request on a worker. Native account/history adapters use the same
    /// initialized transport instead of starting an unowned parallel provider process.
    /// # Errors
    /// Returns transport errors, provider rejection or timeout (delivery may be ambiguous).
    pub fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        send_rpc(
            &self.stdin,
            &self.pending,
            &self.next_request,
            &self.closing,
            (self.config.provider, method, params, FRAME_LIMIT),
        )
    }

    pub(crate) fn rpc_prompt(
        &self,
        method: &str,
        params: Value,
        large_payload: bool,
    ) -> Result<Value, String> {
        send_rpc(
            &self.stdin,
            &self.pending,
            &self.next_request,
            &self.closing,
            (
                self.config.provider,
                method,
                params,
                if large_payload {
                    crate::MAX_NATIVE_IMAGE_ENVELOPE_BYTES
                } else {
                    FRAME_LIMIT
                },
            ),
        )
    }

    /// Pi extension commands can request user input before returning their prompt disposition.
    /// Return that observed phase immediately; the owned worker still validates the real reply.
    pub(crate) fn rpc_prompt_pi(
        &self,
        params: Value,
        large_payload: bool,
        dispatch: &str,
    ) -> Result<Option<Value>, String> {
        let request = begin_rpc(
            &self.stdin,
            &self.pending,
            &self.next_request,
            &self.closing,
            (
                AgentKind::Pi,
                "prompt",
                params,
                if large_payload {
                    crate::MAX_NATIVE_IMAGE_ENVELOPE_BYTES
                } else {
                    FRAME_LIMIT
                },
            ),
        )?;
        let mut wait = ReplyWait::new()?;
        loop {
            match wait.receive(&request.receiver, "prompt") {
                Ok(ReplyEvent::InputWait { waiting: true, .. }) => {
                    let pending = Arc::clone(&self.pending);
                    let snapshot = Arc::clone(&self.snapshot);
                    let handler = Arc::clone(&self.change_handler);
                    let closing = Arc::clone(&self.closing);
                    let dispatch = dispatch.to_owned();
                    let id = request.id.clone();
                    let worker = spawn_worker("native-pi-prompt-reply", move || {
                        let result = wait_for_reply(&request.receiver, "prompt", wait);
                        lock(&pending).remove(&request.id);
                        if !closing.load(Ordering::Acquire) {
                            _ = crate::native_pi::accept_prompt_reply(
                                &mut lock(&snapshot),
                                &dispatch,
                                result,
                            );
                            publish_change(&handler);
                        }
                    });
                    match worker {
                        Ok(worker) => lock(&self.workers).push(worker),
                        Err(error) => {
                            lock(&self.pending).remove(&id);
                            return Err(error);
                        }
                    }
                    return Ok(None);
                }
                Ok(ReplyEvent::InputWait { .. }) => {}
                Ok(ReplyEvent::Reply(result)) => {
                    lock(&self.pending).remove(&request.id);
                    return result.map(Some);
                }
                Err(error) => {
                    lock(&self.pending).remove(&request.id);
                    return Err(error);
                }
            }
        }
    }

    pub(crate) fn write(&self, value: &Value) -> Result<(), String> {
        write_frame(&self.stdin, &self.closing, value)
    }

    pub(crate) fn publish_input_wait(&self) {
        let waiting = !lock(&self.snapshot).requests.is_empty();
        update_input_wait(&self.pending, waiting);
    }

    pub fn stop(&self) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(tools) = &self.tools {
            tools.stop();
        }
        terminate(&self.child);
        lock(&self.stdin).take();
        let mut snapshot = lock(&self.snapshot);
        snapshot.status = NativeSessionStatus::Stopped;
        snapshot.transport_lost = false;
        snapshot.working_since = None;
        snapshot.requests.clear();
        snapshot.turn_id = None;
        snapshot.changed();
        drop(snapshot);
        let replies = std::mem::take(&mut *lock(&self.pending));
        for (_, reply) in replies {
            let _ = reply.sender.send(ReplyEvent::Reply(Err(
                "Native agent session was stopped".to_owned()
            )));
        }
        if let Err(error) = join_workers(&self.workers) {
            let mut snapshot = lock(&self.snapshot);
            snapshot.error = Some(error);
            snapshot.changed();
            drop(snapshot);
        }
        publish_change(&self.change_handler);
    }
}

impl Drop for NativeAgentSession {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn_worker(name: &str, task: impl FnOnce() + Send + 'static) -> Result<NativeWorker, String> {
    let (done, completed) = mpsc::sync_channel(1);
    let handle = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            task();
            let _ = done.send(());
        })
        .map_err(|error| error.to_string())?;
    Ok(NativeWorker { handle, completed })
}

fn join_workers(workers: &Mutex<Vec<NativeWorker>>) -> Result<(), String> {
    // Escaped descendants retaining provider pipes are unsupported; bound teardown to five seconds.
    // Supporting them requires a provider-owned shutdown/EOF contract, not an unbounded UI wait.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Invalid native shutdown deadline")?;
    let mut complete = true;
    loop {
        let retiring = std::mem::take(&mut *lock(workers));
        if retiring.is_empty() {
            break;
        }
        for worker in retiring {
            if worker.handle.thread().id() == thread::current().id() {
                // A callback may stop its own session; this worker unwinds after that callback.
                continue;
            }
            if worker
                .completed
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_ok()
                || worker.handle.is_finished()
            {
                complete &= worker.handle.join().is_ok();
            } else {
                complete = false;
            }
        }
    }
    if complete {
        Ok(())
    } else {
        Err("Native provider output did not close after owned teardown".to_owned())
    }
}

fn start_stderr_worker(
    reader: impl Read + Send + 'static,
    tail: Arc<Mutex<Vec<u8>>>,
    child: &Mutex<Child>,
) -> Result<NativeWorker, String> {
    spawn_worker("native-agent-stderr", move || drain_stderr(reader, &tail))
        .inspect_err(|_| terminate(child))
}

fn drain_stderr(mut reader: impl Read, tail: &Mutex<Vec<u8>>) {
    let mut bytes = [0; 4096];
    while let Ok(count) = reader.read(&mut bytes) {
        if count == 0 {
            break;
        }
        let mut tail = lock(tail);
        tail.extend_from_slice(bytes.get(..count).unwrap_or_default());
        if tail.len() > 8192 {
            let remove = tail.len().saturating_sub(8192);
            tail.drain(..remove);
        }
        drop(tail);
    }
}

fn route_reply(value: &Value, pending: &PendingReplies, provider: AgentKind) {
    if let Some(id) = if provider == AgentKind::Claude {
        field(value, "response").get("request_id")
    } else {
        value.get("id")
    } {
        let id = id.as_str().map_or_else(|| id.to_string(), str::to_owned);
        let sender = lock(pending).remove(&id);
        if let Some(reply) = sender {
            let result = if provider == AgentKind::Pi {
                crate::native_pi::reply(value, &reply.method)
            } else if provider == AgentKind::Claude {
                crate::native_claude::reply(value)
            } else {
                value.get("error").map_or_else(
                    || {
                        value
                            .get("result")
                            .cloned()
                            .ok_or_else(|| "Provider reply has no result".to_owned())
                    },
                    |error| Err(error.to_string()),
                )
            };
            let _ = reply.sender.send(ReplyEvent::Reply(result));
        }
    }
}

fn terminate(child: &Mutex<Child>) {
    crate::terminal_process::terminate_group(&mut lock(child));
}

fn ingest_claude_frame(
    snapshot: &Mutex<NativeSessionSnapshot>,
    protocol: &Mutex<crate::native_claude::ClaudeProtocol>,
    value: &Value,
) -> Result<Option<Value>, String> {
    let now = Some(chrono::Utc::now().timestamp_millis());
    let mut protocol = lock(protocol);
    lock(snapshot).with_message_times((now, now), |snapshot| protocol.ingest(snapshot, value))
}

fn ingest_pi_frame(snapshot: &Mutex<NativeSessionSnapshot>, value: &Value) -> Result<(), String> {
    let now = Some(chrono::Utc::now().timestamp_millis());
    lock(snapshot).with_message_times((now, now), |snapshot| {
        crate::native_pi::ingest(snapshot, value)
    })
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "One reader owns replies, input waits and deferred cancellation for this transport"
)]
fn read_frames(
    mut reader: impl BufRead,
    snapshot: &Arc<Mutex<NativeSessionSnapshot>>,
    pending: &PendingReplies,
    change_handler: &ChangePublisher,
    input: &Arc<Mutex<Option<ChildStdin>>>,
    next: &Arc<AtomicU64>,
    closing: &Arc<AtomicBool>,
    cancel: &Arc<AtomicBool>,
    workers: &NativeWorkers,
    claude: Option<&Mutex<crate::native_claude::ClaudeProtocol>>,
) -> Result<(), String> {
    let provider = lock(snapshot).provider;
    loop {
        let Some((value, count)) = read_native_frame(&mut reader, provider)? else {
            return Ok(());
        };
        if count > FRAME_LIMIT
            && !allows_history_page(&value, snapshot, pending)
            && !allows_image_frame(&value, snapshot, pending, claude)
            && !allows_history_echo(&value, snapshot, pending, claude)
        {
            return Err(
                "Native provider exceeded the control budget outside an owned image or history reply"
                    .to_owned(),
            );
        }
        if let Some(claude) = claude {
            if field(&value, "type") == "control_response" {
                route_reply(&value, pending, provider);
            } else {
                let response = ingest_claude_frame(snapshot, claude, &value)?;
                if let Some(response) = response {
                    write_frame(input, closing, &response)?;
                }
                publish_change(change_handler);
            }
            continue;
        }
        if provider == AgentKind::Pi {
            if field(&value, "type") == "response" {
                route_reply(&value, pending, provider);
            } else {
                ingest_pi_frame(snapshot, &value)?;
                let waiting = !lock(snapshot).requests.is_empty();
                update_input_wait(pending, waiting);
                publish_change(change_handler);
            }
            continue;
        }
        if value.get("method").is_none() {
            route_reply(&value, pending, provider);
            continue;
        }
        let shared_snapshot = Arc::clone(snapshot);
        let mut snapshot = lock(snapshot);
        if value.get("id").is_some() {
            if !snapshot.accepts(&value) {
                write_frame(
                    input,
                    closing,
                    &json!({"id":field(&value,"id"),"error":{"code":-32602,"message":"Request does not match the owned thread and turn"}}),
                )?;
                continue;
            }
            capture_request(&mut snapshot, &value)?;
        } else {
            // The reducer also accepts observed child lifecycles after the parent turn ends.
            // It still rejects foreign threads and unrelated turns before projecting content.
            let now = Some(chrono::Utc::now().timestamp_millis());
            snapshot.with_message_times((now, now), |snapshot| snapshot.ingest(&value));
        }
        let deferred = if field(&value, "method") == "turn/started"
            && snapshot.accepts(&value)
            && cancel.swap(false, Ordering::AcqRel)
        {
            Some(json!({"threadId":snapshot.session_id,"turnId":snapshot.turn_id}))
        } else {
            None
        };
        drop(snapshot);
        let waiting = !lock(&shared_snapshot).requests.is_empty();
        update_input_wait(pending, waiting);
        if let Some(params) = deferred {
            let input = Arc::clone(input);
            let pending = Arc::clone(pending);
            let next = Arc::clone(next);
            let closing = Arc::clone(closing);
            let handler = Arc::clone(change_handler);
            // Cancellation owns a separate RPC so it never waits for prompt acknowledgement.
            let turn =
                string(field(&params, "turnId")).ok_or("Missing interrupted turn identity")?;
            let worker = spawn_worker("native-agent-interrupt", move || {
                let result = send_rpc(
                    &input,
                    &pending,
                    &next,
                    &closing,
                    (AgentKind::Codex, "turn/interrupt", params, FRAME_LIMIT),
                );
                if let Err(error) = result {
                    if closing.load(Ordering::Acquire) {
                        return;
                    }
                    let mut snapshot = lock(&shared_snapshot);
                    snapshot.error = Some(bounded_text(error));
                    snapshot.changed();
                    drop(snapshot);
                } else {
                    let mut snapshot = lock(&shared_snapshot);
                    snapshot.finish_first_turn(&turn, NativeTurnOutcome::Interrupted);
                    snapshot.changed();
                    drop(snapshot);
                }
                publish_change(&handler);
            })?;
            lock(workers).push(worker);
        }
        publish_change(change_handler);
    }
}

fn read_native_frame(
    reader: &mut impl BufRead,
    provider: AgentKind,
) -> Result<Option<(Value, usize)>, String> {
    let mut bytes = Vec::new();
    let count = reader
        .by_ref()
        .take(u64::try_from(crate::MAX_NATIVE_IMAGE_ENVELOPE_BYTES + 1).unwrap_or(u64::MAX))
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if count == 0 {
        return Ok(None);
    }
    if count > crate::MAX_NATIVE_IMAGE_ENVELOPE_BYTES {
        return Err("Native agent frame exceeds 16 MiB".to_owned());
    }
    if matches!(provider, AgentKind::Pi | AgentKind::Claude) && bytes.last() != Some(&b'\n') {
        return Err("Provider emitted an unterminated JSONL record".to_owned());
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid native agent JSON: {error}"))?;
    if !value.is_object() {
        return Err("Native agent emitted a non-object frame".to_owned());
    }
    Ok(Some((value, count)))
}

fn allows_history_page(
    value: &Value,
    snapshot: &Mutex<NativeSessionSnapshot>,
    pending: &PendingReplies,
) -> bool {
    let snapshot = lock(snapshot);
    if snapshot.provider != AgentKind::Codex {
        return false;
    }
    let thread = snapshot.session_id.clone();
    drop(snapshot);
    let Some(id) = field(value, "id").as_u64().map(|id| id.to_string()) else {
        return false;
    };
    let pending = lock(pending);
    pending.get(&id).is_some_and(|reply| {
        reply.thread.as_deref() == thread.as_deref()
            && (reply.method == "thread/items/list"
                && field(field(value, "result"), "data")
                    .as_array()
                    .is_some_and(|items| items.len() <= 64)
                || reply.method == "thread/read"
                    && field(field(field(value, "result"), "thread"), "id").as_str()
                        == thread.as_deref()
                    && field(field(field(value, "result"), "thread"), "turns").is_array())
    })
}

fn allows_history_echo(
    value: &Value,
    snapshot: &Mutex<NativeSessionSnapshot>,
    pending: &PendingReplies,
    claude: Option<&Mutex<crate::native_claude::ClaudeProtocol>>,
) -> bool {
    let protocol = claude.map(lock);
    let current = lock(snapshot);
    let Some(expected) = &current.history_echo_text else {
        return false;
    };
    let paths = if let Some(claude) = &protocol {
        if !claude.allows_history_echo(&current, value) {
            return false;
        }
        vec!["/message/content".to_owned()]
    } else if current.provider == AgentKind::Pi {
        if current.pi_dispatch_id.is_none() {
            return false;
        }
        match field(value, "type").as_str() {
            Some("message_start" | "message_end")
                if field(field(value, "message"), "role") == "user" =>
            {
                vec!["/message/content".to_owned()]
            }
            Some("agent_end") if current.turn_id.is_some() => field(value, "messages")
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .filter(|(_, item)| field(item, "role") == "user")
                .map(|(index, _)| format!("/messages/{index}/content"))
                .collect(),
            _ => return false,
        }
    } else {
        if !current.accepts(value) {
            return false;
        }
        match field(value, "method").as_str() {
            Some("item/started" | "item/completed")
                if field(field(field(value, "params"), "item"), "type") == "userMessage" =>
            {
                vec!["/params/item/content".to_owned()]
            }
            Some("turn/completed") => field(field(field(value, "params"), "turn"), "items")
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .filter(|(_, item)| field(item, "type") == "userMessage")
                .map(|(index, _)| format!("/params/turn/items/{index}/content"))
                .collect(),
            _ => return false,
        }
    };
    // Only the exact host-sent history earns extra bytes; unrelated fields keep the control budget.
    let mut stripped = value.clone();
    let matched = paths
        .iter()
        .filter(|path| {
            stripped
                .pointer_mut(path)
                .is_some_and(|content| strip_history_echo(content, expected))
        })
        .count();
    drop(current);
    drop(protocol);
    matched == 1
        && (stripped.to_string().len() <= FRAME_LIMIT
            || allows_image_frame(&stripped, snapshot, pending, claude))
}

fn strip_history_echo(content: &mut Value, expected: &str) -> bool {
    if content.as_str() == Some(expected) {
        *content = json!("");
    } else if let Some(blocks) = content.as_array_mut() {
        let mut matched = false;
        for block in blocks {
            if field(block, "type") == "text" && field(block, "text").as_str() == Some(expected) {
                if matched {
                    return false;
                }
                block["text"] = json!("");
                matched = true;
            }
        }
        if !matched {
            return false;
        }
    } else {
        return false;
    }
    true
}

fn allows_image_frame(
    value: &Value,
    snapshot: &Mutex<NativeSessionSnapshot>,
    pending: &PendingReplies,
    claude: Option<&Mutex<crate::native_claude::ClaudeProtocol>>,
) -> bool {
    if let Some(claude) = claude {
        return lock(claude).allows_image_echo(&lock(snapshot), value);
    }
    let snapshot = lock(snapshot);
    let mut content = snapshot.computer_image_contents(value);
    content.extend(completion_image_contents(value, &snapshot));
    if let Some(encoded_bytes) = crate::native_prompt::tool_image_bytes(&content)
        && value.to_string().len().saturating_sub(encoded_bytes) <= FRAME_LIMIT
    {
        return true;
    }
    if snapshot.provider == AgentKind::Pi {
        if field(value, "type") == "response" && field(value, "command") == "get_messages" {
            let id = field(value, "id").as_str();
            let pending = lock(pending);
            return id
                .and_then(|id| pending.get(id))
                .is_some_and(|reply| reply.method == "get_messages")
                && history_image_frame(value, AgentKind::Pi);
        }
        return snapshot.image_echo_count > 0
            && snapshot.pi_dispatch_id.is_some()
            && matches!(
                field(value, "type").as_str(),
                Some("message_start" | "message_end")
            )
            && field(field(value, "message"), "role") == "user"
            && image_blocks(
                field(field(value, "message"), "content"),
                snapshot.image_echo_count,
                "image",
            );
    }
    if snapshot.accepts(value)
        && field(field(field(value, "params"), "item"), "type") == "userMessage"
    {
        return snapshot.image_echo_count > 0
            && image_blocks(
                field(field(field(value, "params"), "item"), "content"),
                snapshot.image_echo_count,
                "image",
            );
    }
    let id = value
        .get("id")
        .map(|id| id.as_str().map_or_else(|| id.to_string(), str::to_owned));
    let image_count = snapshot.image_echo_count;
    drop(snapshot);
    let pending = lock(pending);
    let Some(reply) = id.as_ref().and_then(|id| pending.get(id)) else {
        return false;
    };
    let result = field(value, "result");
    if reply.method == "turn/start" && image_count > 0 {
        return field(field(result, "turn"), "items")
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    field(item, "type") == "userMessage"
                        && image_blocks(field(item, "content"), image_count, "image")
                })
            });
    }
    if matches!(reply.method.as_str(), "thread/resume" | "thread/read")
        && reply
            .thread
            .as_deref()
            .is_some_and(|id| field(field(result, "thread"), "id") == id)
    {
        return history_image_frame(value, AgentKind::Codex);
    }
    false
}

fn completion_image_contents<'a>(
    value: &'a Value,
    snapshot: &NativeSessionSnapshot,
) -> Vec<&'a Value> {
    let (messages, role, expected) = if snapshot.provider == AgentKind::Pi
        && snapshot.pi_dispatch_id.is_some()
        && snapshot.turn_id.is_some()
        && field(value, "type") == "agent_end"
    {
        // Pi repeats this run's messages before settling; admitted images retain their budget.
        (field(value, "messages"), "role", "user")
    } else if field(value, "method") == "turn/completed" && snapshot.accepts(value) {
        (
            field(field(field(value, "params"), "turn"), "items"),
            "type",
            "userMessage",
        )
    } else {
        return Vec::new();
    };
    messages
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| {
            field(message, role) == expected
                && snapshot.image_echo_count > 0
                && image_blocks(
                    field(message, "content"),
                    snapshot.image_echo_count,
                    "image",
                )
        })
        .map(|message| field(message, "content"))
        .collect()
}

fn history_image_frame(value: &Value, provider: AgentKind) -> bool {
    // Exact correlated history replies read provider-owned pixels; they never issue capture grants.
    let contents = if provider == AgentKind::Pi {
        field(field(value, "data"), "messages")
            .as_array()
            .into_iter()
            .flatten()
            .filter(|message| {
                field(message, "role") == "user"
                    || field(message, "role") == "toolResult"
                        && field(message, "toolName").as_str().is_some_and(|name| {
                            matches!(
                                crate::native_pi::result_tool_name(name, message),
                                "computer_snapshot" | "codemode"
                            )
                        })
            })
            .map(|message| field(message, "content"))
            .filter(|content| {
                content.as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| field(block, "type") == "image")
                })
            })
            .collect::<Vec<_>>()
    } else {
        field(field(field(value, "result"), "thread"), "turns")
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|turn| field(turn, "items").as_array().into_iter().flatten())
            .filter_map(|item| {
                let content = if field(item, "type") == "userMessage" {
                    field(item, "content")
                } else if field(item, "type") == "mcpToolCall"
                    && field(item, "tool") == "computer_snapshot"
                {
                    field(field(item, "result"), "content")
                } else {
                    return None;
                };
                content
                    .as_array()
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| field(block, "type") == "image")
                    })
                    .then_some(content)
            })
            .collect::<Vec<_>>()
    };
    crate::native_prompt::tool_image_bytes(&contents).is_some_and(|encoded_bytes| {
        value.to_string().len().saturating_sub(encoded_bytes) <= FRAME_LIMIT
    })
}

fn image_blocks(content: &Value, count: usize, image_kind: &str) -> bool {
    content.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .filter(|block| field(block, "type") == image_kind)
            .count()
            == count
            && blocks
                .iter()
                .all(|block| matches!(field(block, "type").as_str(), Some("text" | "image")))
    })
}

fn capture_request(snapshot: &mut NativeSessionSnapshot, value: &Value) -> Result<(), String> {
    let method = field(value, "method").as_str().unwrap_or_default();
    if !matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/tool/requestUserInput"
            | "mcpServer/elicitation/request"
    ) {
        return Err(format!("Unsupported native provider request: {method}"));
    }
    let parameters = field(value, "params").clone();
    if parameters.to_string().len() > 16 * 1024 || snapshot.requests.len() >= 32 {
        return Err("Native provider pending requests exceed the bounded view".to_owned());
    }
    let wire_id = field(value, "id").clone();
    if !(wire_id.is_string() || wire_id.is_number()) || wire_id.to_string().len() > 8192 {
        return Err("Provider request has an invalid identity".to_owned());
    }
    if snapshot
        .requests
        .iter()
        .any(|request| request.wire_id == wire_id)
    {
        return Err("Provider reused a pending request identity".to_owned());
    }
    let id = format!("request-{}-{}", snapshot.revision, wire_id);
    let from_attached_tools = method == "mcpServer/elicitation/request"
        && snapshot
            .tool_server
            .as_deref()
            .is_some_and(|server| field(&parameters, "serverName").as_str() == Some(server));
    snapshot.requests.push(NativeAgentRequest {
        id,
        method: method.to_owned(),
        parameters,
        from_attached_tools,
        wire_id,
    });
    snapshot.status = NativeSessionStatus::Waiting;
    snapshot.changed();
    Ok(())
}

fn validate_answers(parameters: &Value, response: &Value) -> Result<(), String> {
    let answers = field(response, "answers")
        .as_object()
        .ok_or("Question response requires answers")?;
    if response.as_object().is_none_or(|fields| fields.len() != 1) || answers.len() > 16 {
        return Err("Invalid native question response".to_owned());
    }
    for (id, answer) in answers {
        if !field(parameters, "questions")
            .as_array()
            .into_iter()
            .flatten()
            .any(|question| field(question, "id") == id.as_str())
        {
            return Err("Answer belongs to an unknown provider question".to_owned());
        }
        if answer.as_object().is_none_or(|fields| fields.len() != 1)
            || field(answer, "answers").as_array().is_none_or(|values| {
                values.len() > 16 || values.iter().any(|value| !value.is_string())
            })
        {
            return Err("Question answers must be arrays of text".to_owned());
        }
    }
    Ok(())
}

fn send_rpc(
    input: &Mutex<Option<ChildStdin>>,
    pending: &PendingReplies,
    next: &AtomicU64,
    closing: &AtomicBool,
    request: (AgentKind, &str, Value, usize),
) -> Result<Value, String> {
    let method = request.1;
    let request = begin_rpc(input, pending, next, closing, request)?;
    let result = wait_for_reply(&request.receiver, method, ReplyWait::new()?);
    lock(pending).remove(&request.id);
    result
}

fn begin_rpc(
    input: &Mutex<Option<ChildStdin>>,
    pending: &PendingReplies,
    next: &AtomicU64,
    closing: &AtomicBool,
    request: (AgentKind, &str, Value, usize),
) -> Result<PendingRequest, String> {
    let (provider, method, params, frame_limit) = request;
    let number = next.fetch_add(1, Ordering::Relaxed);
    let id = if matches!(provider, AgentKind::Pi | AgentKind::Claude) {
        format!("{}-{number}", provider.default_program())
    } else {
        number.to_string()
    };
    let thread = string(field(&params, "threadId"));
    let request = if provider == AgentKind::Pi {
        crate::native_pi::command(id.clone(), method, &params)?
    } else if provider == AgentKind::Claude {
        crate::native_claude::command(&id, method, &params)?
    } else {
        Value::Object(serde_json::Map::from_iter([
            ("id".to_owned(), number.into()),
            ("method".to_owned(), method.into()),
            ("params".to_owned(), params),
        ]))
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    {
        let mut pending = lock(pending);
        if pending.len() >= 64 {
            return Err("Too many outstanding native agent requests".to_owned());
        }
        pending.insert(
            id.clone(),
            PendingReply {
                method: method.to_owned(),
                thread,
                waiting_for_input: false,
                sender,
            },
        );
    }
    if let Err(error) = write_frame_with_limit(input, closing, &request, frame_limit) {
        lock(pending).remove(&id);
        return Err(error);
    }
    Ok(PendingRequest { id, receiver })
}

fn update_input_wait(pending: &PendingReplies, waiting: bool) {
    for reply in lock(pending).values_mut() {
        if matches!(reply.method.as_str(), "prompt" | "turn/start")
            && reply.waiting_for_input != waiting
        {
            reply.waiting_for_input = waiting;
            let _ = reply.sender.send(ReplyEvent::InputWait {
                waiting,
                at: Instant::now(),
            });
        }
    }
}

impl ReplyWait {
    fn new() -> Result<Self, String> {
        Ok(Self {
            remaining: REQUEST_TIMEOUT,
            deadline: Instant::now()
                .checked_add(REQUEST_TIMEOUT)
                .ok_or("Invalid native request deadline")?,
            waiting: false,
        })
    }

    fn receive(
        &mut self,
        receiver: &mpsc::Receiver<ReplyEvent>,
        method: &str,
    ) -> Result<ReplyEvent, String> {
        // A provider may acknowledge an extension command only after its question is answered.
        // Human input consumes no transport budget; stop and EOF still wake this bounded channel.
        let event = if self.waiting {
            receiver.recv().map_err(|error| error.to_string())
        } else {
            receiver
                .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| error.to_string())
        }
        .map_err(|error| {
            format!("Native {method} reply unavailable; delivery may have occurred: {error}")
        })?;
        if let ReplyEvent::InputWait { waiting, at } = &event {
            if *waiting {
                self.remaining = self
                    .deadline
                    .saturating_duration_since(*at)
                    .min(REQUEST_TIMEOUT);
            } else {
                self.deadline = at
                    .checked_add(self.remaining)
                    .ok_or("Invalid native request deadline")?;
            }
            self.waiting = *waiting;
        }
        Ok(event)
    }
}

fn wait_for_reply(
    receiver: &mpsc::Receiver<ReplyEvent>,
    method: &str,
    mut wait: ReplyWait,
) -> Result<Value, String> {
    loop {
        if let ReplyEvent::Reply(result) = wait.receive(receiver, method)? {
            return result;
        }
    }
}

fn write_frame(
    input: &Mutex<Option<ChildStdin>>,
    closing: &AtomicBool,
    value: &Value,
) -> Result<(), String> {
    write_frame_with_limit(input, closing, value, FRAME_LIMIT)
}

fn write_frame_with_limit(
    input: &Mutex<Option<ChildStdin>>,
    closing: &AtomicBool,
    value: &Value,
    limit: usize,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() >= limit {
        return Err("Native agent frame exceeds its bounded envelope".to_owned());
    }
    bytes.push(b'\n');
    if closing.load(Ordering::Acquire) {
        return Err("Native agent session is stopped".to_owned());
    }
    lock(input)
        .as_mut()
        .ok_or("Native agent stdin is closed")?
        .write_all(&bytes)
        .map_err(|error| error.to_string())
}

pub fn publish_change(publisher: &ChangePublisher) {
    let callback = lock(publisher).clone();
    if let Some(callback) = callback {
        callback();
    }
}

fn claude_protocol(
    config: &NativeSessionConfig,
) -> Result<Option<Arc<Mutex<crate::native_claude::ClaudeProtocol>>>, String> {
    if config.provider != AgentKind::Claude {
        return Ok(None);
    }
    let identity = config
        .session_id
        .clone()
        .or_else(|| config.fresh_session_id.clone())
        .ok_or("Missing captured Claude UUID")?;
    let cwd = if config.remote.is_some() {
        config.cwd.clone()
    } else {
        config
            .cwd
            .canonicalize()
            .map_err(|error| error.to_string())?
    };
    Ok(Some(Arc::new(Mutex::new(
        crate::native_claude::ClaudeProtocol::new(identity, cwd)?,
    ))))
}

fn spawn_native_child(
    config: &NativeSessionConfig,
    tools: Option<&crate::ToolBridge>,
    attachment_directory: Option<&std::path::Path>,
) -> Result<(Child, NativeLaunchResources), String> {
    if tools.is_some_and(|tools| {
        tools.lease().scope().provider != config.provider
            || tools.lease().terminal_target().is_none()
    }) {
        return Err("Native tools require this provider's exact bound task terminal".to_owned());
    }
    let permission_extension = config.permissions.pi_extension(config.provider, tools)?;
    let (tool_arguments, remote_tools, permission_path) = if let Some(remote) = &config.remote
        && let Some(tools) = tools
    {
        let (arguments, relay, permission_path) = tools.remote_arguments(
            &bootty_host::remote::RemoteHost::new(remote.host.clone()),
            &remote.daemon,
            permission_extension
                .as_ref()
                .map(tempfile::NamedTempFile::path),
        )?;
        (arguments, Some(relay), permission_path)
    } else {
        (
            tools.map_or_else(Vec::new, crate::ToolBridge::arguments),
            None,
            permission_extension
                .as_ref()
                .map(|file| file.path().to_path_buf()),
        )
    };
    native_command(
        config,
        &tool_arguments,
        attachment_directory,
        permission_path.as_deref(),
    )?
    .spawn()
    .map(|child| {
        (
            child,
            NativeLaunchResources {
                _permission_extension: permission_extension,
                _remote_tools: remote_tools,
            },
        )
    })
    .map_err(|error| format!("Cannot start {}: {error}", config.provider))
}

fn native_command(
    config: &NativeSessionConfig,
    tool_arguments: &[String],
    attachment_directory: Option<&std::path::Path>,
    permission_extension: Option<&Path>,
) -> Result<Command, String> {
    config.validate()?;
    #[cfg(not(unix))]
    return Err("Native conversations currently require a Unix provider transport".to_owned());
    #[cfg(unix)]
    {
        let mut command = Command::new(&config.program);
        crate::terminal_process::configure_group(&mut command);
        if config.provider == AgentKind::Pi {
            command.args(tool_arguments).args(&config.arguments);
            if let Some(extension) = permission_extension {
                command.arg("--extension").arg(extension);
            }
            command.args(["--mode", "rpc"]);
            if let Some(file) = &config.session_file {
                command.args(["--session", file]);
            }
            if let Some(model) = &config.model {
                command.args(["--model", model]);
            }
        } else if config.provider == AgentKind::Claude {
            let identity = config
                .session_id
                .as_deref()
                .or(config.fresh_session_id.as_deref())
                .ok_or("Missing captured Claude UUID")?;
            command.args(tool_arguments).args(&config.arguments);
            if let Some(mode) = config.permissions.claude() {
                command.args(["--permission-mode", mode]);
                if config.permissions == crate::NativePermissionMode::FullAccess {
                    command.arg("--allow-dangerously-skip-permissions");
                }
            }
            if let Some(directory) = attachment_directory {
                if !directory.is_absolute() {
                    return Err("Native Claude attachment directory must be absolute".to_owned());
                }
                command.arg("--add-dir").arg(directory);
            }
            command.args(crate::native_claude::launch_arguments(
                identity,
                config.session_id.is_some(),
            )?);
            if let Some(model) = &config.model {
                command.args(["--model", model]);
            }
            if config.fast_mode {
                command.args(["--settings", r#"{"fastMode":true}"#]);
            }
            if let Some(effort) = &config.reasoning_effort {
                command.args(["--effort", effort]);
            }
        } else {
            command
                .args(["app-server", "--listen", "stdio://"])
                // A scoped override lets older SDKs ignore the unknown question feature.
                .args(["--config", "features.default_mode_request_user_input=true"])
                .args(tool_arguments)
                .args(crate::codex_terminal::provider_arguments(&config.arguments));
        }
        if let Some(remote) = &config.remote {
            command = remote_native_command(config, remote, &command)?;
        } else {
            command.current_dir(&config.cwd);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("BOOTTY_AGENT_LAUNCH_CONTEXT")
            .env_remove("BOOTTY_CONTROL_ENDPOINT")
            .env("BOOTTY_NATIVE_AGENT", "1");
        let account = config
            .account_directory
            .as_deref()
            .ok_or("Missing captured account directory")?;
        // Preserve Claude's normal CLI environment for its default account.
        let default_claude = config.remote.is_none()
            && config.provider == AgentKind::Claude
            && std::env::var("HOME").is_ok_and(|home| account == format!("{home}/.claude"));
        if default_claude {
            command.env_remove(config.provider.account_directory_variable());
        } else {
            command.env(config.provider.account_directory_variable(), account);
        }
        Ok(command)
    }
}

impl NativeAgentSession {
    pub(crate) fn attach_browser(
        &self,
        target: &CommandTarget,
        attachment: Option<crate::NativeBrowserAttachment>,
    ) -> Result<(), String> {
        self.tools
            .as_ref()
            .ok_or("Conversation tools are not attached")?
            .lease()
            .attach_browser(target, attachment)
    }
    pub(crate) fn grant_applications(
        &self,
        target: &CommandTarget,
        prompt: &crate::NativePrompt,
    ) -> Result<(), String> {
        if let Some(tools) = &self.tools {
            tools
                .lease()
                .grant_applications(target, prompt.applications())?;
            if !prompt.applications().is_empty() {
                lock(&self.snapshot).computer_tool_server = Some(tools.server_name().to_owned());
            }
            Ok(())
        } else if prompt.applications().is_empty() {
            Ok(())
        } else {
            Err("Conversation tools are not attached".into())
        }
    }
    pub(crate) fn application_access(
        &self,
        target: &CommandTarget,
        reference: &str,
        caller: bootty_control::Caller,
    ) -> Result<crate::NativeApplicationAccess, String> {
        let lease = self
            .tools
            .as_ref()
            .ok_or("Conversation tools are not attached")?
            .lease();
        if lease.caller() != caller {
            return Err(
                "Application access belongs to this conversation's attached tool caller".into(),
            );
        }
        lease.application_access(target, reference)
    }
}

fn initial_snapshot(
    provider: AgentKind,
    tools: Option<&crate::ToolBridge>,
) -> NativeSessionSnapshot {
    let mut initial = NativeSessionSnapshot::new(provider);
    initial.application_mentions_supported =
        tools.is_some_and(|tools| tools.lease().application_mentions_supported());
    initial.tool_server = tools.map(|tools| tools.server_name().to_owned());
    initial.computer_tool_server = tools
        .filter(|tools| tools.lease().enabled(Some(crate::ToolCapture::Computer)))
        .map(|tools| tools.server_name().to_owned());
    initial
}

#[cfg(unix)]
fn remote_native_command(
    config: &NativeSessionConfig,
    remote: &NativeRemote,
    command: &Command,
) -> Result<Command, String> {
    let mut arguments = vec![
        "-u".to_owned(),
        "BOOTTY_AGENT_LAUNCH_CONTEXT".to_owned(),
        "-u".to_owned(),
        "BOOTTY_CONTROL_ENDPOINT".to_owned(),
        format!(
            "{}={}",
            config.provider.account_directory_variable(),
            config
                .account_directory
                .as_deref()
                .ok_or("Missing captured remote account")?
        ),
        "BOOTTY_NATIVE_AGENT=1".to_owned(),
        config.program.clone(),
    ];
    arguments.extend(
        command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned()),
    );
    let remote = bootty_host::remote::RemoteHost::new(remote.host.clone());
    let cwd = config
        .cwd
        .to_str()
        .ok_or("Remote directory must be UTF-8")?;
    let (program, arguments) = remote
        .proxy_command_in(cwd, "/usr/bin/env", &arguments)
        .map_err(|error| error.to_string())?;
    let mut command = Command::new(program);
    crate::terminal_process::configure_group(&mut command);
    command.args(arguments);
    Ok(command)
}
