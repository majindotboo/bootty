#![cfg(test)]

use std::process::Command;

use assert_fs::{TempDir, prelude::*};
use bootty_git::{
    Git,
    changes::{ChangeGroup, LineCounts},
};
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
    let special = if cfg!(windows) {
        "file [one] & name.txt"
    } else {
        "file [one]\nname.txt"
    };
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

/// Each changed file counts the lines its staged and unstaged changes add and remove, a rename
/// under its new name; a binary file counts none.
#[rstest]
fn changes_count_each_sides_lines(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    repository
        .child("notes.txt")
        .write_str("one\ntwo\nthree\n")
        .unwrap();
    repository.child("old.txt").write_str("a\nb\n").unwrap();
    repository
        .child("image.bin")
        .write_binary(&[0, 1, 2])
        .unwrap();
    run(&repository, &["add", "."]).expect("git command");
    git.commit_index(root, "initial", false).unwrap();
    repository
        .child("notes.txt")
        .write_str("one\ntwo\nthree\nfour\nfive\n")
        .unwrap();
    git.stage_file(root, "notes.txt").unwrap();
    repository
        .child("notes.txt")
        .write_str("two\nthree\nfour\nfive\n")
        .unwrap();
    run(&repository, &["mv", "old.txt", "new.txt"]).expect("git command");
    repository.child("new.txt").write_str("a\nb\nc\n").unwrap();
    git.stage_file(root, "new.txt").unwrap();
    repository
        .child("image.bin")
        .write_binary(&[3, 0, 4])
        .unwrap();
    let changes = git.changes(root).unwrap();
    let lines = |path: &str| {
        let file = changes.files.iter().find(|file| file.path == path).unwrap();
        (file.staged_lines, file.unstaged_lines)
    };
    let counts = |added, removed| Some(LineCounts { added, removed });
    assert_eq!(lines("notes.txt"), (counts(2, 0), counts(0, 1)));
    assert_eq!(lines("new.txt"), (counts(1, 0), None));
    assert_eq!(lines("image.bin"), (None, None));
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

/// Everything stages and unstages at once, one hunk stages and unstages on its own, and changes
/// can be thrown away file by file or all together.
#[rstest]
fn changes_stage_together_by_hunk_and_discard(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    let lines = |changed: &[usize]| {
        (1..=30)
            .map(|n| {
                if changed.contains(&n) {
                    format!("changed {n}\n")
                } else {
                    format!("line {n}\n")
                }
            })
            .collect::<String>()
    };
    repository.child("kept.txt").write_str(&lines(&[])).unwrap();
    run(&repository, &["add", "kept.txt"]).unwrap();
    run(&repository, &["commit", "--quiet", "-m", "start"]).unwrap();
    // Two changes far apart make two hunks.
    repository
        .child("kept.txt")
        .write_str(&lines(&[2, 28]))
        .unwrap();
    repository.child("loose.txt").write_str("new\n").unwrap();
    let staged = || run(&repository, &["diff", "--cached", "--name-only"]).unwrap();
    git.stage_all(root).unwrap();
    assert_eq!(staged(), "kept.txt\nloose.txt\n");
    git.unstage_all(root).unwrap();
    assert_eq!(staged(), "");
    let diff = git
        .file_diff(root, "kept.txt", ChangeGroup::Unstaged)
        .unwrap();
    let file = bootty_git::diff::FileDiff::parse_all(&diff).remove(0);
    let first = bootty_git::diff::DiffAnchor {
        path: "kept.txt".into(),
        side: bootty_git::diff::DiffSide::Right,
        start_line: 2,
        line: 2,
    };
    let patch = file.hunk_patch(&first).unwrap();
    git.apply_to_index(root, &patch, false).unwrap();
    let cached = run(&repository, &["diff", "--cached"]).unwrap();
    assert!(
        cached.contains("+changed 2") && !cached.contains("+changed 28"),
        "{cached}"
    );
    git.apply_to_index(root, &patch, true).unwrap();
    assert_eq!(staged(), "");
    git.discard_file(root, "loose.txt").unwrap();
    assert!(!repository.child("loose.txt").path().exists());
    git.discard_file(root, "kept.txt").unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.child("kept.txt").path()).unwrap(),
        lines(&[])
    );
    repository
        .child("kept.txt")
        .write_str(&lines(&[5]))
        .unwrap();
    repository.child("other.txt").write_str("loose\n").unwrap();
    git.stage_all(root).unwrap();
    git.discard_all(root).unwrap();
    assert_eq!(run(&repository, &["status", "--porcelain"]).unwrap(), "");
}

/// One diff holds every change and each untracked file, and can leave out changes to
/// whitespace alone.
#[rstest]
fn all_changes_diff_together_and_can_hide_whitespace(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    repository
        .child("spaced.txt")
        .write_str("one two\n")
        .unwrap();
    repository.child("staged.txt").write_str("old\n").unwrap();
    run(&repository, &["add", "-A"]).unwrap();
    run(&repository, &["commit", "--quiet", "-m", "start"]).unwrap();
    repository
        .child("spaced.txt")
        .write_str("one  two\n")
        .unwrap();
    repository.child("staged.txt").write_str("new\n").unwrap();
    run(&repository, &["add", "staged.txt"]).unwrap();
    repository.child("loose.txt").write_str("loose\n").unwrap();
    let all = git.all_diff(root, false).unwrap();
    for file in ["b/spaced.txt", "b/staged.txt", "b/loose.txt"] {
        assert!(all.contains(file), "{file} in {all}");
    }
    let quiet = git.all_diff(root, true).unwrap();
    assert!(
        !quiet.contains("+one  two") && quiet.contains("+new"),
        "{quiet}"
    );
}

/// A commit message is written from the staged changes alone, after the latest subjects; with
/// nothing staged there is nothing to describe.
#[rstest]
fn commit_messages_describe_only_what_is_staged(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    repository.child("notes.txt").write_str("one\n").unwrap();
    run(&repository, &["add", "notes.txt"]).unwrap();
    run(
        &repository,
        &["commit", "--quiet", "-m", "feat: start notes"],
    )
    .unwrap();
    assert_eq!(
        git.commit_message_context(root),
        Err("Stage the changes to describe first".to_owned())
    );
    repository
        .child("notes.txt")
        .write_str("one\ntwo\n")
        .unwrap();
    repository
        .child("loose.txt")
        .write_str("unstaged\n")
        .unwrap();
    run(&repository, &["add", "notes.txt"]).unwrap();
    let context = git.commit_message_context(root).unwrap();
    assert!(
        context.starts_with("Recent commits:\nfeat: start notes\n"),
        "{context}"
    );
    assert!(context.contains("+two"), "{context}");
    assert!(!context.contains("loose.txt"), "{context}");
}

/// A pull request is written from the branch's commits since it left the base, and their
/// changes; a base Git doesn't know, or a branch with nothing new, says so.
#[rstest]
fn pull_requests_describe_the_branch_since_its_base(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    repository.child("notes.txt").write_str("one\n").unwrap();
    run(&repository, &["add", "notes.txt"]).unwrap();
    run(&repository, &["commit", "--quiet", "-m", "start"]).unwrap();
    run(&repository, &["branch", "-M", "main"]).unwrap();
    run(&repository, &["switch", "--quiet", "-c", "feature"]).unwrap();
    assert_eq!(
        git.pull_request_context(root, "main"),
        Err("This branch has no commits to describe yet".to_owned())
    );
    assert_eq!(
        git.pull_request_context(root, "missing"),
        Err("Git doesn't know the branch missing".to_owned())
    );
    repository
        .child("notes.txt")
        .write_str("one\ntwo\n")
        .unwrap();
    run(
        &repository,
        &["commit", "--quiet", "-am", "Add a second note"],
    )
    .unwrap();
    let context = git.pull_request_context(root, "main").unwrap();
    assert!(
        context.starts_with("Commits:\nAdd a second note\n"),
        "{context}"
    );
    assert!(
        context.contains("+two") && !context.contains("start"),
        "{context}"
    );
}

/// Each branch reports how far it is from its upstream, and switching away from changes stashes
/// them, untracked files included, under a message naming both branches.
#[rstest]
fn branches_report_their_distance_and_switch_by_stashing(repository: anyhow::Result<TempDir>) {
    let repository = repository.expect("repository fixture");
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    repository.child("notes.txt").write_str("one\n").unwrap();
    run(&repository, &["add", "notes.txt"]).unwrap();
    run(&repository, &["commit", "--quiet", "-m", "start"]).unwrap();
    run(&repository, &["branch", "-M", "main"]).unwrap();
    run(&repository, &["switch", "--quiet", "-c", "feature"]).unwrap();
    run(
        &repository,
        &["branch", "--quiet", "--set-upstream-to=main"],
    )
    .unwrap();
    repository
        .child("notes.txt")
        .write_str("one\ntwo\n")
        .unwrap();
    run(&repository, &["commit", "--quiet", "-am", "two"]).unwrap();
    let feature = git
        .overview(root, 1)
        .unwrap()
        .branches
        .into_iter()
        .find(|branch| branch.name == "feature")
        .unwrap();
    assert_eq!((feature.ahead, feature.behind), (1, 0));
    assert_eq!(
        git.stash_and_switch(root, "missing"),
        Err("branch is not a local repository branch".to_owned())
    );
    repository
        .child("notes.txt")
        .write_str("changed\n")
        .unwrap();
    repository.child("loose.txt").write_str("new\n").unwrap();
    git.stash_and_switch(root, "main").unwrap();
    let changes = git.changes(root).unwrap();
    assert_eq!(
        (changes.branch.as_deref(), changes.files.len()),
        (Some("main"), 0)
    );
    assert_eq!(
        run(&repository, &["stash", "list", "--format=%s"]).unwrap(),
        "On feature: Stashed switching from feature to main\n"
    );
}

/// Selected hunks remain applicable for new/deleted files and either side's missing EOF newline.
#[rstest]
#[case("new", ChangeGroup::Untracked, false)]
#[case("modified", ChangeGroup::Unstaged, false)]
#[case("deleted", ChangeGroup::Unstaged, true)]
fn hunk_staging_preserves_file_headers_and_eof(
    repository: anyhow::Result<TempDir>,
    #[case] change: &str,
    #[case] group: ChangeGroup,
    #[case] deleted: bool,
) {
    use bootty_git::diff::{DiffAnchor, DiffSide, FileDiff};
    let repository = repository.unwrap();
    let git = Git::new();
    let root = repository.path().to_str().unwrap();
    let path = "my notes.txt";
    repository.child("kept.txt").write_str("kept\n").unwrap();
    if change != "new" {
        repository.child(path).write_str("before").unwrap();
    }
    git.stage_all(root).unwrap();
    git.commit_index(root, "initial", false).unwrap();
    if deleted {
        std::fs::remove_file(repository.child(path).path()).unwrap();
    } else {
        repository.child(path).write_str("after").unwrap();
    }
    let source = git.file_diff(root, path, group).unwrap();
    let file = FileDiff::parse_all(&source).remove(0);
    let patch = file
        .hunk_patch(&DiffAnchor {
            path: path.to_owned(),
            side: if deleted {
                DiffSide::Left
            } else {
                DiffSide::Right
            },
            start_line: 1,
            line: 1,
        })
        .unwrap();
    git.apply_to_index(root, &patch, false).unwrap();
    if deleted {
        assert_eq!(run(&repository, &["ls-files", path]).unwrap(), "");
    } else {
        assert_eq!(
            run(&repository, &["show", &format!(":{path}")]).unwrap(),
            "after"
        );
        repository.child(path).assert("after");
    }
    git.apply_to_index(root, &patch, true).unwrap();
    assert_eq!(run(&repository, &["diff", "--cached"]).unwrap(), "");
}
