#![cfg(test)]
#![cfg(unix)]

use clap::Parser as _;
use pretty_assertions::assert_eq;

use std::{
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_control::{
    AppCommandReceiver, AppCommandRequest, AppCommandSender, Caller, CommandOutcome,
    app_command_channel as command_channel,
};
use bootty_control::{ControlPlane, ControlServer};
use bootty_ui::commands::CommandCatalog;

const HELPER_ENV: &str = "BOOTTY_CLI_CONTROL_TEST_HELPER";

fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    let owner = thread::current();
    command_channel(capacity, Arc::new(move || owner.unpark()))
}

#[test]
fn one_executable_uses_the_live_owner_without_a_second_gui() {
    let runtime = assert_fs::TempDir::new().expect("temporary runtime directory");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "cli_control_helper"])
        .env(HELPER_ENV, "1")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("RMUX_TMPDIR", runtime.path())
        .env("HOME", runtime.path().join("home"))
        .env("USERPROFILE", runtime.path().join("home"))
        .env("XDG_CONFIG_HOME", runtime.path().join("config"))
        .env("XDG_STATE_HOME", runtime.path().join("state"))
        .env("LOCALAPPDATA", runtime.path().join("state"))
        .env("APPDATA", runtime.path().join("config"))
        .env("PATH", "/usr/bin:/bin")
        .env_remove("BASH_ENV")
        .status()
        .expect("run isolated CLI control check");

    assert!(status.success());
}

#[test]
fn cli_control_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }

    let (sender, receiver) = app_command_channel(4);
    let control_plane = ControlPlane::default();
    let catalog = Arc::new(CommandCatalog::default());
    let control_catalog = catalog.control_catalog();
    let server = ControlServer::spawn(
        "main",
        sender.for_caller(Caller::Socket),
        Arc::clone(&control_catalog),
        &control_plane,
    )
    .expect("start control owner");

    let child = spawn_bootty(&["command", "ignore"]);
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "ignore");
    assert_eq!(request.invocation.arguments, Vec::<String>::new());
    assert_eq!(request.invocation.caller, Caller::Socket);
    request
        .response
        .send(CommandOutcome::success())
        .expect("complete CLI command");
    assert!(child.join().unwrap().unwrap());

    let event = r#"{"type":"session_start"}"#;
    let stdin_event = invoke(
        &["command", "agents.pi.ingest", "--stdin-json"],
        serde_json::to_vec(&[event, "%1"]).unwrap(),
    );
    let request = receive_request(&receiver);
    assert_eq!(request.invocation.command, "agents.pi.ingest");
    assert_eq!(request.invocation.arguments, vec![event, "%1"]);
    request.response.send(CommandOutcome::success()).unwrap();
    assert!(stdin_event.join().unwrap().unwrap());

    let payload =
        serde_json::json!({"hook_event_name":"PostToolUse", "result":"x".repeat(128 * 1024)})
            .to_string();
    let raw_event = invoke(
        &["command", "agents.codex.ingest", "--stdin", "%1", ""],
        payload.as_bytes().to_vec(),
    );
    let request = receive_request(&receiver);
    assert_eq!(
        request.invocation.arguments,
        vec![payload, "%1".to_owned(), String::new()]
    );
    request.response.send(CommandOutcome::success()).unwrap();
    assert!(raw_event.join().unwrap().unwrap());

    for (arguments, expected) in [
        (vec!["agents.pi.start"], vec![]),
        (vec!["agents.pi.state"], vec![]),
        (vec!["agents.pi.ingest", event, "%1"], vec![event, "%1"]),
    ] {
        let child = spawn_bootty(&arguments);
        let request = receive_request(&receiver);
        assert_eq!(request.invocation.command, arguments[0]);
        assert_eq!(request.invocation.arguments, expected);
        request.response.send(CommandOutcome::success()).unwrap();
        assert!(child.join().unwrap().unwrap());
    }

    assert_targeted_commands_use_live_owner(&receiver);

    let update = bootty::update::update().unwrap_err();
    assert!(
        update
            .to_string()
            .contains("install the complete Bootty release package")
    );

    assert_bare_invocation_uses_live_owner();

    drop(server);
}

fn assert_targeted_commands_use_live_owner(receiver: &AppCommandReceiver) {
    // Both CLI paths deliver the same exact target, wherever `--target` appears.
    let target = Some(bootty_control::CommandTarget {
        kind: bootty_control::ResourceKind::Session,
        handle: "native:claude:26".to_owned(),
        generation: 43,
    });
    for arguments in [
        &[
            "command",
            "agents.native.status",
            "--target",
            "native:claude:26@43",
            "native:claude:26",
            "43",
        ][..],
        &[
            "command",
            "agents.native.status",
            "native:claude:26",
            "43",
            "--target",
            "native:claude:26@43",
        ],
        &[
            "agents.native.status",
            "--target",
            "native:claude:26@43",
            "native:claude:26",
            "43",
        ],
    ] {
        let child = spawn_bootty(arguments);
        let request = receive_request(receiver);
        assert_eq!(request.invocation.command, "agents.native.status");
        assert_eq!(request.invocation.arguments, vec!["native:claude:26", "43"]);
        assert_eq!(request.invocation.target, target, "{arguments:?}");
        request.response.send(CommandOutcome::success()).unwrap();
        assert!(child.join().unwrap().unwrap());
    }
    let targeted_stdin = invoke(
        &[
            "command",
            "agents.native.status",
            "--target",
            "native:claude:26@43",
            "--stdin-json",
        ],
        br#"["native:claude:26","43"]"#.to_vec(),
    );
    let request = receive_request(receiver);
    assert_eq!(request.invocation.arguments, vec!["native:claude:26", "43"]);
    assert_eq!(request.invocation.target, target);
    request.response.send(CommandOutcome::success()).unwrap();
    assert!(targeted_stdin.join().unwrap().unwrap());
}

fn spawn_bootty(arguments: &[&str]) -> thread::JoinHandle<anyhow::Result<bool>> {
    invoke(arguments, Vec::new())
}

fn invoke(arguments: &[&str], input: Vec<u8>) -> thread::JoinHandle<anyhow::Result<bool>> {
    let arguments = arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect::<Vec<_>>();
    thread::spawn(move || {
        let cli =
            bootty::cli::Cli::parse_from(std::iter::once("bootty".to_owned()).chain(arguments));
        bootty::cli_runtime::invoke_with_input(&cli, &mut input.as_slice())
    })
}

fn assert_bare_invocation_uses_live_owner() {
    let fixture = assert_fs::TempDir::new().unwrap();
    let shell = fixture.path().join("login-shell");
    let marker = fixture.path().join("shell-started");
    bootty_test_support::write_executable(
        &shell,
        "#!/bin/sh\nprintf started > \"$BOOTTY_CLI_SHELL_MARKER\"\n",
    )
    .unwrap();
    let binary = fixture.path().join("bootty");
    bootty_test_support::place_executable(env!("CARGO_BIN_EXE_bootty"), &binary).unwrap();
    let sidecar = fixture.path().join("bootty-daemon");
    let daemon_marker = fixture.path().join("daemon-started");
    bootty_test_support::write_executable(
        &sidecar,
        "#!/bin/sh\nprintf started > \"$BOOTTY_CLI_DAEMON_MARKER\"\nprintf unusable\n",
    )
    .unwrap();
    let mut bare = Command::new(binary)
        .env_remove("BOOTTY_DAEMON_BINARY")
        .env("BOOTTY_CLI_DAEMON_MARKER", &daemon_marker)
        .env("BOOTTY_SHELL", &shell)
        .env("BOOTTY_CLI_SHELL_MARKER", &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start bare Bootty invocation");
    let deadline = Instant::now()
        .checked_add(CLI_BUDGET)
        .expect("CLI deadline is representable");
    let status = loop {
        if let Some(status) = bare.try_wait().expect("poll bare invocation") {
            break status;
        }
        if Instant::now() >= deadline {
            bare.kill().expect("stop unexpected second GUI");
            panic!("bare Bootty tried to open a second GUI");
        }
        thread::yield_now();
    };
    assert!(status.success());
    assert!(!marker.exists(), "an existing owner needs no login shell");
    assert!(
        !daemon_marker.exists(),
        "an existing owner needs no sidecar discovery"
    );
}

/// Wall-clock budget for a spawned CLI to reach the app command channel.
///
/// A loaded parallel run can starve the child for seconds, so the budget only
/// has to be generous enough never to expire on a healthy run.
const CLI_BUDGET: Duration = Duration::from_secs(30);

fn receive_request(receiver: &AppCommandReceiver) -> AppCommandRequest {
    let deadline = Instant::now()
        .checked_add(CLI_BUDGET)
        .expect("CLI deadline is representable");
    loop {
        if let Ok(request) = receiver.try_recv() {
            return request;
        }
        assert!(
            Instant::now() < deadline,
            "CLI did not reach app command channel"
        );
        thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}
