#![cfg(unix)]

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use bootty_agents::{AgentLaunch, AgentObservation, PiTerminalObserver, TerminalAgentStatus};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    socket_path: PathBuf,
    token: String,
}

#[derive(Clone, Copy)]
enum InvalidSnapshot {
    WrongToken,
    UnknownStatus,
    InvalidIdentity,
    Oversized,
    ExtraField,
}

fn launch() -> AgentLaunch {
    AgentLaunch {
        program: "pi".to_owned(),
        cwd: None,
        arguments: vec![
            "--model".to_owned(),
            "native-model".to_owned(),
            "--no-extensions".to_owned(),
        ],
        ephemeral: false,
    }
}

fn connection(observer: &PiTerminalObserver) -> io::Result<Connection> {
    let arguments = observer.arguments();
    let extension = arguments.get(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Pi launch requires extension path",
        )
    })?;
    serde_json::from_slice(&fs::read(
        PathBuf::from(extension).with_file_name("connection.json"),
    )?)
    .map_err(io::Error::other)
}

fn send(connection: &Connection, bytes: &[u8]) -> io::Result<()> {
    let mut stream = UnixStream::connect(&connection.socket_path)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(bytes)?;
    if let Err(error) = stream.shutdown(std::net::Shutdown::Write)
        && error.kind() != io::ErrorKind::NotConnected
    {
        return Err(error);
    }
    let mut response = Vec::new();
    // EOF acknowledges host handling, including rejected snapshots, without clock sleeps.
    match stream.read_to_end(&mut response) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => Ok(()),
        Err(error) => Err(error),
    }
}

fn snapshot(connection: &Connection, status: &str) -> Value {
    json!({
        "token": connection.token,
        "sessionId": "native-pi-session",
        "sessionFile": "/project/session.jsonl",
        "status": status,
        "detail": null,
    })
}

fn encode(snapshot: &Value) -> serde_json::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(snapshot)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[rstest]
fn launch_preserves_literal_arguments_and_owns_only_private_runtime_files() {
    let directory = assert_fs::TempDir::new().unwrap();
    let user_file = directory.path().join("user-extension.ts");
    fs::write(&user_file, "existing extension").unwrap();
    let observer =
        PiTerminalObserver::prepare(&launch(), directory.path(), Arc::new(|_| {})).unwrap();
    let arguments = observer.arguments();
    assert_eq!(&arguments[2..], launch().arguments);
    assert_eq!(arguments[0], "--extension");
    let extension = PathBuf::from(&arguments[1]);
    let runtime = extension.parent().unwrap();
    assert_eq!(
        fs::metadata(runtime).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in [
        extension.clone(),
        runtime.join("connection.json"),
        connection(&observer).unwrap().socket_path,
    ] {
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    drop(observer);
    assert!(!runtime.exists());
    assert_eq!(fs::read_to_string(user_file).unwrap(), "existing extension");
}

#[rstest]
#[case("idle", TerminalAgentStatus::Idle)]
#[case("working", TerminalAgentStatus::Working)]
#[case("waiting", TerminalAgentStatus::Waiting)]
#[case("finished", TerminalAgentStatus::Finished)]
#[case("stopped", TerminalAgentStatus::Stopped)]
#[case("error", TerminalAgentStatus::Error)]
fn native_snapshots_report_exact_identity_and_activity(
    #[case] activity: &str,
    #[case] expected: TerminalAgentStatus,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let (sender, receiver) = mpsc::channel();
    let observer = PiTerminalObserver::prepare(
        &launch(),
        directory.path(),
        Arc::new(move |event| {
            sender.send(event).unwrap();
        }),
    )
    .unwrap();
    let connection = connection(&observer).unwrap();
    send(
        &connection,
        &encode(&snapshot(&connection, activity)).unwrap(),
    )
    .unwrap();
    let event: AgentObservation = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(event.session_id.as_deref(), Some("native-pi-session"));
    assert_eq!(
        event.session_file.as_deref(),
        Some("/project/session.jsonl")
    );
    assert_eq!(event.status, expected);
}

#[rstest]
#[case(InvalidSnapshot::WrongToken)]
#[case(InvalidSnapshot::UnknownStatus)]
#[case(InvalidSnapshot::InvalidIdentity)]
#[case(InvalidSnapshot::Oversized)]
#[case(InvalidSnapshot::ExtraField)]
fn untrusted_or_unbounded_events_cannot_publish_activity(#[case] invalid: InvalidSnapshot) {
    let directory = assert_fs::TempDir::new().unwrap();
    let (sender, receiver) = mpsc::channel();
    let observer = PiTerminalObserver::prepare(
        &launch(),
        directory.path(),
        Arc::new(move |event| {
            sender.send(event).unwrap();
        }),
    )
    .unwrap();
    let connection = connection(&observer).unwrap();
    let mut event = snapshot(&connection, "working");
    match invalid {
        InvalidSnapshot::WrongToken => event["token"] = "unrelated-launch".into(),
        InvalidSnapshot::UnknownStatus => event["status"] = "invented".into(),
        InvalidSnapshot::InvalidIdentity => event["sessionId"] = "\n".into(),
        InvalidSnapshot::Oversized => event["detail"] = "x".repeat(17 * 1024).into(),
        InvalidSnapshot::ExtraField => event["prompt"] = "private content".into(),
    }
    send(&connection, &encode(&event).unwrap()).unwrap();
    send(
        &connection,
        &encode(&snapshot(&connection, "idle")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .status,
        TerminalAgentStatus::Idle
    );
    assert!(receiver.try_recv().is_err());
}

#[rstest]
fn native_session_switches_publish_new_identity_on_the_same_owned_channel() {
    let directory = assert_fs::TempDir::new().unwrap();
    let (sender, receiver) = mpsc::channel();
    let observer = PiTerminalObserver::prepare(
        &launch(),
        directory.path(),
        Arc::new(move |event| {
            sender.send(event).unwrap();
        }),
    )
    .unwrap();
    let connection = connection(&observer).unwrap();
    send(
        &connection,
        &encode(&snapshot(&connection, "working")).unwrap(),
    )
    .unwrap();
    let mut next = snapshot(&connection, "idle");
    next["sessionId"] = "new-native-session".into();
    next["sessionFile"] = "/project/new-session.jsonl".into();
    send(&connection, &encode(&next).unwrap()).unwrap();
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .session_id
            .as_deref(),
        Some("native-pi-session")
    );
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .session_id
            .as_deref(),
        Some("new-native-session")
    );
}
