//! Snapshots of a work tree around an agent's turns.
//!
//! A snapshot keeps every file, untracked ones included, as a commit under a private ref, leaving
//! the index and the files alone. A turn's changes are the difference between two snapshots, and
//! rewinding puts a snapshot's files back.

use crate::CommandRunner;

/// Where a conversation's snapshots live, so Git keeps them and deleting the conversation can
/// let them go.
#[must_use]
pub fn namespace(conversation: &str) -> String {
    let safe = conversation
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("refs/bootty/checkpoints/{safe}")
}

/// Records the work tree at `directory` as a commit kept at `reference`, and returns it. Outside
/// a Git work tree there is nothing to record.
/// # Errors
/// Returns a Git failure while recording.
pub fn capture(
    runner: &impl CommandRunner,
    directory: &str,
    reference: &str,
) -> Result<Option<String>, String> {
    let Some(root) = root(runner, directory) else {
        return Ok(None);
    };
    // A scratch index, one per snapshot so conversations can record at once, keeps the person's
    // staged changes as they are.
    let index = git(
        runner,
        &root,
        &[
            "rev-parse",
            "--git-path",
            &format!("bootty-{}.index", reference.replace('/', "-")),
        ],
    )?;
    let index = if index.starts_with('/') {
        index
    } else {
        format!("{root}/{index}")
    };
    let scratch = |args: &[&str]| {
        let mut command = vec![format!("GIT_INDEX_FILE={index}"), "git".to_owned()];
        command.extend(
            ["-C", &root]
                .iter()
                .chain(args)
                .map(|arg| (*arg).to_owned()),
        );
        output(runner.run("env", &command))
    };
    let head = git(runner, &root, &["rev-parse", "--verify", "-q", "HEAD"]).ok();
    let commit = (|| {
        match &head {
            Some(head) => scratch(&["read-tree", head])?,
            None => scratch(&["read-tree", "--empty"])?,
        };
        scratch(&["add", "-A"])?;
        let tree = scratch(&["write-tree"])?;
        let mut commit = vec![
            "-c",
            "user.name=Bootty",
            "-c",
            "user.email=checkpoint@bootty.invalid",
            "commit-tree",
            tree.as_str(),
            "-m",
            "Bootty checkpoint",
        ];
        if let Some(head) = &head {
            commit.extend(["-p", head.as_str()]);
        }
        git(runner, &root, &commit)
    })();
    _ = runner.run("rm", &["-f".to_owned(), index]);
    let commit = commit?;
    git(runner, &root, &["update-ref", reference, &commit])?;
    Ok(Some(commit))
}

/// The changes from snapshot `from` to snapshot `to`, as a Git diff.
/// # Errors
/// Returns a directory outside Git, or a snapshot Git no longer has.
pub fn diff(
    runner: &impl CommandRunner,
    directory: &str,
    from: &str,
    to: &str,
) -> Result<String, String> {
    let root = root(runner, directory).ok_or("This directory is no longer in Git")?;
    git(
        runner,
        &root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            from,
            to,
        ],
    )
    .map(|diff| format!("{diff}\n"))
}

/// Puts the files of snapshot `commit` back: changed files return, and files it didn't have go.
/// Ignored files stay, and nothing is left staged.
/// # Errors
/// Returns a directory outside Git, or a Git failure partway.
pub fn restore(runner: &impl CommandRunner, directory: &str, commit: &str) -> Result<(), String> {
    let root = root(runner, directory).ok_or("This directory is no longer in Git")?;
    git(
        runner,
        &root,
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
    )
    .map_err(|_| "That snapshot is no longer in this repository".to_owned())?;
    git(
        runner,
        &root,
        &[
            "restore",
            &format!("--source={commit}"),
            "--staged",
            "--worktree",
            "--",
            ":/",
        ],
    )?;
    git(runner, &root, &["clean", "-fd", "--", ":/"])?;
    git(runner, &root, &["reset", "-q"])?;
    Ok(())
}

/// Lets go of every snapshot kept under `namespace`.
/// # Errors
/// Returns a Git failure while listing or deleting them.
pub fn prune(runner: &impl CommandRunner, directory: &str, namespace: &str) -> Result<(), String> {
    let Some(root) = root(runner, directory) else {
        return Ok(());
    };
    let refs = git(
        runner,
        &root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("{namespace}/"),
        ],
    )?;
    for reference in refs.lines().filter(|line| !line.is_empty()) {
        git(runner, &root, &["update-ref", "-d", reference])?;
    }
    Ok(())
}

fn root(runner: &impl CommandRunner, directory: &str) -> Option<String> {
    git(runner, directory, &["rev-parse", "--show-toplevel"]).ok()
}

fn git(runner: &impl CommandRunner, root: &str, args: &[&str]) -> Result<String, String> {
    let args = ["-C", root]
        .iter()
        .chain(args)
        .map(|arg| (*arg).to_owned())
        .collect::<Vec<_>>();
    output(runner.run("git", &args))
}

fn output<E: std::fmt::Display>(result: Result<crate::CommandOutput, E>) -> Result<String, String> {
    let output = result.map_err(|error| error.to_string())?;
    if output.success {
        Ok(output.stdout.trim_end().to_owned())
    } else {
        Err(output.stderr.trim().to_owned())
    }
}
