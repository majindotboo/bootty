#![cfg(unix)]

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use bootty_agents::{
    AgentKind, AgentLaunch, AgentObservation, PiTerminalObserver, TerminalAgentService,
    TerminalAgentStatus,
};
use bootty_control::{CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};
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
        account_directory: None,
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
        // macOS may report NotConnected after the host closes an oversized event early.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::NotConnected
            ) =>
        {
            Ok(())
        }
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
    // The host closes oversized input as soon as its byte limit is crossed, possibly before
    // write_all finishes. Only this fixture permits that early transport rejection.
    match send(&connection, &encode(&event).unwrap()) {
        Err(error)
            if matches!(invalid, InvalidSnapshot::Oversized)
                && matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::NotConnected
                ) => {}
        result => result.unwrap(),
    }
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

type TimedService = (
    assert_fs::TempDir,
    Arc<TerminalAgentService>,
    Arc<AtomicU64>,
);

#[fixture]
fn timed_service() -> Result<TimedService, Box<dyn std::error::Error>> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.path().join("terminals.json");
    let seconds = Arc::new(AtomicU64::new(100));
    let base = Instant::now();
    let clock = Arc::clone(&seconds);
    let service = Arc::new(TerminalAgentService::open_with_clock(
        &path,
        Arc::new(move || {
            base.checked_add(Duration::from_secs(clock.load(Ordering::Acquire)))
                .unwrap_or(base)
        }),
    )?);
    Ok((directory, service, seconds))
}

#[rstest]
#[case("idle")]
#[case("waiting")]
#[case("finished")]
#[case("stopped")]
#[case("error")]
fn working_duration_tracks_live_transitions_without_restarting_on_updates(
    timed_service: Result<TimedService, Box<dyn std::error::Error>>,
    #[case] settled: &str,
) {
    let (directory, service, seconds) = timed_service.unwrap();
    let prepared = service.prepare(AgentKind::Pi, launch()).unwrap();
    let argv = prepared.argv();
    let extension = argv
        .windows(2)
        .find(|pair| {
            pair.first()
                .is_some_and(|argument| argument == "--extension")
        })
        .and_then(|pair| pair.get(1))
        .unwrap();
    let connection: Connection = serde_json::from_slice(
        &fs::read(PathBuf::from(extension).with_file_name("connection.json")).unwrap(),
    )
    .unwrap();
    let target = CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "host-issued-working-terminal".to_owned(),
        generation: 1,
    };
    service
        .register(prepared, target.clone(), "binding".to_owned())
        .unwrap();
    assert_eq!(service.activity(&target).unwrap().working_elapsed, None);
    let (changed, published) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = changed.send(());
    }));

    let working = snapshot(&connection, "working");
    send(&connection, &encode(&working).unwrap()).unwrap();
    published.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        service.activity(&target).unwrap().working_elapsed,
        Some(Duration::ZERO)
    );
    let revision = service.revision();
    seconds.store(107, Ordering::Release);
    assert_eq!(
        service.activity(&target).unwrap().working_elapsed,
        Some(Duration::from_secs(7))
    );
    assert_eq!(service.revision(), revision);

    // An identical snapshot produces no revision. The following detail update acknowledges
    // both observations through the same FIFO socket owner, without waiting on wall-clock time.
    send(&connection, &encode(&working).unwrap()).unwrap();
    seconds.store(108, Ordering::Release);
    let mut continuing = working.clone();
    *continuing.get_mut("detail").unwrap() = "continuing".into();
    send(&connection, &encode(&continuing).unwrap()).unwrap();
    published.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        service.activity(&target).unwrap().working_elapsed,
        Some(Duration::from_secs(8))
    );

    seconds.store(110, Ordering::Release);
    send(
        &connection,
        &encode(&snapshot(&connection, settled)).unwrap(),
    )
    .unwrap();
    published.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(service.activity(&target).unwrap().working_elapsed, None);
    seconds.store(120, Ordering::Release);
    send(&connection, &encode(&working).unwrap()).unwrap();
    published.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        service.activity(&target).unwrap().working_elapsed,
        Some(Duration::ZERO)
    );
    seconds.store(124, Ordering::Release);
    assert_eq!(
        service.activity(&target).unwrap().working_elapsed,
        Some(Duration::from_secs(4))
    );
    assert!(
        service
            .activity(&CommandTarget {
                generation: 2,
                ..target.clone()
            })
            .is_none()
    );

    service.shutdown_and_wait().unwrap();
    assert_eq!(service.activity(&target).unwrap().working_elapsed, None);
    drop(service);
    let restored = TerminalAgentService::open(directory.path().join("terminals.json")).unwrap();
    let activity = restored.activity(&target).unwrap();
    assert_eq!(activity.status, TerminalAgentStatus::Unavailable);
    assert_eq!(activity.working_elapsed, None);
}
