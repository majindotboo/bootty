#![cfg(test)]

use std::process::Command;

use assert_fs::{TempDir, prelude::*};
use bootty_git::{Git, changes::ChangeGroup};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[fixture]
fn repository() -> anyhow::Result<TempDir> {
    let dir = TempDir::new()?;
    run(&dir, &["init", "--quiet"])?;
    run(&dir, &["config", "user.name", "Bootty Test"])?;
    run(&dir, &["config", "user.email", "test@example.invalid"])?;
    run(&dir, &["config", "commit.gpgsign", "false"])?;
    // Keep machine-global hooks out of the temporary repository.
    run(&dir, &["config", "core.hooksPath", "/dev/null"])?;
    Ok(dir)
}

fn run(dir: &TempDir, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[rstest]
fn index_changes_leave_worktree_content_intact(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    repository.child("a.txt").write_str("first\n").unwrap();
    assert_eq!(
        git.changes(root).unwrap().files[0]
            .groups()
            .collect::<Vec<_>>(),
        [ChangeGroup::Untracked]
    );
    assert!(
        git.file_diff(root, "a.txt", ChangeGroup::Untracked)
            .unwrap()
            .contains("+first")
    );
    git.stage_file(root, "a.txt").unwrap();
    repository
        .child("a.txt")
        .write_str("unstaged edit\n")
        .unwrap();
    git.unstage_file(root, "a.txt").unwrap();
    repository.child("a.txt").assert("unstaged edit\n");
    repository.child("a.txt").write_str("first\n").unwrap();
    git.stage_file(root, "a.txt").unwrap();
    git.commit_index(root, "initial\n\nbody", false).unwrap();
    repository.child("a.txt").write_str("second\n").unwrap();
    git.stage_file(root, "a.txt").unwrap();
    repository.child("a.txt").write_str("third\n").unwrap();
    assert!(
        git.file_diff(root, "a.txt", ChangeGroup::Staged)
            .unwrap()
            .contains("+second")
    );
    assert!(
        git.file_diff(root, "a.txt", ChangeGroup::Unstaged)
            .unwrap()
            .contains("+third")
    );
    git.commit_index(root, "amended", true).unwrap();
    assert_eq!(
        run(&repository, &["show", "HEAD:a.txt"]).expect("git command"),
        "second\n"
    );
    repository.child("a.txt").assert("third\n");
    assert_eq!(
        run(&repository, &["rev-list", "--count", "HEAD"])
            .expect("git command")
            .trim(),
        "1"
    );
}

#[rstest]
fn filenames_are_literal_and_renames_unstage_both_paths(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    let special = "file [one]\nname.txt";
    repository.child(special).write_str("content\n").unwrap();
    repository
        .child("unrelated.txt")
        .write_str("keep\n")
        .unwrap();
    git.stage_file(root, special).unwrap();
    git.commit_index(root, "initial", false).unwrap();
    run(&repository, &["mv", special, "renamed.txt"]).expect("git command");
    let changes = git.changes(root).unwrap();
    assert_eq!(
        changes
            .files
            .iter()
            .find(|file| file.path == "renamed.txt")
            .unwrap()
            .previous_path
            .as_deref(),
        Some(special)
    );
    repository
        .child("renamed.txt")
        .write_str("content\nmore\n")
        .unwrap();
    git.stage_file(root, "renamed.txt").unwrap();
    git.unstage_file(root, "renamed.txt").unwrap();
    assert_eq!(
        run(&repository, &["diff", "--cached", "--name-only"]).expect("git command"),
        ""
    );
    repository.child("renamed.txt").assert("content\nmore\n");
    assert!(git.stage_file(root, "*").is_err());
    assert!(git.stage_file(root, "../outside").is_err());
}

fn git_in(cwd: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_file(clone: &std::path::Path, name: &str) {
    std::fs::write(clone.join(name), name).expect("write file");
    git_in(clone, &["add", name]);
    git_in(clone, &["commit", "-q", "-m", name]);
}

/// Two clones of one origin: changes report how far each branch is from its upstream, and fetch,
/// pull and push close the distance without ever merging.
#[rstest]
fn sync_reports_and_closes_the_distance_to_the_upstream() {
    let dir = TempDir::new().unwrap();
    git_in(
        dir.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    let clones = ["mine", "theirs"].map(|name| dir.path().join(name));
    for clone in &clones {
        git_in(
            dir.path(),
            &["clone", "-q", "origin.git", clone.to_str().unwrap()],
        );
        for (key, value) in [
            ("user.name", "Bootty Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
            ("init.defaultBranch", "main"),
        ] {
            git_in(clone, &["config", key, value]);
        }
    }
    let [mine, theirs] = clones.map(|clone| clone.to_str().unwrap().to_owned());
    let git = Git::new();
    let distance = |clone: &str| {
        git.changes(clone)
            .unwrap()
            .upstream
            .map(|upstream| (upstream.name, upstream.ahead, upstream.behind))
    };
    commit_file(std::path::Path::new(&mine), "first");
    git_in(
        std::path::Path::new(&mine),
        &["push", "-q", "-u", "origin", "main"],
    );
    git.pull_upstream(&theirs).unwrap();

    commit_file(std::path::Path::new(&mine), "second");
    assert_eq!(distance(&mine), Some(("origin/main".to_owned(), 1, 0)));
    git.push_upstream(&mine).unwrap();
    assert_eq!(distance(&mine), Some(("origin/main".to_owned(), 0, 0)));
    assert_eq!(distance(&theirs), Some(("origin/main".to_owned(), 0, 0)));
    git.fetch_remote(&theirs).unwrap();
    assert_eq!(distance(&theirs), Some(("origin/main".to_owned(), 0, 1)));
    git.pull_upstream(&theirs).unwrap();
    assert!(std::path::Path::new(&theirs).join("second").exists());

    commit_file(std::path::Path::new(&theirs), "their-change");
    commit_file(std::path::Path::new(&mine), "my-change");
    git.push_upstream(&mine).unwrap();
    git.fetch_remote(&theirs).unwrap();
    assert_eq!(distance(&theirs), Some(("origin/main".to_owned(), 1, 1)));
    assert!(
        git.pull_upstream(&theirs).is_err(),
        "diverged branches never merge"
    );
    assert!(
        git.push_upstream(&theirs).is_err(),
        "a push never overwrites"
    );

    git_in(
        std::path::Path::new(&mine),
        &["switch", "-q", "-c", "local-only"],
    );
    assert_eq!(distance(&mine), None);
}
