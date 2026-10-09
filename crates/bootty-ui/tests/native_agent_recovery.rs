#![cfg(unix)]

use bootty_agents::{AgentKind, TerminalAgentStatus, ToolCapture};
use bootty_config::config::{AgentProfileConfig, MultiplexerBackendConfig};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget,
};
use bootty_ui::{AppState, recovery::OutputArchive};
use pretty_assertions::{assert_eq, assert_ne};
use rstest::{fixture, rstest};
use std::{
    fs,
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[path = "support/idle_frames.rs"]
mod frames;
#[allow(
    clippy::expect_used,
    reason = "Shared desktop backend fixture follows existing test convention"
)]
mod support;
#[path = "support/config.rs"]
mod test_config;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Host {
    directory: assert_fs::TempDir,
    state: AppState,
    wakes: mpsc::Receiver<()>,
    session: CommandTarget,
    terminal: CommandTarget,
    _gate: fs::File,
}
impl Host {
    fn close(&mut self) -> TestResult<()> {
        let service = self
            .state
            .terminal_agent_service()
            .ok_or("agent owner missing")?;
        let live = service.live_records();
        service.shutdown_and_wait()?;
        for record in live {
            let mut close = CommandInvocation::new("pane.close", Vec::new(), Caller::Socket);
            close.target = Some(record.target);
            close.confirmation = Some(close.confirmation());
            success(submit(&mut self.state, &self.wakes, close)?)?;
        }
        for archive in self.state.recovery_archives().iter() {
            let mut delete =
                CommandInvocation::new("recovery.delete", vec![archive.id.clone()], Caller::Socket);
            let outcome = submit(&mut self.state, &self.wakes, delete.clone())?;
            if let CommandOutcome::ConfirmationRequired { confirmation } = outcome {
                delete.confirmation = Some(*confirmation);
                success(submit(&mut self.state, &self.wakes, delete)?)?;
            } else {
                return Err(
                    format!("fixture deletion requires host confirmation: {outcome:?}").into(),
                );
            }
        }
        Ok(())
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
fn success(outcome: CommandOutcome) -> TestResult<serde_json::Value> {
    if let CommandOutcome::Success { value, .. } = outcome {
        Ok(value)
    } else {
        Err(format!("shared command failed: {outcome:?}").into())
    }
}
fn target(value: &serde_json::Value, field: &str) -> TestResult<CommandTarget> {
    Ok(serde_json::from_value(
        value.get(field).cloned().ok_or("issued target missing")?,
    )?)
}
fn submit(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    invocation: CommandInvocation,
) -> TestResult<CommandOutcome> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or("deadline overflow")?;
    let response = state
        .app_command_sender(invocation.caller)
        .submit(invocation, deadline, CommandCancellation::new())
        .map_err(|error| format!("command submission failed: {error:?}"))?;
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        match response.try_recv() {
            Ok(outcome) => return Ok(outcome),
            Err(mpsc::TryRecvError::Disconnected) => return Err("command disconnected".into()),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        wakes.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
}
fn provider(directory: &Path) -> TestResult<(std::path::PathBuf, fs::File)> {
    use std::os::unix::fs::PermissionsExt as _;
    let gate = directory.join("query-gate");
    if !std::process::Command::new("mkfifo")
        .arg(&gate)
        .status()?
        .success()
    {
        return Err("query FIFO creation failed".into());
    }
    let gate = fs::OpenOptions::new().read(true).write(true).open(gate)?;
    let program = directory.join("claude-fixture.py");
    fs::write(
        &program,
        r"#!/usr/bin/env python3
import json, os, pathlib, sys
directory = pathlib.Path(__file__).parent
if sys.argv[1:3] == ['agents', '--json']:
    with open(directory / 'query-gate', 'rb', buffering=0) as gate:
        gate.read(1)
    paths = list(directory.glob('session-*.json'))
    print(json.dumps([{'sessionId': json.loads(path.read_text())['session_id'], 'status': 'idle'} for path in paths]))
    sys.exit(0)
arguments = sys.argv[1:]
session_id = arguments[arguments.index('--session-id') + 1] if '--session-id' in arguments else arguments[arguments.index('--resume') + 1]
clean = []
tool_configs = 0
index = 0
while index < len(arguments):
    if arguments[index] == '--mcp-config':
        tool_configs += 1
        index += 2
    else:
        clean.append(arguments[index])
        index += 1
path = directory / ('session-' + session_id + '.json')
temporary = path.with_suffix('.tmp')
temporary.write_text(json.dumps({'session_id': session_id, 'arguments': clean, 'account': os.environ.get('CLAUDE_CONFIG_DIR'), 'cwd': os.getcwd(), 'tool_configs': tool_configs}))
os.replace(temporary, path)
with open(directory / 'query-gate', 'wb', buffering=0) as gate:
    gate.write(b'x')
print('provider ready', flush=True)
for line in sys.stdin:
    with open(directory / 'received', 'a') as received:
        received.write(json.dumps(line.rstrip('\n')) + '\n')
    print('received', flush=True)
",
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    Ok((program, gate))
}
#[fixture]
fn host() -> TestResult<Host> {
    let directory = assert_fs::TempDir::new()?;
    let (program, gate) = provider(directory.path())?;
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.output_archives = true;
    config.agents.allow_spawn = true;
    config.agents.claude.program = directory
        .path()
        .join("current-program-missing")
        .to_string_lossy()
        .into_owned();
    "current".clone_into(&mut config.agents.claude.selected);
    for profile in ["captured", "current"] {
        config.agents.claude.profiles.insert(
            profile.to_owned(),
            AgentProfileConfig {
                name: profile.to_owned(),
                directory: Some(
                    directory
                        .path()
                        .join(profile)
                        .to_string_lossy()
                        .into_owned(),
                ),
                arguments: vec!["--model".to_owned(), format!("{profile}-model")],
            },
        );
    }
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    let mut state = AppState::new_for_window_with_agents(
        config,
        format!("recovery-{}", directory.path().display()),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )?;
    let binding = target(
        &success(submit(
            &mut state,
            &wakes,
            CommandInvocation::new(
                "resource.current",
                vec!["binding".to_owned()],
                Caller::Socket,
            ),
        )?)?,
        "target",
    )?;
    let mut create = CommandInvocation::new(
        "session.create",
        vec![
            "original".to_owned(),
            directory.path().to_string_lossy().into_owned(),
        ],
        Caller::Socket,
    );
    create.target = Some(binding);
    let session = target(&success(submit(&mut state, &wakes, create)?)?, "created")?;
    let mut start = CommandInvocation::new(
        "agents.claude.start",
        vec![
            directory.path().to_string_lossy().into_owned(),
            program.to_string_lossy().into_owned(),
            serde_json::to_string(&["--model", "captured-model"])?,
            String::new(),
            "captured".to_owned(),
        ],
        Caller::Internal,
    );
    start.target = Some(session.clone());
    let terminal = target(&success(submit(&mut state, &wakes, start)?)?, "terminal")?;
    Ok(Host {
        directory,
        state,
        wakes,
        session,
        terminal,
        _gate: gate,
    })
}
fn wait_observed(host: &mut Host, terminal: &CommandTarget) -> TestResult<()> {
    let service = host
        .state
        .terminal_agent_service()
        .ok_or("agent owner missing")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or("deadline overflow")?;
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        if service.live_records().iter().any(|record| {
            record.target == *terminal && record.observation.status == TerminalAgentStatus::Idle
        }) {
            return Ok(());
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
}
fn checkpoint(host: &mut Host) -> TestResult<OutputArchive> {
    wait_observed(host, &host.terminal.clone())?;
    let tick = Instant::now()
        .checked_add(Duration::from_secs(31))
        .ok_or("checkpoint tick overflow")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or("deadline overflow")?;
    loop {
        host.state.update_frame(frames::idle_frame(tick));
        if let Some(archive) = host
            .state
            .recovery_archives()
            .iter()
            .find(|archive| archive.agent.is_some())
        {
            return Ok(archive.clone());
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
}
fn close_original_turn(host: &mut Host) -> TestResult<()> {
    let mut close = CommandInvocation::new("agents.claude.stop", Vec::new(), Caller::Socket);
    close.target = Some(host.terminal.clone());
    close.confirmation = Some(close.confirmation());
    success(submit(&mut host.state, &host.wakes, close)?)?;
    Ok(())
}
#[rstest]
#[case::captured_account(false)]
#[case::changed_account(true)]
fn history_resume_reuses_the_exact_live_terminal_only_after_account_validation(
    host: TestResult<Host>,
    #[case] changed_account: bool,
) -> TestResult<()> {
    let mut host = host?;
    let terminal = host.terminal.clone();
    wait_observed(&mut host, &terminal)?;
    let service = host.state.terminal_agent_service().ok_or("agent owner")?;
    let (source, prior_tools) = service.spawn_parent(&terminal).ok_or("live native tools")?;
    let topology = host.state.mux().all_sessions().to_vec();
    let scope = host.state.mux_scope();
    let mut resume = CommandInvocation::new(
        "agents.claude.resume",
        vec![
            source
                .observation
                .session_id
                .clone()
                .ok_or("observed session")?,
            source.launch.cwd.clone().ok_or("captured cwd")?,
            source.launch.program.clone(),
            serde_json::to_string(&source.launch.arguments)?,
            if changed_account {
                "current"
            } else {
                "captured"
            }
            .to_owned(),
            source
                .launch
                .account_directory
                .clone()
                .ok_or("captured account")?,
        ],
        Caller::Socket,
    );
    resume.target = Some(host.session.clone());
    let outcome = submit(&mut host.state, &host.wakes, resume)?;
    if changed_account {
        if !matches!(&outcome, CommandOutcome::Failed { message, .. } if message.contains("account changed"))
        {
            return Err(format!("Changed history account was accepted: {outcome:?}").into());
        }
    } else {
        let value = success(outcome)?;
        assert_eq!(target(&value, "terminal")?, terminal);
        assert_eq!(value["reused"], true);
    }
    assert_eq!(host.state.mux().all_sessions(), topology);
    assert_eq!(host.state.mux_scope(), scope);
    assert_eq!(service.records().len(), 1);
    assert_eq!(service.live_records().len(), 1);
    let (retained, tools) = service
        .spawn_parent(&terminal)
        .ok_or("retained native tools")?;
    assert_eq!(retained.target, source.target);
    assert_eq!(retained.launch, source.launch);
    assert_eq!(
        retained.observation.session_id,
        source.observation.session_id
    );
    assert_eq!(tools.caller(), prior_tools.caller());
    assert_eq!(tools.spawn_enabled(), prior_tools.spawn_enabled());
    assert_eq!(tools.enabled(None), prior_tools.enabled(None));
    for capture in [ToolCapture::Computer, ToolCapture::Browser] {
        assert_eq!(
            tools.enabled(Some(capture)),
            prior_tools.enabled(Some(capture))
        );
    }
    host.close()?;
    Ok(())
}

#[rstest]
#[case("resume", Caller::Cli)]
#[case("fork", Caller::Socket)]
fn explicit_recovery_keeps_native_observation_account_and_original_caller(
    host: TestResult<Host>,
    #[case] operation: &str,
    #[case] caller: Caller,
) {
    let mut host = host.unwrap();
    let archive = checkpoint(&mut host).unwrap();
    let captured = archive.agent.as_ref().unwrap();
    assert_eq!(captured.provider, AgentKind::Claude);
    assert_eq!(
        captured.launch.account_directory.as_deref(),
        host.directory.path().join("captured").to_str()
    );
    assert_ne!(
        captured.launch.account_directory.as_deref(),
        host.state
            .config()
            .agents
            .claude
            .selected_profile()
            .unwrap()
            .directory
            .as_deref()
    );
    close_original_turn(&mut host).unwrap();
    let before = host.state.mux().selected_session().map(str::to_owned);
    let value = success(
        submit(
            &mut host.state,
            &host.wakes,
            CommandInvocation::new(
                format!("recovery.{operation}"),
                vec![archive.id.clone()],
                caller,
            ),
        )
        .unwrap(),
    )
    .unwrap();
    let terminal = target(&value, "terminal").unwrap();
    wait_observed(&mut host, &terminal).unwrap();
    let service = host.state.terminal_agent_service().unwrap();
    let record = service.record(&terminal).unwrap();
    assert_eq!(record.launch, captured.launch);
    assert_eq!(
        record.observation.session_id.as_deref() == Some(captured.session.as_str()),
        operation == "resume"
    );
    assert_eq!(host.state.mux().all_sessions().len(), 1);
    assert_eq!(host.state.mux().all_sessions()[0].id, archive.session);
    let (_, lease) = service
        .spawn_parent(&terminal)
        .expect("recovery attaches bounded native tools");
    assert_eq!(lease.caller(), caller);
    assert!(lease.enabled(None));
    assert!(!lease.spawn_enabled());
    assert!(!lease.enabled(Some(ToolCapture::Browser)));
    assert!(!lease.enabled(Some(ToolCapture::Computer)));
    assert_eq!(
        host.state.mux().selected_session().map(str::to_owned),
        before
    );
    let observed: serde_json::Value = serde_json::from_slice(
        &fs::read(host.directory.path().join(format!(
            "session-{}.json",
            record.observation.session_id.unwrap()
        )))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        observed.get("account").and_then(serde_json::Value::as_str),
        captured.launch.account_directory.as_deref()
    );
    assert_eq!(
        observed
            .get("tool_configs")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
    let arguments = observed
        .get("arguments")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert!(arguments.windows(2).any(|pair| pair
        == [
            serde_json::json!("--resume"),
            serde_json::json!(captured.session)
        ]));
    assert_eq!(
        arguments
            .iter()
            .any(|argument| argument == "--fork-session"),
        operation == "fork"
    );
    host.close().unwrap();
}
#[rstest]
fn missing_original_destination_cannot_launch_into_another_session(host: TestResult<Host>) {
    let mut host = host.unwrap();
    let archive = checkpoint(&mut host).unwrap();
    close_original_turn(&mut host).unwrap();
    let mut close = CommandInvocation::new("session.close", Vec::new(), Caller::Socket);
    close.target = Some(host.session.clone());
    close.confirmation = Some(close.confirmation());
    success(submit(&mut host.state, &host.wakes, close).unwrap()).unwrap();
    let binding = target(
        &success(
            submit(
                &mut host.state,
                &host.wakes,
                CommandInvocation::new(
                    "resource.current",
                    vec!["binding".to_owned()],
                    Caller::Socket,
                ),
            )
            .unwrap(),
        )
        .unwrap(),
        "target",
    )
    .unwrap();
    let mut create = CommandInvocation::new(
        "session.create",
        vec![
            "other".to_owned(),
            host.directory.path().to_string_lossy().into_owned(),
        ],
        Caller::Socket,
    );
    create.target = Some(binding);
    success(submit(&mut host.state, &host.wakes, create).unwrap()).unwrap();
    let before = host.state.mux().all_sessions().to_vec();
    for operation in ["resume", "fork"] {
        let outcome = submit(
            &mut host.state,
            &host.wakes,
            CommandInvocation::new(
                format!("recovery.{operation}"),
                vec![archive.id.clone()],
                Caller::Socket,
            ),
        )
        .unwrap();
        assert!(
            matches!(outcome, CommandOutcome::Unavailable { .. }),
            "{outcome:?}"
        );
        assert_eq!(host.state.mux().all_sessions(), before);
        assert_eq!(
            host.state
                .terminal_agent_service()
                .unwrap()
                .live_records()
                .len(),
            0
        );
    }
    host.close().unwrap();
}

fn interrupt_provider(directory: &Path) -> TestResult<(std::path::PathBuf, fs::File)> {
    use std::os::unix::fs::PermissionsExt as _;
    let gate = directory.join("raw-query-gate");
    if !std::process::Command::new("mkfifo")
        .arg(&gate)
        .status()?
        .success()
    {
        return Err("control query FIFO creation failed".into());
    }
    let gate = fs::OpenOptions::new().read(true).write(true).open(gate)?;
    let program = directory.join("interrupt-provider.py");
    fs::write(
        &program,
        r"#!/usr/bin/env python3
import json, os, pathlib, socket, struct, sys, tty
directory = pathlib.Path(__file__).parent
arguments = sys.argv[1:]
def exact(peer, count):
    data = b''
    while len(data) < count:
        chunk = peer.recv(count - len(data))
        if not chunk: raise EOFError()
        data += chunk
    return data
def receive(peer):
    first = exact(peer, 2)
    size = first[1] & 127
    if size == 126: size = struct.unpack('!H', exact(peer, 2))[0]
    if size == 127: size = struct.unpack('!Q', exact(peer, 8))[0]
    mask = exact(peer, 4) if first[1] & 128 else None
    data = exact(peer, size)
    if mask: data = bytes(value ^ mask[index % 4] for index, value in enumerate(data))
    return json.loads(data)
def send(peer, value, masked=False):
    data = json.dumps(value).encode()
    flag = 128 if masked else 0
    header = bytes([129, flag | len(data)]) if len(data) < 126 else bytes([129, flag | 126]) + struct.pack('!H', len(data))
    peer.sendall(header + (b'\0\0\0\0' if masked else b'') + data)
def handshake(peer):
    data = b''
    while not data.endswith(b'\r\n\r\n'): data += exact(peer, 1)
if arguments and arguments[0] == 'app-server':
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(arguments[arguments.index('--listen') + 1][7:])
    listener.listen(1)
    peer, _ = listener.accept()
    handshake(peer)
    peer.sendall(b'HTTP/1.1 101 Switching Protocols\r\n\r\n')
    while True:
        request = receive(peer)
        if request.get('method') == 'initialize': send(peer, {'id': request['id'], 'result': {}})
        if request.get('method') == 'thread/start': send(peer, {'id': request['id'], 'result': {'thread': {'id': 'exact-control', 'status': {'type': 'active', 'activeFlags': []}}}})
        if request.get('method') == 'turn/interrupt':
            send(peer, {'id': request['id'], 'result': {}})
            send(peer, {'method': 'turn/completed', 'params': {'threadId': 'exact-control', 'turn': {'status': 'interrupted'}}})
    sys.exit(0)
if arguments[:2] == ['agents', '--json']:
    with open(directory / 'raw-query-gate', 'rb', buffering=0) as gate: gate.read(1)
    print(json.dumps([{'sessionId': (directory / 'raw-session').read_text(), 'status': 'busy'}]))
    sys.exit(0)
tty.setraw(0)
if '--remote' in arguments:
    peer = socket.socket(socket.AF_UNIX)
    peer.connect(arguments[arguments.index('--remote') + 1][7:])
    peer.sendall(b'GET / HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n')
    handshake(peer)
    send(peer, {'id': 1, 'method': 'initialize'}, True)
    receive(peer)
    send(peer, {'method': 'initialized'}, True)
    send(peer, {'id': 2, 'method': 'thread/start'}, True)
    receive(peer)
elif '--session-id' in arguments:
    (directory / 'raw-session').write_text(arguments[arguments.index('--session-id') + 1])
    with open(directory / 'raw-query-gate', 'wb', buffering=0) as gate: gate.write(b'x')
else:
    extension = pathlib.Path(arguments[arguments.index('--extension') + 1])
    connection = json.loads(extension.with_name('connection.json').read_text())
    peer = socket.socket(socket.AF_UNIX)
    peer.connect(connection['socketPath'])
    peer.sendall((json.dumps({'token': connection['token'], 'sessionId': 'exact-control', 'sessionFile': None, 'status': 'working', 'detail': None}) + '\n').encode())
    peer.shutdown(socket.SHUT_WR)
    while peer.recv(64): pass
    peer.close()
print('control ready', flush=True)
while True:
    value = os.read(0, 1)
    if not value: break
    with open(directory / 'interrupt-bytes', 'ab') as output: output.write(value)
    if '--remote' in arguments and value == b'\x03':
        send(peer, {'id': 3, 'method': 'turn/interrupt', 'params': {'threadId': 'exact-control'}}, True)
        receive(peer)
        receive(peer)
    print('control received', flush=True)
",
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    Ok((program, gate))
}

#[rstest]
#[case(AgentKind::Codex, 3)]
#[case(AgentKind::Claude, 27)]
#[case(AgentKind::Pi, 27)]
fn shared_interrupt_matches_the_native_provider_key(
    host: TestResult<Host>,
    #[case] provider: AgentKind,
    #[case] expected: u8,
    #[values("abort", "interrupt")] operation: &str,
) {
    let mut host = host.unwrap();
    close_original_turn(&mut host).unwrap();
    let (program, _gate) = interrupt_provider(host.directory.path()).unwrap();
    let mut start = CommandInvocation::new(
        format!("agents.{provider}.start"),
        vec![
            host.directory.path().to_string_lossy().into_owned(),
            program.to_string_lossy().into_owned(),
            "[]".to_owned(),
        ],
        Caller::Socket,
    );
    start.target = Some(host.session.clone());
    let value = success(submit(&mut host.state, &host.wakes, start).unwrap()).unwrap();
    let terminal = target(&value, "terminal").unwrap();
    let service = host.state.terminal_agent_service().unwrap();
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        if service.live_records().iter().any(|record| {
            record.target == terminal && record.observation.status == TerminalAgentStatus::Working
        }) {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("provider reports exact active turn");
    }
    let mut invocation = CommandInvocation::new(
        format!("agents.{provider}.{operation}"),
        Vec::new(),
        Caller::Socket,
    );
    invocation.target = Some(terminal.clone());
    invocation.confirmation = Some(invocation.confirmation());
    success(submit(&mut host.state, &host.wakes, invocation).unwrap()).unwrap();
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        if let Ok(bytes) = fs::read(host.directory.path().join("interrupt-bytes"))
            && !bytes.is_empty()
        {
            assert_eq!(bytes, [expected]);
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("exact captured TUI receives interrupt byte");
    }
    assert_eq!(service.record(&terminal).unwrap().provider, provider);
    if provider == AgentKind::Codex {
        cancelled_codex_accepts_next_prompt(&mut host, &terminal).unwrap();
    }
    host.close().unwrap();
}

fn cancelled_codex_accepts_next_prompt(
    host: &mut Host,
    terminal: &CommandTarget,
) -> TestResult<()> {
    let service = host
        .state
        .terminal_agent_service()
        .ok_or("agent owner missing")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or("deadline overflow")?;
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        if service.live_records().iter().any(|record| {
            record.target == *terminal && record.observation.status == TerminalAgentStatus::Stopped
        }) {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
    let mut invocation = CommandInvocation::new(
        "agents.codex.follow_up",
        vec!["after cancellation".to_owned()],
        Caller::Socket,
    );
    invocation.target = Some(terminal.clone());
    success(submit(&mut host.state, &host.wakes, invocation)?)?;
    let expected = b"\x03after cancellation\r";
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        let bytes = fs::read(host.directory.path().join("interrupt-bytes"))?;
        if bytes.len() >= expected.len() {
            if bytes == expected {
                return Ok(());
            }
            return Err("cancelled Codex terminal received unexpected next-prompt bytes".into());
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
}

fn restart_cold(
    host: &mut Host,
    enabled: bool,
) -> TestResult<(assert_fs::TempDir, bootty_agents::TerminalAgentRecord)> {
    restart_cold_with_checkpoint(host, enabled, false, false)
}

fn restart_cold_with_checkpoint(
    host: &mut Host,
    enabled: bool,
    legacy: bool,
    changed_backend_ids: bool,
) -> TestResult<(assert_fs::TempDir, bootty_agents::TerminalAgentRecord)> {
    if !host
        .state
        .terminal_agent_service()
        .ok_or("owner")?
        .live_records()
        .is_empty()
    {
        checkpoint(host)?;
    }
    let source_directory = host
        .state
        .config()
        .config_path
        .parent()
        .ok_or("config directory")?
        .to_path_buf();
    let source = host
        .state
        .terminal_agent_service()
        .ok_or("owner")?
        .record(&host.terminal)
        .ok_or("source")?;
    host.state
        .terminal_agent_service()
        .ok_or("owner")?
        .shutdown_and_wait()?;
    let cold = assert_fs::TempDir::new()?;
    rusqlite::Connection::open(source_directory.join("session-order.sqlite3"))?.execute(
        "VACUUM INTO ?1",
        [cold
            .path()
            .join("session-order.sqlite3")
            .to_string_lossy()
            .as_ref()],
    )?;
    for entry in fs::read_dir(&source_directory)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .ends_with(".terminals.json")
        {
            let path = cold.path().join(entry.file_name());
            fs::copy(entry.path(), &path)?;
            if legacy {
                let mut records: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
                for record in records.as_array_mut().ok_or("catalog array")? {
                    record
                        .as_object_mut()
                        .ok_or("catalog record")?
                        .remove("location");
                }
                fs::write(path, serde_json::to_vec(&records)?)?;
            }
        }
    }
    if changed_backend_ids {
        let database = rusqlite::Connection::open(cold.path().join("session-order.sqlite3"))?;
        let (identity, text): (String, String) = database.query_row("SELECT identity, terminal_snapshot FROM workspace_sessions WHERE terminal_snapshot IS NOT NULL LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut saved: bootty_mux::session_snapshot::SavedTerminalSession =
            serde_json::from_str(&text)?;
        // An interrupted checkpoint can carry a different backend generation while logical keys stay stable.
        "interrupted-session".clone_into(&mut saved.backend_id);
        for (window_index, window) in saved.windows.iter_mut().enumerate() {
            window.backend_id = format!("interrupted-window-{window_index}");
            for (pane_index, pane) in window.panes.iter_mut().enumerate() {
                pane.backend_id = format!("interrupted-pane-{window_index}-{pane_index}");
            }
        }
        saved.validate()?;
        database.execute(
            "UPDATE workspace_sessions SET terminal_snapshot=?1 WHERE identity=?2",
            [serde_json::to_string(&saved)?, identity],
        )?;
    }
    let mut config = host.state.config().clone();
    config.config_path = cold.path().join("config.toml");
    config.agents.claude.enabled = enabled;
    let (wake, wakes) = mpsc::channel();
    let (events, _receiver) = bootty_control::event_queue();
    host.state = AppState::new_for_window_with_agents(
        config,
        format!("recovery-{}", host.directory.path().display()),
        support::backends(),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
        Some(events),
    )?;
    host.wakes = wakes;
    Ok((cold, source))
}

fn restored_agent_target(host: &mut Host) -> TestResult<CommandTarget> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or("deadline")?;
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        if let Some(session) = host.state.mux().all_sessions().first()
            && session
                .windows
                .get(1)
                .is_some_and(|window| !window.panes.is_empty())
        {
            let mut binding = CommandInvocation::new(
                "resource.current",
                vec!["binding".to_owned()],
                Caller::Socket,
            );
            let value = success(submit(&mut host.state, &host.wakes, binding.clone())?)?;
            let binding_target = target(&value, "target")?;
            let session = host
                .state
                .mux()
                .all_sessions()
                .first()
                .ok_or("restored session")?
                .clone();
            let window = session.windows.get(1).ok_or("restored agent window")?;
            let pane = window
                .panes
                .first()
                .and_then(|pane| pane.pane_id.as_ref())
                .ok_or("pane")?;
            // resource.current issues a pane target after selecting the exact restored agent tab.
            "agents.focus".clone_into(&mut binding.command);
            binding.arguments.clear();
            binding.target = Some(
                bootty_mux::target::ExactMuxTarget::Pane(
                    host.state.mux_scope(),
                    session.id.clone(),
                    window.id.clone(),
                    pane.clone(),
                )
                .command_target(
                    bootty_control::ResourceKind::Terminal,
                    host.state.mux(),
                    &binding_target.handle,
                )
                .ok_or("issued pane")?,
            );
            let target = binding.target.clone().ok_or("target")?;
            success(submit(&mut host.state, &host.wakes, binding)?)?;
            return Ok(target);
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
    }
}

#[rstest]
fn cold_terminal_agent_resumes_same_tab_and_pane_with_captured_account_without_prompt_replay(
    host: TestResult<Host>,
) -> TestResult<()> {
    let mut host = host?;
    let before = host.state.mux().all_sessions()[0].windows.len();
    let (_cold, source) = restart_cold(&mut host, true)?;
    let destination = restored_agent_target(&mut host)?;
    wait_observed(&mut host, &destination).map_err(|error| {
        format!(
            "restored observation: {error}; records {:?}; error {:?}",
            host.state
                .terminal_agent_service()
                .map(|service| service.records()),
            host.state.last_error()
        )
    })?;
    let service = host.state.terminal_agent_service().ok_or("owner")?;
    let restored = service.record(&destination).ok_or("restored record")?;
    assert_eq!(restored.launch, source.launch);
    assert_eq!(
        restored.observation.session_id,
        source.observation.session_id
    );
    assert_eq!(service.records().len(), 1);
    if service.record(&source.target).is_some() {
        return Err("old catalog target remained after restored registration".into());
    }
    assert_eq!(host.state.mux().all_sessions().len(), 1);
    assert_eq!(host.state.mux().all_sessions()[0].windows.len(), before);
    let (_, lease) = service
        .spawn_parent(&destination)
        .ok_or("restored own-terminal lease")?;
    if lease.spawn_enabled() {
        return Err("cold recovery restored child-spawn authority".into());
    }
    if lease.enabled(Some(ToolCapture::Computer)) {
        return Err("cold recovery restored computer-capture authority".into());
    }
    if lease.enabled(Some(ToolCapture::Browser)) {
        return Err("cold recovery restored browser-capture authority".into());
    }
    let resumed: serde_json::Value =
        serde_json::from_slice(&fs::read(host.directory.path().join(format!(
            "session-{}.json",
            source.observation.session_id.as_ref().ok_or("id")?
        )))?)?;
    assert_eq!(
        resumed["account"],
        source
            .launch
            .account_directory
            .as_deref()
            .ok_or("account")?
    );
    let arguments: Vec<String> = serde_json::from_value(resumed["arguments"].clone())?;
    if !arguments.contains(&"--resume".to_owned()) {
        return Err("cold recovery did not use native resume".into());
    }
    if arguments.contains(&"--session-id".to_owned()) {
        return Err("cold recovery created a new provider session".into());
    }
    if host.directory.path().join("received").exists() {
        return Err("cold recovery replayed prompt input".into());
    }
    let mut duplicate = CommandInvocation::new(
        "agents.claude.restore",
        vec![source.target.handle],
        Caller::Internal,
    );
    duplicate.target = Some(destination);
    let outcome = submit(&mut host.state, &host.wakes, duplicate)?;
    if !matches!(outcome, CommandOutcome::Failed { .. }) {
        return Err(format!("duplicate cold recovery was accepted: {outcome:?}").into());
    }
    assert_eq!(host.state.mux().all_sessions()[0].windows.len(), before);
    host.close()?;
    Ok(())
}

#[rstest]
#[case::checkpoint_first(false)]
#[case::catalog_first(true)]
fn logical_agent_location_survives_interrupted_checkpoint_catalog_ordering(
    host: TestResult<Host>,
    #[case] catalog_first: bool,
) -> TestResult<()> {
    let mut host = host?;
    let (_first, source) = restart_cold_with_checkpoint(&mut host, catalog_first, true, false)?;
    let destination = restored_agent_target(&mut host)?;
    let service = host.state.terminal_agent_service().ok_or("owner")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or("deadline")?;
    loop {
        host.state.update_frame(frames::idle_frame(Instant::now()));
        if service
            .records()
            .iter()
            .all(|record| record.location.is_some())
        {
            break;
        }
        host.wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| {
                format!(
                    "association timeout {error}; records {:?}; error {:?}",
                    service.records(),
                    host.state.last_error()
                )
            })?;
    }
    if catalog_first {
        wait_observed(&mut host, &destination)?;
        host.terminal = destination.clone();
        assert_ne!(service.records()[0].target, source.target);
    } else {
        if !service.live_records().is_empty() {
            return Err("disabled provider launched during logical association".into());
        }
        assert_eq!(service.records()[0].target, source.target);
    }
    let mut stale = CommandInvocation::new(
        "agents.claude.associate",
        vec![source.target.handle.clone()],
        Caller::Internal,
    );
    stale.target = Some(source.target);
    let outcome = submit(&mut host.state, &host.wakes, stale)?;
    if !matches!(outcome, CommandOutcome::StaleTarget { .. }) {
        return Err(format!("stale logical association was accepted: {outcome:?}").into());
    }
    // Old catalog target survives a new shell checkpoint; its durable logical location reconnects.
    let (_second, retained) = restart_cold_with_checkpoint(&mut host, true, false, true)?;
    let resumed = restored_agent_target(&mut host).map_err(|error| {
        format!(
            "second restore topology {error}; error {:?}",
            host.state.last_error()
        )
    })?;
    wait_observed(&mut host, &resumed).map_err(|error| {
        format!(
            "second restore agent {error}; records {:?}; error {:?}",
            host.state
                .terminal_agent_service()
                .map(|owner| owner.records()),
            host.state.last_error()
        )
    })?;
    let restored = host
        .state
        .terminal_agent_service()
        .ok_or("owner")?
        .record(&resumed)
        .ok_or("restored")?;
    assert_eq!(restored.location, retained.location);
    assert_eq!(
        restored.observation.session_id,
        retained.observation.session_id
    );
    assert_eq!(restored.launch, retained.launch);
    if host.directory.path().join("received").exists() {
        return Err("interrupted recovery replayed prompt input".into());
    }
    assert_eq!(host.state.mux().all_sessions()[0].windows.len(), 2);
    host.terminal = resumed;
    host.close()?;
    Ok(())
}

#[rstest]
#[case::command(false)]
#[case::native_typing(true)]
fn ordinary_input_revokes_cold_agent_recovery(
    host: TestResult<Host>,
    #[case] native: bool,
) -> TestResult<()> {
    let mut host = host?;
    let (_cold, source) = restart_cold(&mut host, false)?;
    let destination = restored_agent_target(&mut host)?;
    if !host
        .state
        .terminal_agent_service()
        .ok_or("owner")?
        .live_records()
        .is_empty()
    {
        return Err("ordinary input recovery boundary has a live agent".into());
    }
    if native {
        let mut frame = frames::idle_frame(Instant::now());
        frame
            .input
            .events
            .push(bootty_ui::gpui::InputEvent::ImeCommit(
                "echo ordinary-input".to_owned(),
            ));
        host.state.update_frame(frame);
    } else {
        let mut write = CommandInvocation::new(
            "terminal.write",
            vec!["echo ordinary-input".to_owned()],
            Caller::Socket,
        );
        write.target = Some(destination.clone());
        success(submit(&mut host.state, &host.wakes, write)?)?;
    }
    fs::write(
        &host.state.config().config_path,
        "[multiplexer]\nbackend = 'native'\n[agents.claude]\nenabled = true\n",
    )?;
    if !host.state.reload_config(&mut Vec::new()) {
        return Err("provider enablement reload failed".into());
    }
    let mut recover = CommandInvocation::new(
        "agents.claude.restore",
        vec![source.target.handle],
        Caller::Internal,
    );
    recover.target = Some(destination);
    let outcome = submit(&mut host.state, &host.wakes, recover)?;
    if !matches!(outcome, CommandOutcome::Failed { .. }) {
        return Err(format!("ordinary input did not revoke recovery: {outcome:?}").into());
    }
    if !host
        .state
        .terminal_agent_service()
        .ok_or("owner")?
        .live_records()
        .is_empty()
    {
        return Err("ordinary input recovery boundary has a live agent".into());
    }
    host.close()?;
    Ok(())
}
