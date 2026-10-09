#![cfg(unix)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_config::config::SshRemoteConfig;
use bootty_host::remote_link::RemoteProcessRequest;
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    os::unix::fs::PermissionsExt as _,
    process::{Command, Stdio},
};

#[rstest]
fn unavailable_direct_transport_falls_back_without_repeated_bootstrap() {
    let root = assert_fs::TempDir::new_in("/tmp").expect("private fixture");
    let ssh = root.path().join("ssh");
    let log = root.path().join("calls");
    std::fs::write(&ssh, format!(
        "#!/bin/sh\nfor line; do :; done\ncase \"$line\" in\n*remote-link-bootstrap*) printf 'bootstrap\\n' >> {}; exit 1;;\n*remote-exec*) printf 'exec\\n' >> {}; cat; exit 9;;\n*) exit 1;;\nesac\n",
        bootty_host::shell_quote(&log.to_string_lossy()), bootty_host::shell_quote(&log.to_string_lossy()),
    )).expect("SSH fixture");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))
        .expect("executable fixture");
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
            .spawn()
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
#[case(None, "bootty-dev-0000000000000088")]
#[case(Some("bootty-dev"), "bootty-dev-0000000000000088")]
#[case(Some("bootty"), "bootty")]
fn shared_transport_inherits_development_identity_and_preserves_explicit_callers(
    #[case] explicit: Option<&str>,
    #[case] namespace: &str,
) {
    let root = assert_fs::TempDir::new_in("/tmp").expect("private fixture");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    let ssh = root.path().join("ssh");
    let broker_pid = root.path().join("broker.pid");
    std::fs::write(&ssh, format!(
        "#!/bin/sh\nprintf '%s' \"$PPID\" > {}\nexec {} --application-identity bootty-dev remote-link-bootstrap\n",
        bootty_host::shell_quote(&broker_pid.to_string_lossy()), bootty_host::shell_quote(daemon),
    )).expect("SSH bootstrap fixture");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))
        .expect("executable fixture");
    let mut config = SshRemoteConfig::for_host("fixture");
    config.program = ssh.to_string_lossy().into_owned();
    let mut args = Vec::new();
    if let Some(identity) = explicit {
        args.extend(["--application-identity".into(), identity.into()]);
    }
    args.extend(["remote-space".into(), "list".into()]);
    let request = RemoteProcessRequest {
        program: bootty_host::REMOTE_DAEMON_PROGRAM.into(),
        args,
        cwd: None,
        terminal: None,
    };
    let output = Command::new(daemon)
        .args([
            "--application-identity",
            "bootty-dev",
            "remote-link-exec",
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(&config).expect("config")),
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(&request).expect("request")),
        ])
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env_remove("BOOTTY_DAEMON_STATE")
        .env(
            "BOOTTY_DEVELOPMENT_NAMESPACE",
            "bootty-dev-0000000000000088",
        )
        .env("SSH_CONNECTION", "127.0.0.1 1 127.0.0.1 22")
        .stdin(Stdio::null())
        .output()
        .expect("shared transport call");
    let pid: u32 = std::fs::read_to_string(broker_pid)
        .expect("private broker identity")
        .parse()
        .expect("broker PID");
    let stopped = Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("stop private test relay");
    assert!(stopped.success());
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("catalog"),
        serde_json::json!([])
    );
    assert!(
        root.path()
            .join("state")
            .join(namespace)
            .join("daemon.sqlite")
            .is_file()
    );
    let other = if namespace == "bootty" {
        "bootty-dev-0000000000000088"
    } else {
        "bootty"
    };
    assert!(
        !root
            .path()
            .join("state")
            .join(other)
            .join("daemon.sqlite")
            .exists(),
        "the other identity was untouched"
    );
}
