#![cfg(test)]

use bootty_os::spawn::CommandSpawn as _;

use std::process::Command;

use assert_fs::prelude::*;
use pretty_assertions::assert_eq;

fn create_fixture_dir(
    path: impl AsRef<std::path::Path>,
) -> std::result::Result<(), assert_fs::fixture::FixtureError> {
    assert_fs::fixture::ChildPath::new(path.as_ref().to_path_buf()).create_dir_all()
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ping_reports_the_compatible_protocol_and_release() {
    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .arg("remote-ping")
        .bootty_output()
        .expect("ping daemon");

    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8"),
        format!(
            "{}:{}\n",
            bootty_host::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[rstest::rstest]
#[case(&["--application-identity"], "Error: --application-identity requires a value\n")]
#[case(
    &["--application-identity", "other", "remote-ping"],
    "Error: unknown application identity \"other\"\n"
)]
#[case(&["remote-project", "add"], "Error: missing option --path\n")]
#[case(
    &["remote-project", "add", "--path", "a", "--path", "b"],
    "Error: duplicate option --path\n"
)]
#[case(&["remote-project", "add", "--path"], "Error: options require values\n")]
fn cli_rejects_malformed_options_and_application_identities(
    #[case] arguments: &[&str],
    #[case] expected: &str,
) {
    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args(arguments)
        .bootty_output()
        .expect("run daemon");
    assert!(!output.status.success());
    assert_eq!(String::from_utf8(output.stderr).expect("UTF-8"), expected);
}

#[test]
fn daemon_discovers_remote_projects_with_the_shared_heuristics() {
    let directory = assert_fs::TempDir::new().expect("tempdir");
    let home = directory.path();
    create_fixture_dir(home.join("src/project")).expect("project");
    create_fixture_dir(home.join("src/.hidden")).expect("hidden");
    create_fixture_dir(home.join("dotfiles")).expect("dotfiles");

    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["remote-project", "list"])
        .bootty_output()
        .expect("list remote projects");

    assert_success(&output);
    let projects: Vec<bootty_git::ProjectPickerEntry> =
        serde_json::from_slice(&output.stdout).expect("project JSON");
    assert!(
        projects
            .iter()
            .any(|project| std::path::Path::new(&project.path).ends_with("src/project"))
    );
    assert!(
        projects
            .iter()
            .any(|project| project.path.ends_with("dotfiles"))
    );
    assert!(
        !projects
            .iter()
            .any(|project| project.path.ends_with(".hidden"))
    );
}

#[test]
fn daemon_adds_remote_project_favorites_idempotently() {
    let directory = assert_fs::TempDir::new().expect("tempdir");
    let home = directory.path();
    let project = home.join("src/project");
    create_fixture_dir(&project).expect("project");
    let path = project.to_string_lossy().into_owned();

    let add = || {
        Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
            .env("HOME", home)
            .env("USERPROFILE", home)
            .args(["remote-project", "add", "--path"])
            .arg(&path)
            .bootty_output()
            .expect("add remote project favorite")
    };
    let first = add();
    assert_success(&first);
    assert_eq!(String::from_utf8_lossy(&first.stdout).trim(), "true");

    let favorites = home.join(".config/tmux/.session-favorites");
    let content = std::fs::read(&favorites).expect("favorite bytes");
    let second = add();
    assert_success(&second);
    assert_eq!(String::from_utf8_lossy(&second.stdout).trim(), "false");
    assert_eq!(std::fs::read(favorites).expect("favorite bytes"), content);

    let listed = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["remote-project", "list"])
        .bootty_output()
        .expect("list remote projects");
    assert_success(&listed);
    let projects: Vec<bootty_git::ProjectPickerEntry> =
        serde_json::from_slice(&listed.stdout).expect("project JSON");
    assert!(
        projects
            .iter()
            .any(|entry| entry.path == path && entry.favorite)
    );
}
