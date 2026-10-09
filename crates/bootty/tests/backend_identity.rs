#![cfg(test)]
#![cfg(unix)]

use std::{env, os::unix::fs::PermissionsExt, path::PathBuf};

use assert_fs::{TempDir, prelude::*};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_config::ApplicationIdentity;
use bootty_host::ssh::SshRemote;
use bootty_mux::tmux::TmuxControlRunner;
use bootty_mux::{SshTarget, process::CommandRunner};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

const HELPER_ENV: &str = "BOOTTY_BACKEND_IDENTITY_HELPER";
const FIXTURE_ENV: &str = "BOOTTY_BACKEND_IDENTITY_FIXTURE";

fn executable(directory: &TempDir, name: &str, source: &str) -> PathBuf {
    let child = directory.child(name);
    let path = child.path().to_path_buf();
    child.write_str(source).expect("write executable");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make executable");
    path
}

#[fixture]
fn directory() -> TempDir {
    TempDir::new().expect("temporary directory")
}

#[rstest]
fn development_tmux_uses_a_distinct_server_namespace(directory: TempDir) {
    executable(
        &directory,
        "argv-probe",
        "#!/bin/sh\nprintf '%s\\n' \"$@\"\n",
    );
    executable(
        &directory,
        "ssh-probe",
        &format!(
            "#!/bin/sh\nfor line; do :; done\ncase \"$line\" in\n*uname*) printf 'Linux\\nx86_64\\n';;\n*remote-ping*) printf '{}:{}\\n';;\n*) printf '%s\\n' \"$line\";;\nesac\n",
            bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        ),
    );
    let output =
        std::process::Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "development_tmux_uses_a_distinct_server_namespace_helper",
            ])
            .env(HELPER_ENV, "1")
            .env(FIXTURE_ENV, directory.path())
            .env("PATH", directory.path())
            .output()
            .expect("run isolated backend identity check");

    assert!(
        output.status.success(),
        "isolated backend identity test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "isolated backend identity test did not execute exactly one test\nstdout:\n{stdout}"
    );
}

#[test]
fn development_tmux_uses_a_distinct_server_namespace_helper() {
    if env::var_os(HELPER_ENV).is_none() {
        return;
    }

    ApplicationIdentity::Development
        .initialize_process()
        .expect("isolated development identity");
    let fixture = PathBuf::from(env::var_os(FIXTURE_ENV).expect("fixture directory"));
    let argv_probe = fixture.join("argv-probe");
    let command = vec![
        "kill-session".to_owned(),
        "-t".to_owned(),
        "build".to_owned(),
    ];

    let production = TmuxControlRunner::for_identity(ApplicationIdentity::Production)
        .run(argv_probe.to_str().expect("probe path"), &command)
        .expect("production tmux command");
    let development = TmuxControlRunner::for_identity(ApplicationIdentity::Development)
        .run(argv_probe.to_str().expect("probe path"), &command)
        .expect("development tmux command");

    assert_eq!(production.stdout, "kill-session\n-t\nbuild\n");
    assert_eq!(
        development.stdout,
        format!(
            "-L\n{}\nkill-session\n-t\nbuild\n",
            ApplicationIdentity::Development.namespace()
        )
    );

    let remote = SshRemote::new(SshTarget {
        host: "remote.example".to_owned(),
        user: None,
        port: None,
        program: fixture.join("ssh-probe").to_string_lossy().into_owned(),
        args: Vec::new(),
    });
    let remote = TmuxControlRunner::for_remote(remote.into())
        .run("tmux", &command)
        .expect("remote tmux command");
    assert!(
        remote
            .stdout
            .contains(ApplicationIdentity::Development.namespace())
    );
    let payload = remote
        .stdout
        .split_whitespace()
        .last()
        .expect("remote execution payload");
    let command: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload)
            .expect("encoded remote command"),
    )
    .expect("versioned remote command");
    assert_eq!(
        command,
        serde_json::json!({"program": "tmux", "args": ["-L", ApplicationIdentity::Development.namespace(), "kill-session", "-t", "build"], "terminal": false})
    );
}
