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

#[cfg(unix)]
#[rstest::rstest]
fn command_input_is_streamed_without_changing_argv() {
    let runner = CancellableCommandRunner::new(CommandCancellation::default());
    let input = b"binary\0input\nwith '$shell' bytes".to_vec();
    let output = runner.run_with_input("cat", &[], input.clone()).unwrap();
    assert!(output.success);
    assert_eq!(output.stdout.as_bytes(), input);
    let cancellation = CommandCancellation::default();
    cancellation.cancel();
    let runner = CancellableCommandRunner::new(cancellation);
    assert!(
        runner
            .run_with_input("nonexistent-cancelled-command", &[], input)
            .unwrap_err()
            .to_string()
            .contains("canceled")
    );

    let checks = std::sync::atomic::AtomicUsize::new(0);
    let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
        CommandCancellation::default(),
        std::time::Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("test deadline"),
        move || checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0,
    );
    assert!(
        runner
            .run_with_input("cat", &[], vec![b'x'; 1024 * 1024])
            .unwrap_err()
            .to_string()
            .contains("canceled")
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case(false, false)]
#[case(false, true)]
#[case(true, false)]
#[case(true, true)]
fn captured_output_stops_at_its_bound(#[case] cancellable: bool, #[case] stderr: bool) {
    let args = vec![
        "-c".to_owned(),
        format!("cat /dev/zero{}", if stderr { " >&2" } else { "" }),
    ];
    let error = if cancellable {
        CancellableCommandRunner::with_deadline(
            CommandCancellation::default(),
            std::time::Instant::now()
                .checked_add(Duration::from_secs(5))
                .expect("test deadline"),
        )
        .run_bytes("/bin/sh", &args)
        .unwrap_err()
    } else {
        bootty_host::SystemCommandRunner
            .run_bytes("/bin/sh", &args)
            .unwrap_err()
    };
    assert_eq!(
        error.to_string(),
        "command output exceeds the 16 MiB capture limit"
    );
}

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
#[rstest::rstest]
#[case(false)]
#[case(true)]
fn stopped_runner_does_not_start_a_marker_command(#[case] expired: bool) {
    use std::os::unix::fs::PermissionsExt;
    let cancellation = CommandCancellation::default();
    let runner = if expired {
        CancellableCommandRunner::with_deadline(cancellation, std::time::Instant::now())
    } else {
        cancellation.cancel();
        CancellableCommandRunner::new(cancellation)
    };
    let directory = UnixTempDir::new().expect("temporary process directory");
    let program = directory.child("marker-command");
    let marker = directory.child("started");
    program
        .write_str("#!/bin/sh\nprintf started > \"$1\"\n")
        .expect("marker command");
    std::fs::set_permissions(program.path(), std::fs::Permissions::from_mode(0o755))
        .expect("executable marker command");
    let error = runner
        .run(
            program.path().to_str().expect("marker command path"),
            &[marker.path().to_string_lossy().into_owned()],
        )
        .expect_err("stopped command");
    assert_eq!(
        (error.to_string(), marker.exists()),
        ("command canceled".to_owned(), false)
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case("wait")]
#[case("exit 0")]
fn cancellation_closes_pipes_inherited_by_descendants(#[case] leader: &str) {
    let directory = UnixTempDir::new().unwrap();
    let marker = directory.child("descendant");
    let ready = marker.path().to_owned();
    let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
        CommandCancellation::default(),
        std::time::Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("test deadline"),
        move || std::fs::read_to_string(&ready).is_ok_and(|pid| !pid.trim().is_empty()),
    );
    let args = vec![
        "-c".to_owned(),
        format!("cat /dev/zero >/dev/null & printf '%s' $! > \"$1\"; {leader}"),
        "bootty-cancellation-probe".to_owned(),
        marker.path().to_string_lossy().into_owned(),
    ];
    let (completed, completion) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        completed.send(runner.run("/bin/sh", &args)).unwrap();
    });
    let result = completion.recv_timeout(Duration::from_secs(2));
    if result.is_err() {
        // Keep a failed cleanup regression from leaking the owned probe or hanging the suite.
        if let Ok(pid) = std::fs::read_to_string(marker.path()) {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", pid.trim()])
                .status();
        }
    }
    worker.join().unwrap();
    assert_eq!(
        result
            .expect("cancellation must close descendant pipes")
            .unwrap_err()
            .to_string(),
        "command canceled"
    );
}
