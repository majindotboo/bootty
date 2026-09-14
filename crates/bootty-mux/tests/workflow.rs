use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use assert_fs::TempDir;
use bootty_control::CommandCancellation;
use bootty_mux::workflow::{DitchAction, DitchCleanupOutcome, run_ditch_cleanup_with_deadline};

#[test]
fn preexpired_ditch_cleanup_leaves_worktree_and_branch_untouched() -> Result<()> {
    let (_root, main, worktree) = repository_with_worktree()?;
    let action = remove_worktree_and_branch(&main);
    let outcome = run_ditch_cleanup_with_deadline(
        Some(worktree.to_str().expect("UTF-8 worktree path")),
        &action,
        Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
        CommandCancellation::new(),
    );

    anyhow::ensure!(
        matches!(&outcome, DitchCleanupOutcome::NoAction(_)),
        "{outcome:?}"
    );
    anyhow::ensure!(worktree.exists(), "expired cleanup must keep the worktree");
    anyhow::ensure!(git_read(&main, &["branch", "--list", "feature"])?.contains("feature"));
    Ok(())
}

#[test]
fn pre_cancelled_ditch_cleanup_leaves_worktree_and_branch_untouched() -> Result<()> {
    let (_root, main, worktree) = repository_with_worktree()?;
    let action = remove_worktree_and_branch(&main);
    let cancellation = CommandCancellation::new();
    anyhow::ensure!(cancellation.cancel());
    let outcome = run_ditch_cleanup_with_deadline(
        Some(worktree.to_str().expect("UTF-8 worktree path")),
        &action,
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .context("deadline")?,
        cancellation,
    );

    anyhow::ensure!(
        matches!(&outcome, DitchCleanupOutcome::NoAction(_)),
        "{outcome:?}"
    );
    anyhow::ensure!(
        worktree.exists(),
        "cancelled cleanup must keep the worktree"
    );
    anyhow::ensure!(git_read(&main, &["branch", "--list", "feature"])?.contains("feature"));
    Ok(())
}

fn remove_worktree_and_branch(main: &Path) -> DitchAction {
    DitchAction::RemoveWorktreeAndBranch {
        force: true,
        branch: "feature".to_owned(),
        repo: main.to_string_lossy().into_owned(),
    }
}

fn repository_with_worktree() -> Result<(TempDir, PathBuf, PathBuf)> {
    let root = TempDir::new().context("temporary repository root")?;
    let main = root.path().join("main");
    let worktree = root.path().join("worktree");
    std::fs::create_dir(&main).context("create main worktree")?;
    git_ok(&main, &["init", "-q", "-b", "main"])?;
    git_ok(&main, &["config", "user.email", "test@bootty.dev"])?;
    git_ok(&main, &["config", "user.name", "Bootty Test"])?;
    std::fs::write(main.join("README"), "hello").context("write initial file")?;
    git_ok(&main, &["add", "."])?;
    git_ok(&main, &["commit", "-q", "-m", "init"])?;
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
    Ok((root, main, worktree))
}

fn git_ok(cwd: &Path, args: &[&str]) -> Result<()> {
    git_read(cwd, args).map(|_| ())
}

fn git_read(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
    ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
