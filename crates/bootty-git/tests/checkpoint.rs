#![cfg(test)]
//! Snapshots around an agent's turns: what a turn changed, and putting files back.

use std::process::Command;

use assert_fs::{TempDir, prelude::*};
use bootty_git::{SystemCommandRunner, checkpoint};
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

/// A snapshot keeps every file, untracked ones included, without touching what is staged; the
/// difference between two is the turn's change, and restoring one puts its files back.
#[test]
fn snapshots_show_a_turns_changes_and_put_files_back() {
    let dir = TempDir::new().unwrap();
    for args in [
        ["init", "--quiet"].as_slice(),
        &["config", "user.name", "Bootty Test"],
        &["config", "user.email", "test@example.invalid"],
        &["config", "commit.gpgsign", "false"],
        &["config", "core.hooksPath", "/dev/null"],
    ] {
        run(&dir, args);
    }
    dir.child("kept.txt").write_str("one\n").unwrap();
    run(&dir, &["add", "kept.txt"]);
    run(&dir, &["commit", "--quiet", "-m", "start"]);
    dir.child("kept.txt").write_str("staged\n").unwrap();
    run(&dir, &["add", "kept.txt"]);
    dir.child("untracked.txt").write_str("loose\n").unwrap();
    let runner = SystemCommandRunner;
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
}

/// Outside Git there is nothing to snapshot.
#[test]
fn directories_outside_git_have_no_snapshots() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    assert_eq!(
        checkpoint::capture(&SystemCommandRunner, root, "refs/bootty/checkpoints/x/y"),
        Ok(None)
    );
}
