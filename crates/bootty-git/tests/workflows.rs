use crate::support::seed;
use bootty_os::spawn::CommandSpawn as _;

use crate::support::runner;

use pretty_assertions::assert_eq;
use rstest::rstest;
use std::process::Command;
fn git(root: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .bootty_status()?;
    anyhow::ensure!(status.success(), "git {args:?}");
    Ok(())
}
fn fixture() -> anyhow::Result<assert_fs::TempDir> {
    let root = assert_fs::TempDir::new()?;
    seed::repository(root.path(), "workflow", false)?;
    Ok(root)
}
#[rstest]
fn overview_branch_history_and_clean_switch_are_consistent() {
    let root = fixture().expect("repository fixture");
    let path = root.path().to_str().unwrap();
    let client = runner::git();
    let overview = client.overview(path, 100).expect("test operation succeeds");
    assert_eq!(overview.history.len(), 1);
    assert_eq!(overview.history[0].subject, "initial");
    assert!(overview.branches[0].current);
    git(root.path(), &["switch", "-qc", "feature"]).expect("git setup command");
    assert_eq!(
        client
            .overview(path, 100)
            .expect("test operation succeeds")
            .branches
            .iter()
            .find(|b| b.current)
            .unwrap()
            .name,
        "feature"
    );
    std::fs::write(root.path().join("file.txt"), "dirty\n").unwrap();
    assert!(
        client
            .checkout_branch(path, "main")
            .unwrap_err()
            .contains("must be clean")
    );
}

#[rstest]
fn branch_catalog_identifies_existing_worktree_checkouts() {
    let root = fixture().expect("repository fixture");
    let linked = root.path().join("linked checkout");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/existing",
            linked.to_str().unwrap(),
        ],
    )
    .expect("linked checkout");
    let overview = runner::git()
        .overview(root.path().to_str().unwrap(), 1)
        .unwrap();
    for (name, expected, current) in [
        ("main", root.path(), true),
        ("feature/existing", linked.as_path(), false),
    ] {
        let branch = overview
            .branches
            .iter()
            .find(|branch| branch.name == name)
            .unwrap();
        assert_eq!(branch.current, current);
        assert_eq!(
            std::path::Path::new(branch.worktree_path.as_deref().unwrap())
                .canonicalize()
                .unwrap(),
            expected.canonicalize().unwrap(),
        );
    }
}
#[rstest]
fn references_are_validated_against_current_repository() {
    let root = fixture().expect("repository fixture");
    let path = root.path().to_str().unwrap();
    let client = runner::git();
    assert!(client.checkout_branch(path, "--detach").is_err());
    assert!(client.checkout_branch(path, "missing").is_err());
}

#[rstest]
fn checkout_preserves_ignored_files_that_the_destination_tracks() {
    let root = fixture().expect("repository fixture");
    let path = root.path().to_str().unwrap();
    let client = runner::git();
    git(root.path(), &["switch", "-qc", "other"]).expect("git setup command");
    std::fs::write(root.path().join("cache.txt"), "tracked in other\n").unwrap();
    git(root.path(), &["add", "cache.txt"]).expect("git setup command");
    git(root.path(), &["commit", "-qm", "track cache"]).expect("git setup command");
    client
        .checkout_branch(path, "main")
        .expect("test operation succeeds");
    std::fs::write(root.path().join(".git/info/exclude"), "cache.txt\n").unwrap();
    std::fs::write(root.path().join("cache.txt"), "private ignored data\n").unwrap();
    assert!(client.checkout_branch(path, "other").is_err());
    assert_eq!(
        std::fs::read_to_string(root.path().join("cache.txt")).unwrap(),
        "private ignored data\n"
    );
}
