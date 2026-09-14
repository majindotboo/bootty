#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(unix)]
use std::{thread, time::Duration};

#[cfg(target_os = "macos")]
use assert_fs::TempDir;
#[cfg(unix)]
use assert_fs::{TempDir as UnixTempDir, prelude::*};
#[cfg(target_os = "macos")]
use bootty_host::SystemCommandRunner;
use bootty_host::{CancellableCommandRunner, CommandCancellation, CommandRunner};
use pretty_assertions::assert_eq;

#[cfg(target_os = "macos")]
const HELPER_ENV: &str = "BOOTTY_MUX_PROCESS_HELPER";

#[cfg(target_os = "macos")]
#[test]
fn disowned_commands_resolve_programs_and_preserve_the_bootty_environment() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new().expect("temporary process directory");
    let program = directory.child("bootty-env-probe");
    let captured_path = directory.child("captured-path");
    let captured_custom = directory.child("captured-custom");
    program
        .write_str(
            "#!/bin/sh\nprintf '%s' \"$PATH\" > \"$1\"\nprintf '%s' \"$BOOTTY_ENV_PROBE\" > \"$2\"",
        )
        .expect("write environment probe");
    std::fs::set_permissions(program.path(), std::fs::Permissions::from_mode(0o755))
        .expect("make environment probe executable");
    let path = std::env::join_paths(std::iter::once(directory.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("join PATH");

    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "process_behavior_helper", "--nocapture"])
        .env(HELPER_ENV, "1")
        .env("PATH", path)
        .env("BOOTTY_ENV_PROBE", "login-env-value")
        .env("BOOTTY_ENV_PROBE_PROGRAM", program.path())
        .env("BOOTTY_ENV_PROBE_PATH", captured_path.path())
        .env("BOOTTY_ENV_PROBE_CUSTOM", captured_custom.path())
        .output()
        .expect("run isolated process behavior test");

    assert!(
        output.status.success(),
        "stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let child_path = std::fs::read_to_string(captured_path.path()).expect("captured PATH");
    assert!(
        std::env::split_paths(std::ffi::OsStr::new(&child_path))
            .any(|entry| entry == directory.path())
    );
    assert_eq!(
        std::fs::read_to_string(captured_custom.path()).expect("captured custom environment"),
        "login-env-value"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn process_behavior_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }
    let program = std::env::var("BOOTTY_ENV_PROBE_PROGRAM").expect("probe program");
    let captured_path = std::env::var("BOOTTY_ENV_PROBE_PATH").expect("captured PATH");
    let captured_custom =
        std::env::var("BOOTTY_ENV_PROBE_CUSTOM").expect("captured custom environment");
    let resolved = bootty_host::resolve_program("bootty-env-probe")
        .expect("resolve bare program through PATH");
    assert_eq!(resolved, program);
    assert_eq!(
        bootty_host::resolve_program("./tmux").expect("keep relative program"),
        "./tmux"
    );

    let output = SystemCommandRunner
        .run_disowned("bootty-env-probe", &[captured_path, captured_custom])
        .expect("run disowned environment probe");
    assert!(output.success, "probe failed: {}", output.stderr);
}

#[test]
fn canceled_runner_does_not_start_a_command() {
    let cancellation = CommandCancellation::default();
    cancellation.cancel();
    let runner = CancellableCommandRunner::new(cancellation);

    assert_eq!(
        runner
            .run("bootty-command-that-must-not-exist", &[])
            .unwrap_err()
            .to_string(),
        "command canceled"
    );
}

#[cfg(unix)]
fn marker_was_not_created(runner: &CancellableCommandRunner) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let directory = UnixTempDir::new().expect("temporary process directory");
    let program = directory.child("marker-command");
    let marker = directory.child("started");
    program
        .write_str("#!/bin/sh\nprintf started > \"$1\"\n")
        .expect("write marker command");
    std::fs::set_permissions(program.path(), std::fs::Permissions::from_mode(0o755))
        .expect("make marker command executable");

    let error = runner
        .run(
            program.path().to_str().expect("UTF-8 marker command path"),
            &[marker.path().to_string_lossy().into_owned()],
        )
        .expect_err("pre-cancelled command");
    assert_eq!(error.to_string(), "command canceled");
    !marker.exists()
}

#[cfg(unix)]
#[test]
fn pre_cancelled_runner_does_not_start_a_marker_command() {
    let cancellation = CommandCancellation::default();
    cancellation.cancel();

    assert!(marker_was_not_created(&CancellableCommandRunner::new(
        cancellation
    )));
}

#[cfg(unix)]
#[test]
fn expired_runner_does_not_start_a_marker_command() {
    let deadline = std::time::Instant::now();

    assert!(marker_was_not_created(
        &CancellableCommandRunner::with_deadline(CommandCancellation::default(), deadline,)
    ));
}

#[cfg(unix)]
#[test]
fn cancellation_stops_a_running_command() {
    let cancellation = CommandCancellation::default();
    let runner = CancellableCommandRunner::new(cancellation.clone());
    let worker = thread::spawn(move || runner.run("sh", &["-c".to_owned(), "sleep 10".to_owned()]));
    thread::sleep(Duration::from_millis(50));

    cancellation.cancel();
    let error = worker
        .join()
        .expect("command worker")
        .expect_err("canceled command");

    assert_eq!(error.to_string(), "command canceled");
}
