#![cfg(test)]

use crate::support::seed;
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
#[case(&["remote-worktree", "list"], "Error: missing option --project\n")]
#[case(
    &["remote-worktree", "list", "--project", "a", "--project", "b"],
    "Error: duplicate option --project\n"
)]
#[case(&["remote-worktree", "list", "--project"], "Error: options require values\n")]
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

#[test]
fn daemon_marks_canonical_worktree_aliases_as_occupied() {
    let directory = assert_fs::TempDir::new().expect("tempdir");
    let project = directory.path().join("project");
    create_fixture_dir(&project).expect("project");
    let alias = project.join("..").join("project");

    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args(["remote-worktree", "list", "--project"])
        .arg(&project)
        .arg("--open-cwd")
        .arg(alias)
        .bootty_output()
        .expect("list remote worktrees");

    assert_success(&output);
    let worktrees: Vec<bootty_git::WorktreePickerEntry> =
        serde_json::from_slice(&output.stdout).expect("worktree JSON");
    assert!(worktrees[0].occupied);
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn remote_worktree_create_resolves_options_on_the_daemon_host(#[case] request_json: bool) {
    let directory = assert_fs::TempDir::new().unwrap();
    let project = directory.child("repo");
    project.create_dir_all().unwrap();
    seed::repository(project.path(), "project", false).unwrap();
    std::fs::write(
        project.path().join(".git/refs/tags/starting-tag"),
        std::fs::read(project.path().join(".git/refs/heads/main")).unwrap(),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"));
    // The default worktree directory belongs to the daemon host's home and the client's identity.
    let host_home = directory.child("home");
    command
        .env("HOME", host_home.path())
        .env("LOCALAPPDATA", host_home.path())
        .env(
            bootty_config::DEVELOPMENT_NAMESPACE_ENV,
            "bootty-dev-0123456789abcdef",
        )
        .args([
            "--application-identity",
            "bootty-dev",
            "remote-worktree",
            "create",
            "--project",
        ])
        .arg(project.path());
    if request_json {
        command.args([
            "--request",
            r#"{"branch":"feature/new","name":"my checkout","start_ref":"starting-tag"}"#,
        ]);
    } else {
        command.args(["--branch", "feature/new"]);
    }
    let output = command.bootty_output().unwrap();
    assert_success(&output);
    let path: String = serde_json::from_slice(&output.stdout).unwrap();
    let worktrees = if cfg!(windows) {
        host_home
            .path()
            .join("bootty-dev-0123456789abcdef/worktrees")
    } else {
        host_home
            .path()
            .join(".local/share/bootty-dev-0123456789abcdef/worktrees")
    };
    assert_eq!(
        std::path::PathBuf::from(&path),
        worktrees.join("repo").join(if request_json {
            "my checkout"
        } else {
            "feature-new"
        })
    );
    let branch = Command::new("git")
        .args(["-C", &path, "branch", "--show-current"])
        .bootty_output()
        .unwrap();
    assert_success(&branch);
    assert_eq!(
        String::from_utf8(branch.stdout).unwrap().trim(),
        "feature/new"
    );
}

/// The daemon runs Bootty's worktree cleanup on its own host, resolving `~` there, and keeps a
/// checkout whose commits no remote has.
#[rstest::rstest]
fn remote_worktree_clean_runs_on_the_daemon_host() {
    let directory = assert_fs::TempDir::new().unwrap();
    let project = directory.child("repo");
    project.create_dir_all().unwrap();
    let host_home = directory.child("home");
    let checkout = host_home.path().join("worktrees").join("repo").join("old");
    seed::repository(project.path(), "project", false).unwrap();
    assert_success(
        &Command::new("git")
            .arg("-C")
            .arg(project.path())
            .args([
                "worktree",
                "add",
                "-q",
                "-b",
                "old",
                checkout.to_str().unwrap(),
            ])
            .bootty_output()
            .unwrap(),
    );
    let request = serde_json::json!({
        "directory": "~/worktrees", "limit": 10, "sessions": [], "now": i64::MAX / 2,
    });
    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .env("HOME", host_home.path())
        .env("USERPROFILE", host_home.path())
        .args(["remote-worktree", "clean", "--request"])
        .arg(request.to_string())
        .bootty_output()
        .unwrap();
    assert_success(&output);
    let report: bootty_git::CleanupReport = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report,
        bootty_git::CleanupReport {
            removed: Vec::new(),
            kept: vec![bootty_git::KeptWorktree {
                path: checkout.to_string_lossy().into_owned(),
                reason: "It has 1 commit no remote has".to_owned(),
            }],
            remaining: 1,
        }
    );
}
