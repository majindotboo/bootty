#![cfg(unix)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_config::config::SshRemoteConfig;
use bootty_host::remote_link::RemoteProcessRequest;
use bootty_os::spawn::CommandSpawn as _;
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

#[rstest]
fn unavailable_direct_transport_falls_back_without_repeated_bootstrap() {
    let root = assert_fs::TempDir::new_in("/tmp").expect("private fixture");
    let ssh = root.path().join("ssh");
    let log = root.path().join("calls");
    bootty_test_support::write_executable(&ssh,
        "#!/bin/sh\nfor line; do :; done\ncase \"$line\" in\n*remote-link-bootstrap*) printf 'bootstrap\n' >> \"$BOOTTY_REMOTE_LINK_LOG\"; exit 1;;\n*remote-exec*) printf 'exec\n' >> \"$BOOTTY_REMOTE_LINK_LOG\"; cat; exit 9;;\n*) exit 1;;\nesac\n",
    ).expect("SSH fixture");
    let mut config = SshRemoteConfig::for_host("fixture");
    config.program = ssh.to_string_lossy().into_owned();
    let request = RemoteProcessRequest {
        program: "probe".into(),
        args: Vec::new(),
        cwd: None,
        terminal: None,
    };
    let config = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&config).expect("config"));
    let request = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&request).expect("request"));
    let bytes: Vec<u8> = (0..=255).cycle().take(1024).collect();
    for _ in 0..2 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
            .env("BOOTTY_REMOTE_LINK_LOG", &log)
            .args([
                "--application-identity",
                "bootty-dev",
                "remote-link-exec",
                &config,
                &request,
            ])
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env(
                "BOOTTY_DEVELOPMENT_NAMESPACE",
                "bootty-dev-0000000000000088",
            )
            .env("BOOTTY_DAEMON_STATE", root.path().join("daemon.sqlite"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .bootty_spawn()
            .expect("relay process");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(&bytes)
            .expect("input");
        let output = child.wait_with_output().expect("relay output");
        assert_eq!(output.status.code(), Some(9));
        assert_eq!(output.stdout, bytes);
        assert_eq!(output.stderr, b"");
    }
    assert_eq!(
        std::fs::read_to_string(log).expect("calls"),
        "bootstrap\nexec\nexec\n"
    );
}

#[rstest]
#[case(None)]
#[case(Some("bootty-dev"))]
#[case(Some("bootty"))]
fn shared_transport_inherits_development_identity_and_preserves_explicit_callers(
    #[case] explicit: Option<&str>,
) {
    let mut args = Vec::new();
    if let Some(identity) = explicit {
        args.extend(["--application-identity".into(), identity.into()]);
    }
    args.extend(["remote-project".into(), "list".into()]);
    let effective = bootty_host::daemon_program_args(
        bootty_host::REMOTE_DAEMON_PROGRAM,
        &args,
        bootty_config::ApplicationIdentity::Development,
    );
    assert_eq!(
        effective.as_ref(),
        &[
            "--application-identity".to_owned(),
            explicit.unwrap_or("bootty-dev").to_owned(),
            "remote-project".to_owned(),
            "list".to_owned(),
        ]
    );
    if explicit.is_some() {
        return;
    }
    let root = assert_fs::TempDir::new_in("/tmp").expect("private fixture");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    let ssh = root.path().join("ssh");
    let broker_pid = root.path().join("broker.pid");
    bootty_test_support::write_executable(&ssh,
        "#!/bin/sh\nprintf '%s' \"$PPID\" > \"$BOOTTY_REMOTE_LINK_BROKER_PID\"\nexec \"$BOOTTY_REMOTE_LINK_DAEMON\" --application-identity bootty-dev remote-link-bootstrap\n",
    ).expect("SSH bootstrap fixture");
    let mut config = SshRemoteConfig::for_host("fixture");
    config.program = ssh.to_string_lossy().into_owned();
    let request = RemoteProcessRequest {
        program: bootty_host::REMOTE_DAEMON_PROGRAM.into(),
        args,
        cwd: None,
        terminal: None,
    };
    let output = Command::new(daemon)
        .env("BOOTTY_REMOTE_LINK_BROKER_PID", &broker_pid)
        .env("BOOTTY_REMOTE_LINK_DAEMON", daemon)
        .args([
            "--application-identity",
            "bootty-dev",
            "remote-link-exec",
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(&config).expect("config")),
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(&request).expect("request")),
        ])
        .env("HOME", root.path().join("home"))
        .env(
            "BOOTTY_DEVELOPMENT_NAMESPACE",
            "bootty-dev-0000000000000088",
        )
        .env("SSH_CONNECTION", "127.0.0.1 1 127.0.0.1 22")
        .stdin(Stdio::null())
        .bootty_output()
        .expect("shared transport call");
    let pid: u32 = std::fs::read_to_string(broker_pid)
        .expect("private broker identity")
        .parse()
        .expect("broker PID");
    let stopped = Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .bootty_status()
        .expect("stop private test relay");
    assert!(stopped.success());
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("project list"),
        serde_json::json!([])
    );
}

#[rstest]
fn native_stdio_bridge_exits_on_remote_eof_with_client_stdin_still_open() {
    let namespace = format!("bootty-dev-{:016x}", std::process::id());
    let endpoint = bootty_os::ipc::endpoint_for_label(&format!(
        "{namespace}-mux-v{}",
        bootty_mux::native::protocol::WIRE_VERSION,
    ))
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let binding = {
        let _context = runtime.enter();
        bootty_mux::native::local::bind(&endpoint).unwrap().unwrap()
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args(["--native-mux-bridge", &namespace])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .bootty_spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let (peer, identity) = runtime.block_on(binding.accept()).unwrap();
    assert!(identity.is_current_user());
    drop(peer);
    let output = child.wait_with_output().unwrap();
    drop(input);
    drop(binding);
    std::fs::remove_file(endpoint.as_path()).unwrap();
    std::fs::remove_file(endpoint.as_path().with_extension("lock")).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, Vec::<u8>::new());
}
