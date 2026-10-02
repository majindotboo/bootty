use bootty_agents::{
    AgentLaunch, CodexTerminalObserver, CodexTerminalProtocol, TerminalAgentStatus,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};
use std::sync::Arc;

fn frame(value: &Value, masked: bool) -> Vec<u8> {
    let payload = value.to_string().into_bytes();
    raw_frame(&payload, masked)
}

fn raw_frame(payload: &[u8], masked: bool) -> Vec<u8> {
    let mut bytes = vec![0x81];
    let mask_bit = if masked { 128 } else { 0 };
    if let Ok(length) = u8::try_from(payload.len())
        && length < 126
    {
        bytes.push(length | mask_bit);
    } else if let Ok(length) = u16::try_from(payload.len()) {
        bytes.push(0x7e | mask_bit);
        bytes.extend(length.to_be_bytes());
    } else {
        bytes.push(127 | mask_bit);
        bytes.extend(
            u64::try_from(payload.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
    }
    if masked {
        let mask = [2, 3, 4, 5];
        bytes.extend(mask);
        bytes.extend(
            payload
                .iter()
                .zip(mask.iter().cycle())
                .map(|(byte, mask)| byte ^ mask),
        );
    } else {
        bytes.extend(payload);
    }
    bytes
}

fn connected() -> CodexTerminalProtocol {
    let mut protocol = CodexTerminalProtocol::default();
    let _ = protocol.observe_client(b"GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n");
    let _ = protocol.observe_server(b"HTTP/1.1 101 Switching Protocols\r\n\r\n");
    protocol
}

#[rstest]
fn idle_requires_a_successful_exact_initialize_handshake() {
    let mut protocol = connected();
    let initialized = frame(&json!({"method":"initialized"}), true);
    assert_eq!(protocol.observe_client(&initialized), Vec::new());
    assert_eq!(
        protocol.observe_client(&frame(
            &json!({"id":"initialize","method":"initialize","params":{}}),
            true
        )),
        Vec::new()
    );
    assert_eq!(
        protocol.observe_server(&frame(&json!({"id":"other","result":{}}), false)),
        Vec::new()
    );
    assert_eq!(protocol.observe_client(&initialized), Vec::new());
    let response = frame(
        &json!({"id":"initialize","result":{"userAgent":"fixture"}}),
        false,
    );
    // Incomplete frame + an idle read retain exact decoder state.
    assert_eq!(
        protocol.observe_server(response.get(..1).unwrap()),
        Vec::new()
    );
    assert_eq!(protocol.observe_server(&[]), Vec::new());
    assert_eq!(
        protocol.observe_server(response.get(1..).unwrap()),
        Vec::new()
    );
    let observations = protocol.observe_client(&initialized);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].status, TerminalAgentStatus::Idle);
    assert_eq!(observations[0].session_id, None);
    assert_eq!(protocol.observe_client(&initialized), Vec::new());
}

#[rstest]
fn failed_initialize_does_not_mark_a_terminal_ready() {
    let mut protocol = connected();
    let _ = protocol.observe_client(&frame(&json!({"id":1,"method":"initialize"}), true));
    assert_eq!(
        protocol.observe_server(&frame(
            &json!({"id":1,"error":{"code":-1,"message":"rejected"}}),
            false
        )),
        Vec::new()
    );
    assert_eq!(
        protocol.observe_client(&frame(&json!({"method":"initialized"}), true)),
        Vec::new()
    );
}

#[rstest]
#[case("thread/start")]
#[case("thread/resume")]
#[case("thread/fork")]
fn only_correlated_thread_replies_bind_identity(#[case] method: &str) {
    let mut protocol = connected();
    assert_eq!(
        protocol.observe_client(&frame(&json!({"id":7,"method":method,"params":{}}), true)),
        Vec::new()
    );
    assert_eq!(
        protocol.observe_server(&frame(
            &json!({"id":"7","result":{"thread":{"id":"wrong"}}}),
            false
        )),
        Vec::new()
    );
    // Server approval request IDs belong to the other RPC direction.
    assert_eq!(
        protocol.observe_server(&frame(
            &json!({"id":7,"method":"item/commandExecution/requestApproval","params":{}}),
            false
        )),
        Vec::new()
    );
    let observations = protocol.observe_server(&frame(
        &json!({"id":7,"result":{"thread":{"id":"exact","status":{"type":"idle"}}}}),
        false,
    ));
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].session_id.as_deref(), Some("exact"));
    assert_eq!(observations[0].status, TerminalAgentStatus::Idle);
    assert_eq!(protocol.observe_server(&frame(&json!({"method":"thread/status/changed","params":{"threadId":"wrong","status":{"type":"active","activeFlags":[]}}}), false)), Vec::new());
}

#[rstest]
#[case(json!({"type":"active","activeFlags":[]}), TerminalAgentStatus::Working)]
#[case(json!({"type":"active","activeFlags":["waitingOnApproval"]}), TerminalAgentStatus::Waiting)]
#[case(json!({"type":"active","activeFlags":["waitingOnUserInput"]}), TerminalAgentStatus::Waiting)]
#[case(json!({"type":"idle"}), TerminalAgentStatus::Idle)]
#[case(json!({"type":"systemError"}), TerminalAgentStatus::Error)]
#[case(json!({"type":"notLoaded"}), TerminalAgentStatus::Unavailable)]
fn official_thread_states_are_reported(
    #[case] status: Value,
    #[case] expected: TerminalAgentStatus,
) {
    let mut protocol = connected();
    let _ = protocol.observe_client(&frame(&json!({"id":1,"method":"thread/start"}), true));
    let _ = protocol.observe_server(&frame(
        &json!({"id":1,"result":{"thread":{"id":"exact"}}}),
        false,
    ));
    let observations = protocol.observe_server(&frame(
        &json!({"method":"thread/status/changed","params":{"threadId":"exact","status":status}}),
        false,
    ));
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].status, expected);
}

#[rstest]
#[case("completed", TerminalAgentStatus::Finished)]
#[case("interrupted", TerminalAgentStatus::Stopped)]
#[case("failed", TerminalAgentStatus::Error)]
fn completion_is_not_inferred_from_exit(
    #[case] turn_status: &str,
    #[case] expected: TerminalAgentStatus,
) {
    let mut protocol = connected();
    let _ = protocol.observe_client(&frame(&json!({"id":"start","method":"thread/start"}), true));
    let _ = protocol.observe_server(&frame(
        &json!({"id":"start","result":{"thread":{"id":"exact"}}}),
        false,
    ));
    let observations = protocol.observe_server(&frame(&json!({"method":"turn/completed","params":{"threadId":"exact","turn":{"status":turn_status}}}), false));
    assert_eq!(observations[0].status, expected);
}

proptest! {
    #[test]
    fn arbitrary_chunk_boundaries_preserve_exact_correlation(chunk in 1usize..100, id in "[a-zA-Z0-9]{1,80}") {
        let mut protocol = connected();
        for bytes in frame(&json!({"id":id,"method":"thread/start"}), true).chunks(chunk) {
            prop_assert!(protocol.observe_client(bytes).is_empty());
        }
        let mut observed = Vec::new();
        for bytes in frame(&json!({"id":id,"result":{"thread":{"id":"exact"}}}), false).chunks(chunk) {
            observed.extend(protocol.observe_server(bytes));
        }
        prop_assert_eq!(observed.len(), 1);
        prop_assert_eq!(observed[0].session_id.as_deref(), Some("exact"));
    }
}

#[rstest]
fn oversized_frames_fail_observation_without_allocating_the_payload() {
    let mut protocol = connected();
    let mut header = vec![0x81, 127];
    header.extend(u64::MAX.to_be_bytes());
    let observations = protocol.observe_server(&header);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].status, TerminalAgentStatus::Unavailable);
    assert_eq!(protocol.observe_server(b"anything"), Vec::new());
}

#[rstest]
#[case(false, 8192)]
#[case(true, 8192)]
#[case(false, usize::MAX)]
#[case(true, usize::MAX)]
fn oversized_messages_preserve_framing_and_later_exact_activity(
    #[case] fragmented: bool,
    #[case] chunk_size: usize,
) {
    let mut protocol = connected();
    let _ = protocol.observe_client(&frame(&json!({"id":1,"method":"thread/start"}), true));
    let _ = protocol.observe_server(&frame(
        &json!({"id":1,"result":{"thread":{"id":"exact"}}}),
        false,
    ));
    let payload = json!({
        "method":"turn/completed", "params":{"threadId":"exact","turn":{"status":"completed"}},
        "padding":"x".repeat(1024 * 1024),
    })
    .to_string()
    .into_bytes();
    let mut stream = if fragmented {
        let (first, last) = payload.split_at(payload.len().checked_div(2).unwrap());
        let mut first = raw_frame(first, false);
        *first.first_mut().unwrap() = 0x01;
        let mut last = raw_frame(last, false);
        *last.first_mut().unwrap() = 0x80;
        first.extend(last);
        first
    } else {
        raw_frame(&payload, false)
    };
    stream.extend(frame(
        &json!({"method":"turn/started","params":{"threadId":"wrong"}}),
        false,
    ));
    stream.extend(frame(
        &json!({"method":"turn/started","params":{"threadId":"exact"}}),
        false,
    ));
    let observations: Vec<_> = stream
        .chunks(chunk_size)
        .flat_map(|chunk| protocol.observe_server(chunk))
        .collect();
    assert_eq!(
        observations
            .iter()
            .map(|observation| observation.status)
            .collect::<Vec<_>>(),
        [
            TerminalAgentStatus::Unavailable,
            TerminalAgentStatus::Working
        ]
    );
    assert!(
        observations
            .iter()
            .all(|observation| observation.session_id.as_deref() == Some("exact"))
    );
}

#[rstest]
fn multiple_bounded_frames_do_not_share_a_payload_budget() {
    let mut protocol = connected();
    let _ = protocol.observe_client(&frame(&json!({"id":1,"method":"thread/start"}), true));
    let _ = protocol.observe_server(&frame(
        &json!({"id":1,"result":{"thread":{"id":"exact"}}}),
        false,
    ));
    let mut stream = frame(&json!({"padding":"x".repeat(1024 * 1024 - 100)}), false);
    stream.extend(frame(
        &json!({
            "method":"turn/completed", "params":{"threadId":"exact","turn":{"status":"completed"}},
            "padding":"y".repeat(1024),
        }),
        false,
    ));
    let observations = protocol.observe_server(&stream);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].status, TerminalAgentStatus::Finished);
    assert_eq!(observations[0].session_id.as_deref(), Some("exact"));
}

#[rstest]
#[case(true, b"POST / HTTP/1.1\r\n\r\n".as_slice())]
#[case(false, b"HTTP/1.1 200 OK\r\n\r\n".as_slice())]
fn invalid_upgrade_cannot_publish_provider_activity(#[case] client: bool, #[case] header: &[u8]) {
    let mut protocol = CodexTerminalProtocol::default();
    let observations = if client {
        protocol.observe_client(header)
    } else {
        protocol.observe_server(header)
    };
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].status, TerminalAgentStatus::Unavailable);
    assert_eq!(
        protocol.observe_client(&frame(&json!({"id":1,"method":"thread/start"}), true)),
        Vec::new()
    );
    assert_eq!(
        protocol.observe_server(&frame(
            &json!({"id":1,"result":{"thread":{"id":"untrusted"}}}),
            false
        )),
        Vec::new()
    );
}

#[rstest]
fn explicit_external_endpoints_are_not_adopted() {
    let directory = assert_fs::TempDir::new().unwrap();
    let launch = AgentLaunch {
        program: "codex".to_owned(),
        cwd: None,
        arguments: vec!["--remote=unix:///unowned".to_owned()],
        ephemeral: false,
    };
    let error = CodexTerminalObserver::prepare(&launch, directory.path(), Arc::new(|_| {}))
        .err()
        .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}

#[cfg(unix)]
#[rstest]
fn real_transport_forwards_bytes_and_reaps_its_provider() {
    use assert_fs::prelude::*;
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::time::Duration;

    let directory = assert_fs::TempDir::new().unwrap();
    let executable = directory.child("provider.py");
    // A provider fixture speaks the supported transport, without credentials,
    // a real agent turn, or any global provider configuration.
    executable
        .write_str(
            r#"#!/usr/bin/env python3
import json, os, socket, struct, sys
with open(os.path.join(os.path.dirname(__file__), 'arguments'), 'w') as file:
    json.dump(sys.argv[1:], file)
endpoint = sys.argv[sys.argv.index('--listen') + 1][7:]
listener = socket.socket(socket.AF_UNIX)
listener.bind(endpoint)
listener.listen(1)
gate = socket.socket(socket.AF_UNIX)
gate.bind(os.path.join(os.path.dirname(__file__), 'gate.sock'))
gate.listen(1)
peer, _ = listener.accept()
peer.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
def exact(count):
    result = bytearray()
    while len(result) < count:
        part = peer.recv(count - len(result))
        if not part:
            raise EOFError()
        result.extend(part)
    return bytes(result)
header = b''
while not header.endswith(b'\r\n\r\n'):
    header += exact(1)
peer.sendall(b'HTTP/1.1 101 Switching Protocols\r\n\r\n')
first = exact(2)
length = first[1] & 127
extra = b''
if length == 126:
    extra = exact(2)
    length = struct.unpack('!H', extra)[0]
elif length == 127:
    extra = exact(8)
    length = struct.unpack('!Q', extra)[0]
mask = exact(4)
payload = exact(length)
raw = first + extra + mask + payload
with open(os.path.join(os.path.dirname(__file__), 'forwarded'), 'wb') as file:
    file.write(raw)
response = b'{ "id": 4, "result": {"thread": {"id":"exact"}} }'
peer.sendall(bytes([129, len(response)]) + response)
control, _ = gate.accept()
assert control.recv(1) == b'g'
header = exact(2)
extended = exact(8)
mask = exact(4)
length = struct.unpack('!Q', extended)[0]
body = exact(length)
with open(os.path.join(os.path.dirname(__file__), 'bulk'), 'wb') as file:
    file.write(header + extended + mask + body)
control.sendall(b'd')
response = b'"' + b'y' * (8 * 1024 * 1024) + b'"'
peer.sendall(bytes([129, 127]) + struct.pack('!Q', len(response)) + response)
completed = json.dumps({'method':'turn/completed', 'params':{'threadId':'exact', 'turn':{'status':'completed'}}}).encode()
peer.sendall(bytes([129, len(completed)]) + completed)
try:
    while peer.recv(4096):
        pass
finally:
    with open(os.path.join(os.path.dirname(__file__), 'exited'), 'wb') as file:
        file.write(b'closed')
"#,
        )
        .unwrap();
    std::fs::set_permissions(executable.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (sender, receiver) = mpsc::channel();
    let observer = CodexTerminalObserver::prepare(
        &AgentLaunch {
            program: executable.path().to_str().unwrap().to_owned(),
            cwd: None,
            arguments: vec![
                "--profile".to_owned(),
                "work".to_owned(),
                "-c".to_owned(),
                "shell_environment_policy.inherit=all".to_owned(),
                "--sandbox=read-only".to_owned(),
                "--ask-for-approval".to_owned(),
                "on-request".to_owned(),
            ],
            ephemeral: false,
        },
        directory.path(),
        Arc::new(move |observation| {
            let _ = sender.send(observation);
        }),
    )
    .unwrap();
    let arguments = observer.arguments();
    let endpoint = arguments[1].strip_prefix("unix://").unwrap();
    let mut terminal = UnixStream::connect(endpoint).unwrap();
    terminal
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    terminal
        .write_all(b"GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n")
        .unwrap();
    let mut upgrade = vec![0; b"HTTP/1.1 101 Switching Protocols\r\n\r\n".len()];
    terminal.read_exact(&mut upgrade).unwrap();
    assert_eq!(upgrade, b"HTTP/1.1 101 Switching Protocols\r\n\r\n");
    let request = frame(&json!({"id":4,"method":"thread/start"}), true);
    terminal.write_all(&request).unwrap();
    let expected = b"{ \"id\": 4, \"result\": {\"thread\": {\"id\":\"exact\"}} }";
    let mut response = vec![0; expected.len().checked_add(2).unwrap()];
    terminal.read_exact(&mut response).unwrap();
    assert_eq!(&response[2..], expected);
    assert_eq!(
        std::fs::read(directory.child("forwarded").path()).unwrap(),
        request
    );
    let provider_arguments: Vec<String> =
        serde_json::from_slice(&std::fs::read(directory.child("arguments").path()).unwrap())
            .unwrap();
    assert_eq!(
        &provider_arguments[3..],
        [
            "--config",
            "profile=\"work\"",
            "-c",
            "shell_environment_policy.inherit=all",
            "--config",
            "sandbox_mode=\"read-only\"",
            "--config",
            "approval_policy=\"on-request\""
        ]
    );

    let observation = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(observation.session_id.as_deref(), Some("exact"));
    // The provider gates consumption on a second socket. More than both OS
    // buffers can hold must cross the relay after that deterministic release.
    let bulk = frame(&json!("z".repeat(8 * 1024 * 1024)), true);
    let mut writer = terminal.try_clone().unwrap();
    let (completed, completion) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = writer.write_all(&bulk);
        let _ = completed.send(result);
        bulk
    });
    let limited = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(limited.status, TerminalAgentStatus::Unavailable);
    assert!(matches!(
        completion.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let mut gate = UnixStream::connect(directory.child("gate.sock").path()).unwrap();
    gate.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    gate.write_all(b"g").unwrap();
    completion
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    let mut drained = [0; 1];
    gate.read_exact(&mut drained).unwrap();
    assert_eq!(drained, [b'd']);
    assert_eq!(
        std::fs::read(directory.child("bulk").path()).unwrap(),
        writer.join().unwrap()
    );
    let expected_provider = frame(&json!("y".repeat(8 * 1024 * 1024)), false);
    let mut provider_message = vec![0; expected_provider.len()];
    terminal.read_exact(&mut provider_message).unwrap();
    assert_eq!(provider_message, expected_provider);
    let limited = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(limited.status, TerminalAgentStatus::Unavailable);
    let expected_completed = json!({"method":"turn/completed","params":{"threadId":"exact","turn":{"status":"completed"}}});
    let mut completed_header = [0; 2];
    terminal.read_exact(&mut completed_header).unwrap();
    let mut completed = vec![0; usize::from(completed_header[1])];
    terminal.read_exact(&mut completed).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&completed).unwrap(),
        expected_completed
    );
    let observed = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(observed.status, TerminalAgentStatus::Finished);
    assert_eq!(observed.session_id.as_deref(), Some("exact"));
    observer.stop();
    let stopped = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(stopped.status, TerminalAgentStatus::Stopped);
    // The terminal sees the owned transport close; no live connection survives stop.
    assert_eq!(terminal.read(&mut [0; 1]).unwrap(), 0);
}
