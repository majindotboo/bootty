#![cfg(test)]
//! Snapshots around an agent's turns: what a turn changed, and putting files back.

#[path = "support/repository.rs"]
mod repository_support;

#[path = "support/process.rs"]
mod runner;

use std::process::Command;

use std::cell::Cell;

use assert_fs::{TempDir, prelude::*};
use bootty_git::{CommandOutput, CommandRunner, checkpoint};
use pretty_assertions::assert_eq;

fn run(dir: &TempDir, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A repository that commits as a test user and runs no hooks.
fn repository() -> TempDir {
    let dir = TempDir::new().unwrap();
    repository_support::init(dir.path(), false).unwrap();
    dir
}

/// Git, except that its first `add -A` loses a file mid-walk the way Git reports it.
struct VanishingFile {
    lost: Cell<bool>,
}

impl CommandRunner for VanishingFile {
    fn run_with_env(
        &self,
        program: &str,
        args: &[String],
        env: &[(&str, &str)],
    ) -> anyhow::Result<CommandOutput> {
        if !self.lost.get() && args.ends_with(&["add".to_owned(), "-A".to_owned()]) {
            self.lost.set(true);
            return Ok(CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "fatal: unable to stat 'state.sqlite3-wal': No such file or directory"
                    .to_owned(),
            });
        }
        runner::RepositoryRunner.run_with_env(program, args, env)
    }

    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        runner::RepositoryRunner.run(program, args)
    }

    fn remove_file(&self, path: &str) -> anyhow::Result<()> {
        runner::RepositoryRunner.remove_file(path)
    }
}

/// A snapshot keeps every file, untracked ones included, without touching what is staged; the
/// difference between two is the turn's change, and restoring one puts its files back.
#[test]
fn snapshots_show_a_turns_changes_and_put_files_back() {
    let dir = repository();
    dir.child("kept.txt").write_str("one\n").unwrap();
    run(&dir, &["add", "kept.txt"]);
    run(&dir, &["commit", "--quiet", "-m", "start"]);
    dir.child("kept.txt").write_str("staged\n").unwrap();
    run(&dir, &["add", "kept.txt"]);
    dir.child("untracked.txt").write_str("loose\n").unwrap();
    let runner = runner::RepositoryRunner;
    let root = dir.path().to_str().unwrap();
    let space = checkpoint::namespace("conversation 1");
    let before = checkpoint::capture(&runner, root, &format!("{space}/before"))
        .unwrap()
        .unwrap();
    assert_eq!(
        run(&dir, &["diff", "--cached", "--name-only"]),
        "kept.txt\n"
    );
    // The turn changes a file, removes the loose one and adds another.
    dir.child("kept.txt").write_str("turned\n").unwrap();
    std::fs::remove_file(dir.child("untracked.txt").path()).unwrap();
    dir.child("added.txt").write_str("new\n").unwrap();
    let after = checkpoint::capture(&runner, root, &format!("{space}/after"))
        .unwrap()
        .unwrap();
    let changes = checkpoint::diff(&runner, root, &before, &after).unwrap();
    for change in ["+turned", "-loose", "+new"] {
        assert!(changes.contains(change), "{change} in {changes}");
    }
    checkpoint::restore(&runner, root, &before).unwrap();
    let read = |name: &str| std::fs::read_to_string(dir.child(name).path()).ok();
    assert_eq!(
        (read("kept.txt"), read("untracked.txt"), read("added.txt")),
        (Some("staged\n".into()), Some("loose\n".into()), None)
    );
    assert_eq!(run(&dir, &["diff", "--cached", "--name-only"]), "");
    checkpoint::prune(&runner, root, &space).unwrap();
    assert_eq!(run(&dir, &["for-each-ref", "refs/bootty/"]), "");
    assert!(
        std::fs::read_dir(dir.path().join(".git"))
            .unwrap()
            .all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("bootty-")
            }),
        "snapshot scratch indexes are removed on their execution host"
    );
}

/// Outside Git there is nothing to snapshot.
#[test]
fn directories_outside_git_have_no_snapshots() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    assert_eq!(
        checkpoint::capture(
            &runner::RepositoryRunner,
            root,
            "refs/bootty/checkpoints/x/y"
        ),
        Ok(None)
    );
}

/// A file that disappears while Git walks the tree, such as a database's journal, costs one
/// walk, not the turn's snapshot.
#[test]
fn a_file_lost_mid_walk_still_leaves_a_snapshot() {
    let dir = repository();
    dir.child("kept.txt").write_str("one\n").unwrap();
    let runner = VanishingFile {
        lost: Cell::new(false),
    };
    let root = dir.path().to_str().unwrap();
    let snapshot = checkpoint::capture(&runner, root, "refs/bootty/checkpoints/x/after")
        .unwrap()
        .unwrap();
    assert_eq!(
        run(&dir, &["ls-tree", "--name-only", &snapshot]),
        "kept.txt\n"
    );
}
