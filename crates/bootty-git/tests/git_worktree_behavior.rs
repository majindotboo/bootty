use anyhow::Context as _;
use std::{fs, path::Path, process::Command};

use assert_fs::{TempDir, prelude::*};
use bootty_git::project::{
    WorktreeStatus, add_worktree, delete_branch, detach_head, diff_counts, head_branch,
    remove_worktree, status, suggested_session_name, trunk_branch, worktree_count,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

fn git_ok(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn git_read(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
    anyhow::ensure!(output.status.success());
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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
    git_ok(&main, &["init", "-q", "-b", "main"])?;
    git_ok(&main, &["config", "user.email", "test@bootty.dev"])?;
    git_ok(&main, &["config", "user.name", "Bootty Test"])?;
    git_ok(&main, &["config", "commit.gpgsign", "false"])?;
    git_ok(&main, &["config", "core.hooksPath", "/dev/null"])?;
    fs::write(main.join("README"), "hello")?;
    git_ok(&main, &["add", "."])?;
    git_ok(&main, &["commit", "-q", "-m", "init"])?;
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
    assert_eq!(status(path), WorktreeStatus::default());
    assert_eq!(suggested_session_name(path), expected);
}

#[rstest]
fn suggested_names_group_linked_worktrees_by_repository_and_branch(
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
fn detached_worktrees_use_their_directory_as_the_session_leaf(
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
fn status_distinguishes_main_linked_and_dirty_worktrees(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    let main_status = status(repository.main.to_str().unwrap());
    assert!(main_status.in_repo);
    assert!(!main_status.is_linked_worktree);
    assert_eq!(main_status.branch.as_deref(), Some("main"));

    let linked = status(repository.worktree.to_str().unwrap());
    assert!(linked.in_repo && linked.is_linked_worktree);
    assert_eq!(linked.branch.as_deref(), Some("feature"));
    assert!(!linked.dirty);

    fs::write(repository.worktree.join("scratch"), "wip").expect("write untracked file");
    assert!(status(repository.worktree.to_str().unwrap()).dirty);
}

#[rstest]
fn native_branch_and_diff_facts_follow_the_worktree(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    assert_eq!(
        head_branch(repository.worktree.to_str().unwrap()).as_deref(),
        Some("feature")
    );
    fs::write(repository.worktree.join("staged.txt"), "one\n").expect("write staged file");
    git_ok(&repository.worktree, &["add", "staged.txt"]).expect("git setup command");
    assert_eq!(
        diff_counts(repository.worktree.to_str().unwrap()),
        Some((1, 0))
    );
}

#[rstest]
fn detach_preserves_the_worktree_and_current_commit(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    let before = git_read(&repository.worktree, &["rev-parse", "HEAD"]).expect("git output");
    detach_head(repository.worktree.to_str().unwrap()).expect("detach HEAD");

    assert!(
        status(repository.worktree.to_str().unwrap())
            .branch
            .is_none()
    );
    assert_eq!(
        git_read(&repository.worktree, &["rev-parse", "HEAD"]).expect("git output"),
        before
    );
}

#[rstest]
fn forced_branch_deletion_removes_unmerged_work(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    fs::write(repository.worktree.join("feature.txt"), "work").expect("write branch file");
    git_ok(&repository.worktree, &["add", "."]).expect("git setup command");
    git_ok(
        &repository.worktree,
        &["commit", "-q", "-m", "feature work"],
    )
    .expect("git setup command");
    remove_worktree(repository.worktree.to_str().unwrap(), false).expect("remove worktree");
    assert!(!repository.worktree.exists());
    assert!(
        !git_read(&repository.main, &["worktree", "list"])
            .expect("git output")
            .contains("wt")
    );
    delete_branch(repository.main.to_str().unwrap(), "feature", true).expect("delete branch");
    assert_eq!(
        git_read(&repository.main, &["branch", "--list", "feature"]).expect("git output"),
        ""
    );
}

#[rstest]
fn worktree_count_includes_main_and_linked_checkouts(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    assert_eq!(worktree_count(repository.main.to_str().unwrap()), 2);
    assert_eq!(worktree_count(repository.worktree.to_str().unwrap()), 2);
}

#[rstest]
fn trunk_uses_the_remote_default_then_falls_back_to_main_worktree(
    repository: anyhow::Result<Repository>,
) {
    let repository = repository.expect("repository fixture");
    assert_eq!(
        trunk_branch(repository.worktree.to_str().unwrap()).as_deref(),
        Some("main")
    );

    let head = git_read(&repository.main, &["rev-parse", "HEAD"]).expect("git output");
    git_ok(
        &repository.main,
        &["update-ref", "refs/remotes/origin/release", &head],
    )
    .expect("git setup command");
    git_ok(
        &repository.main,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/release",
        ],
    )
    .expect("git setup command");
    assert_eq!(
        trunk_branch(repository.worktree.to_str().unwrap()).as_deref(),
        Some("release")
    );
}

#[rstest]
fn add_worktree_creates_a_sibling_for_the_new_branch(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    let created =
        add_worktree(repository.main.to_str().unwrap(), "wip/login").expect("add worktree");
    assert!(created.ends_with("main-wip-login"));
    let added = status(&created);
    assert!(added.is_linked_worktree);
    assert_eq!(added.branch.as_deref(), Some("wip/login"));
}

#[rstest]
#[case(None)]
#[case(Some("custom checkout"))]
fn create_worktree_resolves_starting_ref_and_preserves_current_checkout(
    repository: anyhow::Result<Repository>,
    #[case] name: Option<&str>,
) {
    let repository = repository.expect("repository fixture");
    let root = repository.main.to_str().unwrap();
    let initial = git_read(&repository.main, &["rev-parse", "HEAD"]).expect("git output");
    git_ok(&repository.main, &["tag", "baseline"]).expect("git setup command");
    fs::write(repository.main.join("README"), "newer").unwrap();
    git_ok(&repository.main, &["commit", "-am", "newer"]).expect("git setup command");
    let current = git_read(&repository.main, &["rev-parse", "HEAD"]).expect("git output");
    let path = bootty_git::Git::new()
        .create_worktree(
            root,
            &bootty_git::WorktreeRequest {
                branch: "topic/nested".to_owned(),
                name: name.map(str::to_owned),
                start_ref: Some("baseline".to_owned()),
            },
        )
        .unwrap();
    assert_eq!(
        Path::new(&path),
        repository
            .root
            .path()
            .canonicalize()
            .unwrap()
            .join(name.unwrap_or("main-topic-nested"))
    );
    assert_eq!(
        git_read(Path::new(&path), &["rev-parse", "HEAD"]).expect("git output"),
        initial
    );
    assert_eq!(
        git_read(Path::new(&path), &["branch", "--show-current"]).expect("git output"),
        "topic/nested"
    );
    assert_eq!(
        git_read(&repository.main, &["rev-parse", "HEAD"]).expect("git output"),
        current
    );
    assert_eq!(
        git_read(&repository.main, &["status", "--porcelain"]).expect("git output"),
        ""
    );
}

#[rstest]
#[case("feature", "new-checkout", "HEAD")]
#[case("new-branch", "../escape", "HEAD")]
#[case("new-branch", "/absolute", "HEAD")]
#[case("new-branch", "..", "HEAD")]
#[case("new-branch", "wt", "HEAD")]
#[case("new-branch", "new-checkout", "missing-ref")]
#[case("@{-1}", "new-checkout", "HEAD")]
#[case("HEAD", "new-checkout", "HEAD")]
#[case("-bad", "new-checkout", "HEAD")]
fn rejected_worktree_request_does_not_change_repository(
    repository: anyhow::Result<Repository>,
    #[case] branch: &str,
    #[case] name: &str,
    #[case] start_ref: &str,
) {
    let repository = repository.expect("repository fixture");
    let before = git_read(&repository.main, &["show-ref"]).expect("git output");
    let worktrees =
        git_read(&repository.main, &["worktree", "list", "--porcelain"]).expect("git output");
    assert!(
        bootty_git::Git::new()
            .create_worktree(
                repository.main.to_str().unwrap(),
                &bootty_git::WorktreeRequest {
                    branch: branch.to_owned(),
                    name: Some(name.to_owned()),
                    start_ref: Some(start_ref.to_owned()),
                }
            )
            .is_err()
    );
    assert_eq!(
        git_read(&repository.main, &["show-ref"]).expect("git output"),
        before
    );
    assert_eq!(
        git_read(&repository.main, &["worktree", "list", "--porcelain"]).expect("git output"),
        worktrees
    );
}

#[cfg(unix)]
#[rstest]
#[case("tab\tline\ncheckout")]
#[case("checkout \n")]
fn worktree_paths_preserve_tabs_and_newlines(
    repository: anyhow::Result<Repository>,
    #[case] name: &str,
) {
    let repository = repository.expect("repository fixture");
    let path = repository.root.path().join(name);
    git_ok(
        &repository.main,
        &["worktree", "add", "--detach", path.to_str().unwrap()],
    )
    .expect("git setup command");
    let entries = bootty_git::discover_worktree_picker_entries(repository.main.to_str().unwrap());
    let expected = path.canonicalize().unwrap();
    assert_eq!(
        bootty_git::worktree_root(path.to_str().unwrap()).as_deref(),
        expected.to_str()
    );
    assert!(
        entries
            .iter()
            .any(|entry| { entry.path.as_deref().map(Path::new) == Some(expected.as_path()) })
    );
    assert_eq!(worktree_count(repository.main.to_str().unwrap()), 3);
}

#[rstest]
fn linked_checkout_of_bare_repository_can_be_removed(repository: anyhow::Result<Repository>) {
    let repository = repository.expect("repository fixture");
    let bare = repository.root.path().join("bare.git");
    git_ok(
        &repository.main,
        &["clone", "--bare", ".", bare.to_str().unwrap()],
    )
    .expect("git setup command");
    let linked = repository.root.path().join("bare-checkout");
    git_ok(
        &bare,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    )
    .expect("git setup command");
    assert_eq!(
        bootty_git::main_worktree(linked.to_str().unwrap())
            .as_deref()
            .map(Path::new),
        Some(bare.canonicalize().unwrap().as_path())
    );
    remove_worktree(linked.to_str().unwrap(), false).expect("remove bare repository checkout");
    assert!(!linked.exists());
}
