use assert_fs::TempDir;
#[cfg(unix)]
use assert_fs::prelude::*;
#[cfg(unix)]
use bootty_config::config::{SshRemoteConfig, WslDistribution, WslRemoteConfig};
use bootty_host::{
    CancellableCommandRunner, CommandCancellation, CommandRunner, SystemCommandRunner,
};
#[cfg(unix)]
use bootty_host::{remote::RemoteHost, run_remote_command};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(false)]
#[case(true)]
fn captured_directory_does_not_change_the_callers_directory(#[case] cancellable: bool) {
    let directory = TempDir::new().expect("working-directory fixture");
    let before = std::env::current_dir().expect("working-directory fixture");
    let path = directory
        .path()
        .to_str()
        .expect("working-directory fixture");
    let (program, args) = if cfg!(windows) {
        ("cmd", vec!["/c".into(), "cd".into()])
    } else {
        ("pwd", Vec::new())
    };
    let output = if cancellable {
        CancellableCommandRunner::new(CommandCancellation::default()).run_in(path, program, &args)
    } else {
        SystemCommandRunner.run_in(path, program, &args)
    }
    .expect("working-directory fixture");
    assert!(output.success);
    assert_eq!(
        std::fs::canonicalize(output.stdout.trim()).expect("working-directory fixture"),
        std::fs::canonicalize(directory.path()).expect("working-directory fixture")
    );
    assert_eq!(
        std::env::current_dir().expect("working-directory fixture"),
        before
    );
    let cancellation = CommandCancellation::default();
    cancellation.cancel();
    assert!(
        CancellableCommandRunner::new(cancellation)
            .run_in(path, "must-not-launch", &[])
            .unwrap_err()
            .to_string()
            .contains("canceled")
    );
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn remote_directory_and_arguments_survive_the_protocol_without_shell_evaluation(#[case] wsl: bool) {
    let directory = TempDir::new().expect("working-directory fixture");
    let checkout = directory.child("it's a $checkout; 開発");
    checkout
        .create_dir_all()
        .expect("working-directory fixture");
    checkout
        .child("marker")
        .write_str("retained")
        .expect("working-directory fixture");
    let host = if wsl {
        RemoteHost::new(WslRemoteConfig {
            distribution: WslDistribution::new("Ubuntu").expect("working-directory fixture"),
        })
    } else {
        RemoteHost::new(SshRemoteConfig::for_host("private-test-host"))
    };
    let (_, args) = host
        .proxy_command_in(
            checkout.path().to_str().expect("working-directory fixture"),
            "/bin/sh",
            &[
                "-c".into(),
                "test \"$(cat marker)\" = retained && test \"$1\" = \"it's $2\"".into(),
                "sh".into(),
                "it's literal $HOME".into(),
                "literal $HOME".into(),
            ],
        )
        .expect("working-directory fixture");
    let payload = if wsl {
        args.last().expect("working-directory fixture").as_str()
    } else {
        args.last()
            .expect("working-directory fixture")
            .split_whitespace()
            .last()
            .expect("working-directory fixture")
    };
    assert_eq!(
        run_remote_command(payload).expect("working-directory fixture"),
        0
    );
    checkout.child("marker").assert("retained");
}
