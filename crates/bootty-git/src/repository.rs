use std::path::Path;

use crate::{
    project::session_name_for_path,
    runner::{CommandRunner, SystemCommandRunner},
};

/// A read-only Git service scoped to an execution host.
///
/// Git policy lives here while command execution is injected by the host. The
/// default constructor is the local executable used by desktop transitions;
/// remote callers supply a host runner with the same argv.
#[derive(Clone, Debug)]
pub struct Git<R = SystemCommandRunner> {
    pub(crate) runner: R,
}

impl Git<SystemCommandRunner> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            runner: SystemCommandRunner,
        }
    }
}

impl Default for Git<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: CommandRunner> Git<R> {
    pub const fn with_runner(runner: R) -> Self {
        Self { runner }
    }

    fn is_linked_checkout(&self, cwd: &str) -> bool {
        let git_dir = self.read(cwd, &["rev-parse", "--absolute-git-dir"]);
        let common = self.read(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        );
        git_dir.is_some() && common.is_some() && git_dir != common
    }

    /// The main checkout that owns the linked checkout containing `cwd`, so its sessions group
    /// with their project. `None` for a main checkout, a bare repository's checkouts, or a
    /// non-repository.
    pub fn linked_checkout_project(&self, cwd: &str) -> Option<String> {
        if !self.is_linked_checkout(cwd) {
            return None;
        }
        let main = self.main_checkout(cwd)?;
        (self
            .read(&main, &["rev-parse", "--is-inside-work-tree"])
            .as_deref()
            == Some("true"))
        .then_some(main)
    }

    pub fn checkout_root(&self, cwd: &str) -> Option<String> {
        self.read(cwd, &["rev-parse", "--show-toplevel"])
    }

    /// Return the branch name, or the stable seven-character detached-head
    /// label used by the built-in session presentation.
    pub fn head_branch(&self, cwd: &str) -> Option<String> {
        if let Some(branch) = self.read(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
            return Some(branch);
        }
        let commit = self.read(cwd, &["rev-parse", "HEAD"])?;
        let commit = commit.get(..7).unwrap_or(&commit);
        (!commit.is_empty()).then(|| format!("detached {commit}"))
    }

    /// Count added and removed lines in the checkout's diff. Binary entries are
    /// ignored, matching Git's `--numstat` output and the previous sidebar.
    pub fn diff_counts(&self, cwd: &str) -> Option<(u64, u64)> {
        let output = self.read(cwd, &["diff", "HEAD", "--numstat"])?;
        let mut added = 0u64;
        let mut removed = 0u64;
        for line in output.lines() {
            let mut columns = line.split('\t');
            let Some(add) = columns.next().and_then(|value| value.parse::<u64>().ok()) else {
                continue;
            };
            let Some(remove) = columns.next().and_then(|value| value.parse::<u64>().ok()) else {
                continue;
            };
            added = added.saturating_add(add);
            removed = removed.saturating_add(remove);
        }
        (added != 0 || removed != 0).then_some((added, removed))
    }

    /// Suggest a grouped session name for a checkout, or a basename for a
    /// plain directory.
    pub fn suggested_session_name(&self, cwd: &str) -> String {
        let Some(checkout) = self.checkout_root(cwd) else {
            return session_name_for_path(cwd).to_owned();
        };
        let branch = self.read(&checkout, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let group = self.main_checkout(&checkout).as_deref().map_or_else(
            || session_name_for_path(&checkout).to_owned(),
            |path| session_name_for_path(path).to_owned(),
        );
        let leaf = branch
            .as_deref()
            .and_then(|branch| branch.rsplit('/').next())
            .filter(|branch| !branch.is_empty())
            .map_or_else(
                || session_name_for_path(&checkout).to_owned(),
                str::to_owned,
            );
        format!("{group}/{leaf}")
    }

    /// The repository's main checkout. Git lists it first; in a bare repository this is the Git
    /// directory itself, which callers must not treat as a checkout.
    fn main_checkout(&self, cwd: &str) -> Option<String> {
        let output = self
            .runner
            .run(
                "git",
                &git_args(cwd, &["worktree", "list", "--porcelain", "-z"]),
            )
            .ok()
            .filter(|output| output.success)?;
        // The runner transports UTF-8 text; never act on a lossy path.
        if output.stdout.contains('\u{fffd}') {
            return None;
        }
        output
            .stdout
            .split('\0')
            .next()?
            .strip_prefix("worktree ")
            .map(str::to_owned)
    }

    fn read(&self, cwd: &str, args: &[&str]) -> Option<String> {
        self.runner
            .run("git", &git_args(cwd, args))
            .ok()
            .filter(|output| output.success)
            .map(|output| {
                output
                    .stdout
                    .strip_suffix('\n')
                    .unwrap_or(&output.stdout)
                    .to_owned()
            })
    }
}

fn git_args(cwd: &str, args: &[&str]) -> Vec<String> {
    let mut output = Vec::with_capacity(args.len().saturating_add(2));
    output.push("-C".to_owned());
    output.push(cwd.to_owned());
    output.extend(args.iter().map(|arg| (*arg).to_owned()));
    output
}

#[must_use]
pub fn checkout_root(cwd: &str) -> Option<String> {
    local(cwd)?.checkout_root(cwd)
}

#[must_use]
pub fn suggested_session_name(cwd: &str) -> String {
    local(cwd).map_or_else(
        || session_name_for_path(cwd).to_owned(),
        |git| git.suggested_session_name(cwd),
    )
}

/// Local Git for `cwd`, or `None` where Git could find no work tree, so plain directories on
/// the frame path start no process. An ancestor must hold `.git`, or the environment must name
/// the Git directory.
fn local(cwd: &str) -> Option<Git> {
    let directory = Path::new(cwd);
    (directory.is_relative()
        || std::env::var_os("GIT_DIR").is_some()
        || directory.ancestors().any(|dir| dir.join(".git").exists()))
    .then(Git::new)
}
