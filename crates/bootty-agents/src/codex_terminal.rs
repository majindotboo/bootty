//! Passive observation of the real terminal client's app-server connection.

use std::collections::BTreeSet;
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::AgentLaunch;
use crate::terminal_observation::{AgentObservation, ObservationSink, TerminalAgentStatus};

const MAX_MESSAGE: usize = 1024 * 1024;
const MAX_PENDING: usize = 64;
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(50);
#[cfg(unix)]
static NEXT_ENDPOINT: AtomicU64 = AtomicU64::new(0);

/// Owns one provider process and a private endpoint for its real terminal client.
pub struct CodexTerminalObserver {
    arguments: Vec<String>,
    stopping: Arc<AtomicBool>,
}

impl CodexTerminalObserver {
    /// Prepare on a background thread. Existing external endpoints are not adopted.
    ///
    /// # Errors
    /// Returns launch, endpoint, process, or unsupported-platform errors.
    pub fn prepare(
        launch: &AgentLaunch,
        runtime_directory: &Path,
        sink: ObservationSink,
    ) -> io::Result<Self> {
        launch
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        if launch
            .arguments
            .iter()
            .any(|arg| arg == "--remote" || arg.starts_with("--remote="))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Observation cannot adopt an external Codex endpoint",
            ));
        }
        #[cfg(unix)]
        {
            Self::prepare_unix(launch, runtime_directory, sink)
        }
        #[cfg(not(unix))]
        {
            let _ = (runtime_directory, sink);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Codex terminal observation requires a private Unix transport",
            ))
        }
    }

    #[must_use]
    pub fn arguments(&self) -> Vec<String> {
        self.arguments.clone()
    }

    /// Cancellation is nonblocking; the transport worker reaps its owned process.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    #[cfg(unix)]
    fn prepare_unix(
        launch: &AgentLaunch,
        runtime_directory: &Path,
        sink: ObservationSink,
    ) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        use std::os::unix::net::UnixListener;

        let _ = runtime_directory;
        // Socket paths have small platform limits; the caller state tree may be long.
        let directory = std::env::temp_dir().join(format!(
            "codex-{}-{}",
            std::process::id(),
            NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let endpoint = directory.join("tui.sock");
        let provider_endpoint = directory.join("provider.sock");
        let setup = (|| {
            let listener = UnixListener::bind(&endpoint)?;
            listener.set_nonblocking(true)?;
            let mut command = Command::new(&launch.program);
            command
                .args(["app-server", "--listen"])
                .arg(format!("unix://{}", provider_endpoint.display()))
                .args(provider_arguments(&launch.arguments))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(cwd) = &launch.cwd {
                command.current_dir(cwd);
            }
            let child = command.spawn()?;
            Ok::<_, io::Error>((listener, child))
        })();
        let (listener, child) = match setup {
            Ok(value) => value,
            Err(error) => {
                cleanup(&directory);
                return Err(error);
            }
        };
        let provider = OwnedProvider { child, directory };
        let mut arguments = vec![
            "--remote".to_owned(),
            format!("unix://{}", endpoint.display()),
        ];
        arguments.extend(launch.arguments.clone());
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stop = stopping.clone();
        thread::spawn(move || {
            run_provider(provider, listener, &provider_endpoint, &worker_stop, &sink);
        });
        Ok(Self {
            arguments,
            stopping,
        })
    }
}

#[cfg(unix)]
fn run_provider(
    mut provider: OwnedProvider,
    listener: std::os::unix::net::UnixListener,
    provider_endpoint: &Path,
    worker_stop: &Arc<AtomicBool>,
    sink: &ObservationSink,
) {
    let protocol = Arc::new(Mutex::new(CodexTerminalProtocol::default()));
    let disconnected = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let result = (|| {
        let Some(terminal) = accept_terminal(&listener, &mut provider.child, worker_stop)? else {
            return Ok(());
        };
        // One launch owns one connection. Reconnect needs an explicit new launch.
        drop(listener);
        let Some(provider_stream) =
            connect_provider(provider_endpoint, &mut provider.child, worker_stop, started)?
        else {
            return Ok(());
        };
        for stream in [&terminal, &provider_stream] {
            // Accepted sockets inherit O_NONBLOCK on macOS. Timeouts do not clear it.
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(POLL))?;
            stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        }
        let terminal_copy = terminal.try_clone()?;
        let provider_copy = provider_stream.try_clone()?;
        let inbound_protocol = protocol.clone();
        let inbound_stop = worker_stop.clone();
        let inbound_disconnected = disconnected.clone();
        let inbound_sink = sink.clone();
        let inbound = thread::spawn(move || {
            relay(
                terminal_copy,
                provider_copy,
                true,
                &inbound_protocol,
                &inbound_stop,
                &inbound_disconnected,
                &inbound_sink,
            )
        });
        let outbound = relay(
            provider_stream,
            terminal,
            false,
            &protocol,
            worker_stop,
            &disconnected,
            sink,
        );
        disconnected.store(true, Ordering::Release);
        let inbound = inbound
            .join()
            .map_err(|_| io::Error::other("Codex transport worker panicked"))?;
        outbound.and(inbound)
    })();
    let cancelled = worker_stop.load(Ordering::Acquire);
    let _ = provider.child.kill();
    let _ = provider.child.wait();
    let status = if result.is_err() && !cancelled {
        TerminalAgentStatus::Unavailable
    } else {
        TerminalAgentStatus::Stopped
    };
    if let Ok(protocol) = protocol.lock() {
        sink(protocol.observation(status, result.err().map(|error| error.to_string())));
    }
}

#[cfg(unix)]
fn accept_terminal(
    listener: &std::os::unix::net::UnixListener,
    child: &mut Child,
    stopping: &AtomicBool,
) -> io::Result<Option<UnixStream>> {
    loop {
        if stopping.load(Ordering::Acquire) {
            return Ok(None);
        }
        if child.try_wait()?.is_some() {
            return Err(io::Error::other(
                "Codex app-server exited before its terminal connected",
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(Some(stream)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn connect_provider(
    endpoint: &Path,
    child: &mut Child,
    stopping: &AtomicBool,
    started: Instant,
) -> io::Result<Option<UnixStream>> {
    loop {
        if stopping.load(Ordering::Acquire) {
            return Ok(None);
        }
        if child.try_wait()?.is_some() {
            return Err(io::Error::other("Codex app-server exited during startup"));
        }
        match UnixStream::connect(endpoint) {
            Ok(stream) => return Ok(Some(stream)),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) && started.elapsed() < Duration::from_secs(10) =>
            {
                thread::sleep(POLL);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
struct OwnedProvider {
    child: Child,
    directory: PathBuf,
}

#[cfg(unix)]
impl Drop for OwnedProvider {
    fn drop(&mut self) {
        // Keep ownership on early returns, worker panics, and failed thread startup.
        let _ = self.child.kill();
        let _ = self.child.wait();
        cleanup(&self.directory);
    }
}

#[cfg(unix)]
fn provider_arguments(arguments: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            break;
        }
        let (key, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(key, value)| (key, Some(value)));
        let config_key = match key {
            "--profile" | "-p" => Some("profile"),
            "--model" | "-m" => Some("model"),
            "--sandbox" | "-s" => Some("sandbox_mode"),
            "--ask-for-approval" | "-a" => Some("approval_policy"),
            _ => None,
        };
        if matches!(key, "--config" | "-c" | "--enable" | "--disable") {
            if let Some(value) = inline.or_else(|| arguments.next().map(String::as_str)) {
                result.extend([key.to_owned(), value.to_owned()]);
            }
        } else if let Some(config_key) = config_key {
            if let Some(value) = inline.or_else(|| arguments.next().map(String::as_str)) {
                // JSON string quoting is also valid TOML basic-string quoting for
                // validated launch values. These overrides never enter storage.
                result.extend([
                    "--config".to_owned(),
                    format!("{config_key}={}", Value::String(value.to_owned())),
                ]);
            }
        } else if argument
            .strip_prefix("-c")
            .is_some_and(|value| !value.is_empty())
        {
            result.extend([
                "--config".to_owned(),
                argument.strip_prefix("-c").unwrap_or_default().to_owned(),
            ]);
        }
    }
    result
}

impl Drop for CodexTerminalObserver {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
fn cleanup(directory: &Path) {
    // Only this launch's two owned entries are removed, never arbitrary contents.
    let _ = std::fs::remove_file(directory.join("tui.sock"));
    let _ = std::fs::remove_file(directory.join("provider.sock"));
    let _ = std::fs::remove_dir(directory);
}

#[cfg(unix)]
fn relay(
    mut source: std::os::unix::net::UnixStream,
    mut destination: std::os::unix::net::UnixStream,
    client: bool,
    protocol: &Mutex<CodexTerminalProtocol>,
    stopping: &AtomicBool,
    disconnected: &AtomicBool,
    sink: &ObservationSink,
) -> io::Result<()> {
    let mut bytes = [0; 8192];
    let result = (|| {
        while !stopping.load(Ordering::Acquire) && !disconnected.load(Ordering::Acquire) {
            let count = match source.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("Codex transport read: {error}"),
                    ));
                }
            };
            // Parse before forwarding so a fast response cannot overtake correlation.
            let bytes = bytes
                .get(..count)
                .ok_or_else(|| io::Error::other("Invalid Codex transport read length"))?;
            let observations = {
                let mut protocol = protocol
                    .lock()
                    .map_err(|_| io::Error::other("Codex observation lock poisoned"))?;
                if client {
                    protocol.observe_client(bytes)
                } else {
                    protocol.observe_server(bytes)
                }
            };
            for observation in observations {
                sink(observation);
            }
            write_transparent(&mut destination, bytes, stopping, disconnected)?;
        }
        Ok(())
    })();
    disconnected.store(true, Ordering::Release);
    let _ = source.shutdown(std::net::Shutdown::Both);
    let _ = destination.shutdown(std::net::Shutdown::Both);
    result
}

#[cfg(unix)]
fn write_transparent(
    destination: &mut UnixStream,
    mut bytes: &[u8],
    stopping: &AtomicBool,
    disconnected: &AtomicBool,
) -> io::Result<()> {
    while !bytes.is_empty()
        && !stopping.load(Ordering::Acquire)
        && !disconnected.load(Ordering::Acquire)
    {
        match destination.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "Codex transport write closed",
                ));
            }
            Ok(count) => {
                bytes = bytes
                    .get(count..)
                    .ok_or_else(|| io::Error::other("Invalid Codex transport write length"))?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                thread::yield_now();
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("Codex transport write: {error}"),
                ));
            }
        }
    }
    Ok(())
}

/// Bounded, passive observer for a pair of raw `WebSocket` streams, including their
/// HTTP upgrade. It never edits transport bytes or answers provider requests.
#[derive(Default)]
pub struct CodexTerminalProtocol {
    client: WebSocketObserver,
    server: WebSocketObserver,
    pending: BTreeSet<String>,
    initializing: Option<String>,
    initialize_succeeded: bool,
    session_id: Option<String>,
}

impl CodexTerminalProtocol {
    #[must_use]
    pub fn observe_client(&mut self, bytes: &[u8]) -> Vec<AgentObservation> {
        let mut observations = Vec::new();
        if self.server.disabled {
            return observations;
        }
        let was_disabled = self.client.disabled;
        for observed in self.client.push(bytes, true) {
            let ObservedMessage::Json(message) = observed else {
                observations.push(self.observation(
                    TerminalAgentStatus::Unavailable,
                    Some("Codex request exceeded the observation limit".to_owned()),
                ));
                continue;
            };
            match message.get("method").and_then(Value::as_str) {
                Some("initialize") => {
                    self.initializing = message.get("id").and_then(request_id);
                    self.initialize_succeeded = false;
                }
                Some("initialized")
                    if std::mem::take(&mut self.initialize_succeeded)
                        && self.session_id.is_none() =>
                {
                    observations.push(self.observation(TerminalAgentStatus::Idle, None));
                }
                _ => {}
            }
            if matches!(
                message.get("method").and_then(Value::as_str),
                Some("thread/start" | "thread/resume" | "thread/fork")
            ) && let Some(id) = message.get("id").and_then(request_id)
            {
                if self.pending.len() < MAX_PENDING {
                    self.pending.insert(id);
                } else {
                    observations.push(self.observation(
                        TerminalAgentStatus::Unavailable,
                        Some("Too many pending Codex thread requests".to_owned()),
                    ));
                }
            }
        }
        if self.client.disabled && !was_disabled {
            observations.push(self.observation(
                TerminalAgentStatus::Unavailable,
                Some("Codex request framing is not inspectable".to_owned()),
            ));
        }
        observations
    }

    #[must_use]
    pub fn observe_server(&mut self, bytes: &[u8]) -> Vec<AgentObservation> {
        let mut observations = Vec::new();
        if self.client.disabled {
            return observations;
        }
        let was_disabled = self.server.disabled;
        for observed in self.server.push(bytes, false) {
            let ObservedMessage::Json(message) = observed else {
                observations.push(self.observation(
                    TerminalAgentStatus::Unavailable,
                    Some("Codex response exceeded the observation limit".to_owned()),
                ));
                continue;
            };
            if self.observe_initialize_reply(&message) {
                continue;
            }
            if (message.get("result").is_some() || message.get("error").is_some())
                && let Some(id) = message.get("id").and_then(request_id)
                && self.pending.remove(&id)
            {
                if let Some(thread) = message.pointer("/result/thread")
                    && let Some(id) = thread
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty() && id.len() <= 8192)
                {
                    self.session_id = Some(id.to_owned());
                    observations.push(
                        self.observation(
                            thread
                                .get("status")
                                .map_or(TerminalAgentStatus::Idle, thread_status),
                            None,
                        ),
                    );
                } else if let Some(error) = message.get("error") {
                    let detail = error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(|message| message.chars().take(1024).collect());
                    observations.push(self.observation(TerminalAgentStatus::Error, detail));
                }
                continue;
            }
            let Some(params) = message.get("params") else {
                continue;
            };
            if params.get("threadId").and_then(Value::as_str) != self.session_id.as_deref()
                || self.session_id.is_none()
            {
                continue;
            }
            let status = match message.get("method").and_then(Value::as_str) {
                Some("thread/status/changed") => params.get("status").map(thread_status),
                Some("turn/started") => Some(TerminalAgentStatus::Working),
                Some("turn/completed") => Some(
                    match params.pointer("/turn/status").and_then(Value::as_str) {
                        Some("completed") => TerminalAgentStatus::Finished,
                        Some("interrupted") => TerminalAgentStatus::Stopped,
                        Some("failed") => TerminalAgentStatus::Error,
                        _ => TerminalAgentStatus::Unavailable,
                    },
                ),
                _ => None,
            };
            if let Some(status) = status {
                observations.push(self.observation(status, None));
            }
        }
        if self.server.disabled && !was_disabled {
            observations.push(self.observation(
                TerminalAgentStatus::Unavailable,
                Some("Codex response framing is not inspectable".to_owned()),
            ));
        }
        observations
    }

    fn observe_initialize_reply(&mut self, message: &Value) -> bool {
        let matches = message
            .get("id")
            .and_then(request_id)
            .as_ref()
            .is_some_and(|id| Some(id) == self.initializing.as_ref());
        if !matches || (message.get("result").is_none() && message.get("error").is_none()) {
            return false;
        }
        self.initializing = None;
        self.initialize_succeeded =
            message.get("result").is_some_and(Value::is_object) && message.get("error").is_none();
        true
    }

    fn observation(&self, status: TerminalAgentStatus, detail: Option<String>) -> AgentObservation {
        AgentObservation {
            session_id: self.session_id.clone(),
            session_file: None,
            status,
            detail,
        }
    }
}

fn request_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if id.len() <= 8192 => Some(format!("s:{id}")),
        Value::Number(id) => Some(format!("n:{id}")),
        _ => None,
    }
}

fn thread_status(value: &Value) -> TerminalAgentStatus {
    match value.get("type").and_then(Value::as_str) {
        Some("idle") => TerminalAgentStatus::Idle,
        Some("systemError") => TerminalAgentStatus::Error,
        Some("active") => {
            if value
                .get("activeFlags")
                .and_then(Value::as_array)
                .is_some_and(|flags| {
                    flags.iter().any(|flag| {
                        matches!(
                            flag.as_str(),
                            Some("waitingOnApproval" | "waitingOnUserInput")
                        )
                    })
                })
            {
                TerminalAgentStatus::Waiting
            } else {
                TerminalAgentStatus::Working
            }
        }
        _ => TerminalAgentStatus::Unavailable,
    }
}

enum ObservedMessage {
    Json(Value),
    Skipped,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum MessageState {
    #[default]
    Empty,
    Text,
    Discarded,
}

#[derive(Default)]
struct WebSocketObserver {
    upgraded: bool,
    disabled: bool,
    header: Vec<u8>,
    frame: Option<ObservedFrame>,
    message: Vec<u8>,
    message_state: MessageState,
}

struct ObservedFrame {
    opcode: u8,
    final_frame: bool,
    remaining: u64,
    mask: Option<[u8; 4]>,
    mask_offset: usize,
}

fn decode_header(buffer: &[u8], client: bool) -> Result<Option<ObservedFrame>, ()> {
    let Some(&[first, second]) = buffer.first_chunk::<2>() else {
        return Ok(None);
    };
    let opcode = first & 15;
    let final_frame = first & 128 != 0;
    let masked = second & 128 != 0;
    if first & 0x70 != 0
        || masked != client
        || !matches!(opcode, 0 | 1 | 2 | 8 | 9 | 10)
        || (opcode >= 8 && (!final_frame || second & 127 > 125))
    {
        return Err(());
    }
    let (length, extended) = match second & 127 {
        126 => {
            let Some(bytes) = buffer
                .get(2..4)
                .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            else {
                return Ok(None);
            };
            let length = u16::from_be_bytes(bytes);
            if length < 126 {
                return Err(());
            }
            (u64::from(length), 2_usize)
        }
        127 => {
            let Some(bytes) = buffer
                .get(2..10)
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
            else {
                return Ok(None);
            };
            let length = u64::from_be_bytes(bytes);
            // RFC 6455 lengths are unsigned 63-bit values, never a saturation sentinel.
            if length < 65536 || length > i64::MAX.unsigned_abs() {
                return Err(());
            }
            (length, 8_usize)
        }
        length => (u64::from(length), 0_usize),
    };
    let offset = 2_usize.saturating_add(extended);
    let mask = if masked {
        let Some(mask) = buffer
            .get(offset..offset.saturating_add(4))
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        else {
            return Ok(None);
        };
        Some(mask)
    } else {
        None
    };
    Ok(Some(ObservedFrame {
        opcode,
        final_frame,
        remaining: length,
        mask,
        mask_offset: 0,
    }))
}

impl WebSocketObserver {
    fn push(&mut self, mut bytes: &[u8], client: bool) -> Vec<ObservedMessage> {
        let mut values = Vec::new();
        while !self.disabled {
            if let Some(mut frame) = self.frame.take() {
                let count = usize::try_from(frame.remaining)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                let Some((payload, rest)) = bytes.split_at_checked(count) else {
                    self.disable();
                    break;
                };
                bytes = rest;
                self.consume_payload(&mut frame, payload);
                if frame.remaining == 0 {
                    self.finish_frame(&frame, &mut values);
                } else {
                    self.frame = Some(frame);
                    if bytes.is_empty() {
                        break;
                    }
                }
                continue;
            }
            let Some((&byte, rest)) = bytes.split_first() else {
                break;
            };
            bytes = rest;
            self.header.push(byte);
            if !self.upgraded {
                self.upgrade(client);
                continue;
            }
            match decode_header(&self.header, client) {
                Ok(Some(frame)) => {
                    self.header.clear();
                    if self.begin_frame(&frame, &mut values).is_err() {
                        self.disable();
                        break;
                    }
                    self.frame = Some(frame);
                }
                Ok(None) => {}
                Err(()) => self.disable(),
            }
        }
        values
    }

    fn upgrade(&mut self, client: bool) {
        if self.header.ends_with(b"\r\n\r\n") {
            let line = self
                .header
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap_or_default();
            let valid = if client {
                line.starts_with(b"GET ") && line.ends_with(b" HTTP/1.1\r")
            } else {
                line.starts_with(b"HTTP/1.1 101 ")
            };
            if valid {
                self.upgraded = true;
                self.header.clear();
            } else {
                self.disable();
            }
        } else if self.header.len() >= 16 * 1024 {
            self.disable();
        }
    }

    fn begin_frame(
        &mut self,
        frame: &ObservedFrame,
        values: &mut Vec<ObservedMessage>,
    ) -> Result<(), ()> {
        if frame.opcode >= 8 {
            return Ok(());
        }
        if (frame.opcode == 0) != (self.message_state != MessageState::Empty) {
            return Err(());
        }
        if frame.opcode != 0 {
            self.message_state = if frame.opcode == 1 {
                MessageState::Text
            } else {
                MessageState::Discarded
            };
        }
        let available = MAX_MESSAGE.saturating_sub(self.message.len());
        if self.message_state == MessageState::Text
            && frame.remaining > u64::try_from(available).map_err(|_| ())?
        {
            self.message.clear();
            self.message_state = MessageState::Discarded;
            values.push(ObservedMessage::Skipped);
        }
        Ok(())
    }

    fn consume_payload(&mut self, frame: &mut ObservedFrame, payload: &[u8]) {
        if frame.opcode < 8 && self.message_state == MessageState::Text {
            if let Some(mask) = frame.mask {
                self.message.extend(
                    payload
                        .iter()
                        .zip(mask.iter().cycle().skip(frame.mask_offset))
                        .map(|(byte, mask)| byte ^ mask),
                );
            } else {
                self.message.extend_from_slice(payload);
            }
        }
        frame.remaining = frame
            .remaining
            .saturating_sub(u64::try_from(payload.len()).unwrap_or(u64::MAX));
        frame.mask_offset = frame.mask_offset.saturating_add(payload.len()) % 4;
    }

    fn finish_frame(&mut self, frame: &ObservedFrame, values: &mut Vec<ObservedMessage>) {
        if frame.opcode < 8 && frame.final_frame {
            if self.message_state == MessageState::Text
                && let Ok(value) = serde_json::from_slice(&self.message)
            {
                values.push(ObservedMessage::Json(value));
            }
            self.message.clear();
            self.message_state = MessageState::Empty;
        }
    }

    fn disable(&mut self) {
        // Invalid framing cannot be resynchronized safely. Transport remains transparent.
        self.disabled = true;
        self.header.clear();
        self.frame = None;
        self.message.clear();
    }
}
