use bootty_git::Git;
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::process::Command;
fn git(root: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()?;
    anyhow::ensure!(status.success(), "git {args:?}");
    Ok(())
}
fn fixture() -> anyhow::Result<assert_fs::TempDir> {
    let root = assert_fs::TempDir::new()?;
    git(root.path(), &["init", "-q", "-b", "main"])?;
    git(root.path(), &["config", "user.email", "test@example.test"])?;
    git(root.path(), &["config", "user.name", "Test User"])?;
    std::fs::write(root.path().join("file.txt"), "one\n")?;
    git(root.path(), &["add", "file.txt"])?;
    git(root.path(), &["commit", "-qm", "initial"])?;
    Ok(root)
}
#[rstest]
fn overview_branch_history_diff_and_clean_switch_are_consistent() {
    let root = fixture().expect("repository fixture");
    let path = root.path().to_str().unwrap();
    let client = Git::new();
    let overview = client.overview(path, 100).expect("test operation succeeds");
    assert_eq!(overview.history.len(), 1);
    assert_eq!(overview.history[0].subject, "initial");
    assert!(overview.branches[0].current);
    assert!(
        client
            .commit_diff(path, &overview.history[0].id)
            .expect("test operation succeeds")
            .contains("initial")
    );
    client
        .create_branch(path, "feature", "HEAD")
        .expect("test operation succeeds");
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
fn stash_apply_keeps_record_until_explicit_drop_and_includes_untracked() {
    let root = fixture().expect("repository fixture");
    let path = root.path().to_str().unwrap();
    let client = Git::new();
    std::fs::write(root.path().join("file.txt"), "changed\n").unwrap();
    std::fs::write(root.path().join("new.txt"), "new\n").unwrap();
    client
        .stash_push(path, "saved", true)
        .expect("test operation succeeds");
    let overview = client.overview(path, 20).expect("test operation succeeds");
    let stash = &overview.stashes[0];
    assert!(!root.path().join("new.txt").exists());
    client
        .stash_apply(path, &stash.reference, &stash.commit)
        .expect("test operation succeeds");
    assert!(root.path().join("new.txt").exists());
    assert_eq!(
        client
            .overview(path, 20)
            .expect("test operation succeeds")
            .stashes
            .len(),
        1
    );
    git(root.path(), &["reset", "--hard", "-q"]).expect("git setup command");
    std::fs::remove_file(root.path().join("new.txt")).unwrap();
    client
        .stash_drop(path, &stash.reference, &stash.commit)
        .expect("test operation succeeds");
    assert_eq!(
        client
            .overview(path, 20)
            .expect("test operation succeeds")
            .stashes,
        Vec::<bootty_git::changes::GitStash>::new()
    );
}
#[rstest]
fn references_are_validated_against_current_repository() {
    let root = fixture().expect("repository fixture");
    let path = root.path().to_str().unwrap();
    let client = Git::new();
    assert!(client.checkout_branch(path, "--detach").is_err());
    assert!(client.stash_apply(path, "stash@{99}", "missing").is_err());
    assert!(client.commit_diff(path, "HEAD").is_err());
    assert!(client.create_branch(path, "bad..name", "HEAD").is_err());
}

#[rstest]
fn checkout_preserves_ignored_files_that_the_destination_tracks() {
    let root = fixture().expect("repository fixture");
    let path = root.path().to_str().unwrap();
    let client = Git::new();
    client
        .create_branch(path, "other", "HEAD")
        .expect("test operation succeeds");
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
