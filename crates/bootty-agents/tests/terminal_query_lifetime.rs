#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    process::{Command, Stdio},
};

use bootty_agents::{AgentKind, terminal_account_status_in};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[rstest]
#[case("ready")]
#[case("malformed")]
#[case("oversized")]
fn completed_account_queries_reap_the_launcher_and_stop_pipe_holding_descendants(
    #[case] response: &str,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let parent_pid = directory.path().join("launcher.pid");
    let native_pid = directory.path().join("native.pid");
    let program = directory.path().join("provider");
    let output = match response {
        "ready" => "{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}".to_owned(),
        "oversized" => "x".repeat(1024 * 1024 + 1),
        _ => "malformed private response".to_owned(),
    };
    let script = format!(
        "#!/bin/sh\nprintf '%s' \"$$\" > {}\n/bin/cat /dev/zero > /dev/null &\nprintf '%s' \"$!\" > {}\nprintf '%s' {}\nexit 0\n",
        quoted(parent_pid.to_str().unwrap()),
        quoted(native_pid.to_str().unwrap()),
        quoted(&output)
    );
    fs::write(&program, script).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let result =
        terminal_account_status_in(AgentKind::Claude, program.to_str().unwrap(), None, None);
    let parent_pid = fs::read_to_string(parent_pid).unwrap();
    let native_pid = fs::read_to_string(native_pid).unwrap();
    let parent_alive = Command::new("/bin/kill")
        .args(["-0", &parent_pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    let native = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &native_pid])
        .output()
        .unwrap();
    let native_state = String::from_utf8_lossy(&native.stdout);
    // The OS reaps an adopted grandchild; it must already be stopped or absent.
    let native_stopped = native_state.trim().is_empty() || native_state.trim().starts_with('Z');
    if !native_stopped {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", &native_pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    assert!(!parent_alive);
    assert!(
        native_stopped,
        "Native query child still active: {native_state}"
    );
    assert_eq!(result.is_ok(), response == "ready");
    if let Ok(status) = result {
        assert_eq!(status.authenticated, Some(true));
    }
}
