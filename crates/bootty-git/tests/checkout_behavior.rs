use crate::support::seed;
use bootty_os::spawn::CommandSpawn as _;

use anyhow::Context as _;
use std::{fs, path::Path, process::Command};

use assert_fs::{TempDir, prelude::*};
use bootty_git::{Git, checkout_root, suggested_session_name};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

fn git_ok(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .bootty_output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

struct Repository {
    root: TempDir,
    main: std::path::PathBuf,
    worktree: std::path::PathBuf,
}

#[fixture]
fn repository() -> anyhow::Result<Repository> {
    let root = TempDir::new()?;
    let main = root.path().join("main");
    root.child("main").create_dir_all()?;
    seed::repository(&main, "project", false)?;
    let worktree = root.path().join("wt");
    git_ok(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().context("UTF-8 worktree path")?,
        ],
    )?;
    Ok(Repository {
        root,
        main,
        worktree,
    })
}

#[test]
fn git_queries_are_safe_outside_a_repository() {
    let directory = TempDir::new().expect("temporary directory");
    let path = directory.path().to_str().unwrap();
    let expected = directory
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap()
        .trim_end_matches(".git");
    assert_eq!(checkout_root(path), None);
    assert_eq!(suggested_session_name(path), expected);
}

#[rstest]
fn suggested_names_group_linked_checkouts_by_repository_and_branch(
    repository: anyhow::Result<Repository>,
) {
    let repository = repository.expect("repository fixture");
    let nested = repository.worktree.join("nested");
    fs::create_dir(&nested).expect("create nested directory");

    assert_eq!(
        suggested_session_name(repository.main.to_str().unwrap()),
        "main/main"
    );
    assert_eq!(
        suggested_session_name(nested.to_str().unwrap()),
        "main/feature"
    );
}

#[rstest]
fn detached_checkouts_use_their_directory_as_the_session_leaf(
    repository: anyhow::Result<Repository>,
) {
    let repository = repository.expect("repository fixture");
    let detached = repository.root.path().join("detached");
    git_ok(
        &repository.main,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().unwrap(),
        ],
    )
    .expect("git setup command");
    assert_eq!(
        suggested_session_name(detached.to_str().unwrap()),
        "main/detached"
    );
}

#[rstest]
fn native_branch_and_diff_facts_follow_the_checkout(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    assert_eq!(
        Git::new()
            .head_branch(repository.worktree.to_str().unwrap())
            .as_deref(),
        Some("feature")
    );
    fs::write(repository.worktree.join("staged.txt"), "one\n").expect("write staged file");
    git_ok(&repository.worktree, &["add", "staged.txt"]).expect("git setup command");
    assert_eq!(
        Git::new().diff_counts(repository.worktree.to_str().unwrap()),
        Some((1, 0))
    );
}

#[rstest]
fn only_a_linked_checkout_belongs_to_another_project(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    let bare = repository.root.path().join("bare.git");
    git_ok(
        &repository.main,
        &["clone", "--bare", ".", bare.to_str().unwrap()],
    )
    .expect("git setup command");
    let bare_checkout = repository.root.path().join("bare-checkout");
    git_ok(
        &bare,
        &[
            "worktree",
            "add",
            "--detach",
            bare_checkout.to_str().unwrap(),
        ],
    )
    .expect("git setup command");
    let main = repository.main.canonicalize().unwrap();
    let nested = repository.worktree.join("nested");
    fs::create_dir(&nested).unwrap();
    let git = Git::new();
    let project = |path: &Path| {
        git.linked_checkout_project(path.to_str().unwrap())
            .map(|path| Path::new(&path).canonicalize().unwrap())
    };
    assert_eq!(
        [
            project(&repository.worktree),
            project(&nested),
            project(&repository.main),
            project(&bare_checkout),
            project(repository.root.path()),
        ],
        [Some(main.clone()), Some(main), None, None, None,]
    );
}
