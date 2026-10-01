use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AgentKind, AgentLaunch, NativeAgentRequest, NativeSessionSnapshot, NativeSessionStatus,
    native_protocol::{field, string},
};

const FRAME_LIMIT: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
type PendingReplies = Arc<Mutex<BTreeMap<String, mpsc::SyncSender<Result<Value, String>>>>>;
pub type NativeChangeHandler = Arc<dyn Fn() + Send + Sync>;
type ChangePublisher = Arc<Mutex<Option<NativeChangeHandler>>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeSessionConfig {
    pub provider: AgentKind,
    pub program: String,
    pub cwd: PathBuf,
    pub arguments: Vec<String>,
    pub session_id: Option<String>,
    /// One-shot source selector; the durable session resumes the resulting identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_session_id: Option<String>,
    pub model: Option<String>,
}

impl NativeSessionConfig {
    #[must_use]
    pub fn new(provider: AgentKind, cwd: impl Into<PathBuf>) -> Self {
        Self {
            provider,
            program: provider.default_program().to_owned(),
            cwd: cwd.into(),
            arguments: Vec::new(),
            session_id: None,
            fork_session_id: None,
            model: None,
        }
    }
}

/// A bidirectional provider child. Spawn and requests run on a host worker; snapshots never wait
/// for provider I/O. Dropping this owner terminates and reaps its process tree.
pub struct NativeAgentSession {
    config: NativeSessionConfig,
    child: Arc<Mutex<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    snapshot: Arc<Mutex<NativeSessionSnapshot>>,
    pending: PendingReplies,
    next_request: AtomicU64,
    closing: Arc<AtomicBool>,
    change_handler: ChangePublisher,
}

impl NativeAgentSession {
    /// # Errors
    /// Returns provider launch, handshake, resume or protocol errors after reaping the child.
    pub fn spawn(config: NativeSessionConfig) -> Result<Self, String> {
        let session = Self::start(config)?;
        session.initialize()?;
        Ok(session)
    }

    pub(crate) fn start(config: NativeSessionConfig) -> Result<Self, String> {
        let mut command = native_command(&config)?;
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start {}: {error}", config.provider))?;
        let stdin = child.stdin.take().ok_or("Agent stdin was not opened")?;
        let stdout = child.stdout.take().ok_or("Agent stdout was not opened")?;
        let stderr = child.stderr.take().ok_or("Agent stderr was not opened")?;
        let child = Arc::new(Mutex::new(child));
        let snapshot = Arc::new(Mutex::new(NativeSessionSnapshot::new(config.provider)));
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let closing = Arc::new(AtomicBool::new(false));
        let change_handler = Arc::new(Mutex::new(None));
        let tail = Arc::new(Mutex::new(Vec::new()));
        let tail_writer = Arc::clone(&tail);
        thread::Builder::new()
            .name("native-agent-stderr".to_owned())
            .spawn(move || {
                let mut reader = stderr;
                let mut bytes = [0; 4096];
                while let Ok(count) = reader.read(&mut bytes) {
                    if count == 0 {
                        break;
                    }
                    let mut tail = lock(&tail_writer);
                    tail.extend_from_slice(bytes.get(..count).unwrap_or_default());
                    if tail.len() > 8192 {
                        let remove = tail.len().saturating_sub(8192);
                        tail.drain(..remove);
                    }
                }
            })
            .map_err(|error| {
                terminate(&child);
                error.to_string()
            })?;
        let reader_snapshot = Arc::clone(&snapshot);
        let reader_pending = Arc::clone(&pending);
        let reader_child = Arc::clone(&child);
        let reader_closing = Arc::clone(&closing);
        let reader_handler = Arc::clone(&change_handler);
        thread::Builder::new()
            .name("native-agent-protocol".to_owned())
            .spawn(move || {
                let fault = read_frames(
                    BufReader::new(stdout),
                    &reader_snapshot,
                    &reader_pending,
                    &reader_handler,
                )
                .err();
                // EOF ends the protocol lease even when a misbehaving child keeps running.
                terminate(&reader_child);
                let status = lock(&reader_child).wait();
                let mut snapshot = lock(&reader_snapshot);
                let intentional = reader_closing.load(Ordering::Acquire);
                let error = fault.unwrap_or_else(|| {
                    format!(
                        "Agent process ended ({}): {}",
                        status.map_or_else(|error| error.to_string(), |status| status.to_string()),
                        String::from_utf8_lossy(&lock(&tail))
                    )
                });
                if !intentional {
                    snapshot.error = Some(error.clone());
                }
                snapshot.status = if intentional {
                    NativeSessionStatus::Stopped
                } else {
                    NativeSessionStatus::Error
                };
                snapshot.changed();
                drop(snapshot);
                publish_change(&reader_handler);
                let replies = std::mem::take(&mut *lock(&reader_pending));
                for (_, reply) in replies {
                    let _ = reply.send(Err(error.clone()));
                }
            })
            .map_err(|error| {
                terminate(&child);
                error.to_string()
            })?;
        let session = Self {
            config,
            child,
            stdin: Mutex::new(Some(stdin)),
            snapshot,
            pending,
            next_request: AtomicU64::new(1),
            closing,
            change_handler,
        };
        Ok(session)
    }

    pub(crate) fn initialize(&self) -> Result<(), String> {
        match self.config.provider {
            AgentKind::Codex => {
                self.rpc("initialize", json!({"clientInfo":{"name":"bootty","title":"Bootty","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
                self.write(&json!({"method":"initialized"}))?;
                let (method, selector) = match (
                    self.config.fork_session_id.as_ref(),
                    self.config.session_id.as_ref(),
                ) {
                    (Some(id), _) => ("thread/fork", Some(id)),
                    (_, Some(id)) => ("thread/resume", Some(id)),
                    (None, None) => ("thread/start", None),
                };
                let params = selector.map_or_else(
                    || json!({"cwd":self.config.cwd,"model":self.config.model}),
                    |id| json!({"threadId":id,"cwd":self.config.cwd,"model":self.config.model}),
                );
                // Fork in the destination owner: a second server cannot resume its active writer.
                let result = self.rpc(method, params)?;
                let id = string(field(field(&result, "thread"), "id"))
                    .ok_or("Provider returned no thread identity")?;
                let mut snapshot = lock(&self.snapshot);
                snapshot.session_id = Some(id);
                if let Some(turns) = field(field(&result, "thread"), "turns").as_array() {
                    for turn in turns {
                        for item in field(turn, "items").as_array().into_iter().flatten() {
                            snapshot
                                .ingest(&json!({"method":"item/completed","params":{"item":item}}));
                        }
                    }
                }
                snapshot.status = NativeSessionStatus::Idle;
                snapshot.changed();
            }
            AgentKind::Claude => {
                let result = self.rpc(
                    "initialize",
                    json!({"hooks":null,"sdkMcpServers":[],"sdkAgents":[]}),
                )?;
                let mut snapshot = lock(&self.snapshot);
                snapshot.session_id = string(field(&result, "session_id"))
                    .or_else(|| self.config.session_id.clone())
                    .or_else(|| {
                        self.config
                            .arguments
                            .iter()
                            .position(|arg| arg == "--session-id")
                            .and_then(|index| {
                                self.config.arguments.get(index.saturating_add(1)).cloned()
                            })
                    });
                snapshot.status = NativeSessionStatus::Idle;
                snapshot.changed();
            }
            AgentKind::Pi => {
                let result = self.rpc("get_state", json!({}))?;
                let mut snapshot = lock(&self.snapshot);
                snapshot.session_id = string(field(&result, "sessionId"));
                snapshot.session_file = string(field(&result, "sessionFile"));
                snapshot.status = NativeSessionStatus::Idle;
                snapshot.changed();
                drop(snapshot);
                let history = self.rpc("get_messages", json!({}))?;
                for message in field(&history, "messages").as_array().into_iter().flatten() {
                    lock(&self.snapshot).ingest(&json!({"type":"message_end","message":message}));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> NativeSessionSnapshot {
        lock(&self.snapshot).clone()
    }

    pub(crate) fn restore_recent_history(&self, history: &NativeSessionSnapshot) {
        let mut snapshot = lock(&self.snapshot);
        if snapshot.transcript.is_empty() {
            snapshot.transcript.clone_from(&history.transcript);
            snapshot.usage.clone_from(&history.usage);
            snapshot.changed();
        }
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
        if self.closing.load(Ordering::Acquire) {
            return Err("Native agent session is stopped".to_owned());
        }
        if message.is_empty() || message.len() > 64 * 1024 {
            return Err("An agent prompt must contain 1–65536 bytes".to_owned());
        }
        if matches!(
            self.snapshot().status,
            NativeSessionStatus::Working | NativeSessionStatus::Waiting
        ) {
            return Err("Agent is busy; interrupt or wait for its current turn".to_owned());
        }
        {
            let mut snapshot = lock(&self.snapshot);
            let id = format!("local-user-{}", snapshot.revision);
            snapshot.pending_user_item = Some(id.clone());
            snapshot.message(id, "user", message.to_owned(), false);
            snapshot.status = NativeSessionStatus::Working;
            snapshot.changed();
        }
        let result = match self.config.provider {
            AgentKind::Codex => {
                let id = self.snapshot().session_id.ok_or("Agent has no native thread")?;
                self.rpc("turn/start", json!({"threadId":id,"input":[{"type":"text","text":message}]})).map(|_| ())
            }
            AgentKind::Claude => self.write(&json!({"type":"user","session_id":self.snapshot().session_id.unwrap_or_default(),"message":{"role":"user","content":message},"parent_tool_use_id":null})),
            // Pi acknowledges some prompts only after the entire turn. Delivery uses the owned
            // input stream; its response or message-end event reports rejection/completion.
            AgentKind::Pi => self.write(&json!({"type":"prompt","message":message})),
        };
        let mut snapshot = lock(&self.snapshot);
        if let Err(error) = &result {
            snapshot.status = NativeSessionStatus::Error;
            snapshot.error = Some(error.clone());
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
        result
    }

    /// # Errors
    /// Returns provider or transport errors; interrupt preserves the resumable session.
    pub fn interrupt(&self) -> Result<(), String> {
        let snapshot = self.snapshot();
        match self.config.provider {
            AgentKind::Codex => {
                self.rpc("turn/interrupt", json!({"threadId":snapshot.session_id,"turnId":snapshot.turn_id.ok_or("No active agent turn")?}))?;
            }
            AgentKind::Claude => {
                self.rpc("interrupt", json!({}))?;
            }
            AgentKind::Pi => {
                self.rpc("abort", json!({}))?;
            }
        }
        Ok(())
    }

    /// Reply only to a first-hand, currently pending provider request.
    /// # Errors
    /// Returns an error for unknown requests, invalid response types, or transport failure.
    pub fn respond(&self, id: &str, response: Value) -> Result<(), String> {
        let request = self
            .snapshot()
            .requests
            .into_iter()
            .find(|request| request.id == id)
            .ok_or("Agent request is no longer pending")?;
        let value = match self.config.provider {
            AgentKind::Codex => {
                json!({"id":serde_json::from_str::<Value>(id).unwrap_or_else(|_| id.into()),"result":response})
            }
            AgentKind::Claude => {
                json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}})
            }
            AgentKind::Pi if request.method == "confirm" => {
                json!({"type":"extension_ui_response","id":id,"confirmed":response.as_bool().ok_or("Confirmation requires a boolean response")?})
            }
            AgentKind::Pi => json!({"type":"extension_ui_response","id":id,"value":response}),
        };
        drop(response);
        self.write(&value)?;
        let mut snapshot = lock(&self.snapshot);
        snapshot.requests.retain(|item| item.id != request.id);
        if snapshot.requests.is_empty() {
            snapshot.status = NativeSessionStatus::Working;
        }
        snapshot.changed();
        drop(snapshot);
        Ok(())
    }

    /// # Errors
    /// Returns an error if the request is not a command/file/tool permission request.
    pub fn approve(&self, id: &str, allow: bool) -> Result<(), String> {
        let request = self
            .snapshot()
            .requests
            .into_iter()
            .find(|request| request.id == id)
            .ok_or("Agent request is no longer pending")?;
        let response = match self.config.provider {
            AgentKind::Codex
                if matches!(
                    request.method.as_str(),
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
                ) =>
            {
                json!({"decision":if allow {"accept"} else {"decline"}})
            }
            AgentKind::Claude if request.method == "can_use_tool" => {
                if allow {
                    json!({"behavior":"allow","updatedInput":field(&request.parameters,"input")})
                } else {
                    json!({"behavior":"deny","message":"Declined in Bootty"})
                }
            }
            AgentKind::Pi if request.method == "confirm" => json!(allow),
            _ => return Err("This request requires a structured answer".to_owned()),
        };
        self.respond(id, response)
    }

    /// Read native provider history. Call from a worker; the provider owns durable storage.
    /// # Errors
    /// Returns provider protocol errors or unsupported history operations.
    pub fn history(&self) -> Result<Value, String> {
        match self.config.provider {
            AgentKind::Codex => self.rpc(
                "thread/read",
                json!({"threadId":self.snapshot().session_id,"includeTurns":true}),
            ),
            AgentKind::Pi => self.rpc("get_messages", json!({})),
            AgentKind::Claude => Ok(
                json!({"session_id":self.snapshot().session_id,"messages":self.snapshot().transcript}),
            ),
        }
    }

    /// Normalize provider history into the same bounded transcript used during live turns.
    /// # Errors
    /// Returns first-hand provider history errors.
    pub fn refresh_history(&self) -> Result<NativeSessionSnapshot, String> {
        let history = self.history()?;
        let mut snapshot = lock(&self.snapshot);
        match self.config.provider {
            AgentKind::Codex => {
                if let Some(turns) = field(field(&history, "thread"), "turns").as_array() {
                    snapshot.transcript.clear();
                    for turn in turns {
                        for item in field(turn, "items").as_array().into_iter().flatten() {
                            snapshot
                                .ingest(&json!({"method":"item/completed","params":{"item":item}}));
                        }
                    }
                }
            }
            AgentKind::Pi => {
                if let Some(messages) = field(&history, "messages").as_array() {
                    snapshot.transcript.clear();
                    for message in messages {
                        snapshot.ingest(&json!({"type":"message_end","message":message}));
                    }
                }
            }
            AgentKind::Claude => {}
        }
        Ok(snapshot.clone())
    }

    /// Execute a provider request on a worker. Native account/history adapters use the same
    /// initialized transport instead of starting an unowned parallel provider process.
    /// # Errors
    /// Returns transport errors, provider rejection or timeout (delivery may be ambiguous).
    pub fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let number = self.next_request.fetch_add(1, Ordering::Relaxed);
        let id = number.to_string();
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut pending = lock(&self.pending);
            if pending.len() >= 64 {
                return Err("Too many outstanding native agent requests".to_owned());
            }
            pending.insert(id.clone(), sender);
        }
        let value = match self.config.provider {
            AgentKind::Codex => json!({"id":number,"method":method,"params":params}),
            AgentKind::Claude => {
                json!({"type":"control_request","request_id":id,"request":with_field(params,"subtype",method)})
            }
            AgentKind::Pi => with_field(with_field(params, "type", method), "id", &id),
        };
        if let Err(error) = self.write(&value) {
            lock(&self.pending).remove(&id);
            return Err(error);
        }
        let result = receiver.recv_timeout(REQUEST_TIMEOUT).map_err(|error| {
            format!("Native {method} reply unavailable; delivery may have occurred: {error}")
        });
        lock(&self.pending).remove(&id);
        result?
    }

    fn write(&self, value: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        if bytes.len() > FRAME_LIMIT {
            return Err("Native agent frame exceeds 1 MiB".to_owned());
        }
        bytes.push(b'\n');
        if self.closing.load(Ordering::Acquire) {
            return Err("Native agent session is stopped".to_owned());
        }
        lock(&self.stdin)
            .as_mut()
            .ok_or("Native agent stdin is closed")?
            .write_all(&bytes)
            .map_err(|error| error.to_string())
    }

    pub fn stop(&self) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        lock(&self.stdin).take();
        terminate(&self.child);
        let mut snapshot = lock(&self.snapshot);
        snapshot.status = NativeSessionStatus::Stopped;
        snapshot.requests.clear();
        snapshot.changed();
    }
}

impl Drop for NativeAgentSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn with_field(mut value: Value, name: &str, field: &str) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert(name.to_owned(), field.into());
    }
    value
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn terminate(child: &Mutex<Child>) {
    let mut child = lock(child);
    let id = child.id();
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", &format!("-{id}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &id.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[allow(clippy::too_many_lines)] // One bounded read/dispatch loop owns the provider transport.
fn read_frames(
    mut reader: impl BufRead,
    snapshot: &Mutex<NativeSessionSnapshot>,
    pending: &PendingReplies,
    change_handler: &ChangePublisher,
) -> Result<(), String> {
    loop {
        let mut bytes = Vec::new();
        let count = reader
            .by_ref()
            .take(u64::try_from(FRAME_LIMIT + 1).unwrap_or(u64::MAX))
            .read_until(b'\n', &mut bytes)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(());
        }
        if bytes.len() > FRAME_LIMIT {
            return Err("Native agent emitted a frame larger than 1 MiB".to_owned());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid native agent JSON: {error}"))?;
        if !value.is_object() {
            return Err("Native agent emitted a non-object frame".to_owned());
        }
        let provider = lock(snapshot).provider;
        let reply_id = match provider {
            AgentKind::Codex if value.get("method").is_none() => value
                .get("id")
                .map(|id| id.as_str().map_or_else(|| id.to_string(), str::to_owned)),
            AgentKind::Claude if field(&value, "type") == "control_response" => {
                string(field(field(&value, "response"), "request_id"))
            }
            AgentKind::Pi if field(&value, "type") == "response" => string(field(&value, "id")),
            _ => None,
        };
        if let Some(id) = reply_id {
            let sender = lock(pending).remove(&id);
            if let Some(sender) = sender {
                let result = match provider {
                    AgentKind::Codex => {
                        if value.get("error").is_some() {
                            Err(field(&value, "error").to_string())
                        } else {
                            Ok(field(&value, "result").clone())
                        }
                    }
                    AgentKind::Claude => {
                        if field(field(&value, "response"), "subtype") == "error" {
                            Err(field(field(&value, "response"), "error").to_string())
                        } else {
                            Ok(field(field(&value, "response"), "response").clone())
                        }
                    }
                    AgentKind::Pi => {
                        if field(&value, "success") == false {
                            Err(field(&value, "error").to_string())
                        } else {
                            Ok(field(&value, "data").clone())
                        }
                    }
                };
                let _ = sender.send(result);
            }
            continue;
        }
        if provider == AgentKind::Pi
            && field(&value, "type") == "response"
            && field(&value, "success") == false
        {
            let mut snapshot = lock(snapshot);
            snapshot.error = Some(field(&value, "error").to_string());
            snapshot.status = NativeSessionStatus::Error;
            snapshot.changed();
            drop(snapshot);
            publish_change(change_handler);
            continue;
        }
        let request = match provider {
            AgentKind::Codex if value.get("id").is_some() && value.get("method").is_some() => {
                Some(NativeAgentRequest {
                    id: field(&value, "id").to_string(),
                    method: field(&value, "method")
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    parameters: field(&value, "params").clone(),
                })
            }
            AgentKind::Claude if field(&value, "type") == "control_request" => {
                Some(NativeAgentRequest {
                    id: field(&value, "request_id")
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    method: field(field(&value, "request"), "subtype")
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    parameters: field(&value, "request").clone(),
                })
            }
            AgentKind::Pi
                if field(&value, "type") == "extension_ui_request"
                    && matches!(
                        field(&value, "method").as_str(),
                        Some("select" | "confirm" | "input" | "editor")
                    ) =>
            {
                Some(NativeAgentRequest {
                    id: field(&value, "id").as_str().unwrap_or_default().to_owned(),
                    method: field(&value, "method")
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    parameters: value.clone(),
                })
            }
            _ => None,
        };
        let mut snapshot = lock(snapshot);
        if let Some(request) = request {
            if snapshot.requests.len() >= 32 {
                return Err("Too many pending provider requests".to_owned());
            }
            if !snapshot
                .requests
                .iter()
                .any(|pending| pending.id == request.id)
            {
                snapshot.requests.push(request);
            }
            snapshot.status = NativeSessionStatus::Waiting;
            snapshot.changed();
        } else {
            snapshot.ingest(&value);
        }
        drop(snapshot);
        publish_change(change_handler);
    }
}

fn publish_change(publisher: &ChangePublisher) {
    let callback = lock(publisher).clone();
    if let Some(callback) = callback {
        callback();
    }
}

fn native_command(config: &NativeSessionConfig) -> Result<Command, String> {
    AgentLaunch {
        program: config.program.clone(),
        cwd: Some(config.cwd.to_string_lossy().into_owned()),
        arguments: config.arguments.clone(),
        ephemeral: false,
    }
    .validate()?;
    if !config.cwd.is_absolute() {
        return Err("Native agent directory must be absolute".to_owned());
    }
    if config
        .session_id
        .iter()
        .chain(&config.fork_session_id)
        .any(|id| {
            id.is_empty()
                || id.starts_with('-')
                || id.len() > 8192
                || id.chars().any(char::is_control)
        })
    {
        return Err("Invalid native agent session selector".to_owned());
    }
    if config.session_id.is_some() && config.fork_session_id.is_some() {
        return Err("Native session cannot resume and fork simultaneously".to_owned());
    }
    let mut command = Command::new(&config.program);
    command
        .args(&config.arguments)
        .current_dir(&config.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .env_remove("BOOTTY_AGENT_LAUNCH_CONTEXT")
        .env_remove("BOOTTY_CONTROL_ENDPOINT")
        .env("BOOTTY_NATIVE_AGENT", "1");
    match config.provider {
        AgentKind::Codex => {
            command.args(["app-server", "--stdio"]);
        }
        AgentKind::Claude => {
            command.args([
                "--print",
                "--verbose",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--permission-prompt-tool",
                "stdio",
                "--settings",
                "{\"disableAllHooks\":true}",
            ]);
            if let Some(id) = &config.fork_session_id {
                command.args(["--resume", id, "--fork-session"]);
            } else if let Some(id) = &config.session_id {
                command.args(["--resume", id]);
            }
            if let Some(model) = &config.model {
                command.args(["--model", model]);
            }
        }
        AgentKind::Pi => {
            command.args(["--mode", "rpc"]);
            if let Some(id) = &config.fork_session_id {
                command.args(["--fork", id]);
            } else if let Some(id) = &config.session_id {
                command.args(["--session", id]);
            }
            if let Some(model) = &config.model {
                command.args(["--model", model]);
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    Ok(command)
}
