use std::path::Path;

use crate::{
    project::{WorktreePickerEntry, main_worktree_entry, session_name_for_path},
    runner::{CommandRunner, SystemCommandRunner},
};

/// A new branch and sibling checkout, resolved on the repository's host.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRequest {
    pub branch: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub start_ref: Option<String>,
}
impl WorktreeRequest {
    /// # Errors
    /// Returns an error for an invalid branch, folder name, or starting reference.
    pub fn validate(&self) -> Result<(), String> {
        if self.branch.trim().is_empty()
            || self.branch.starts_with('-')
            || self.branch == "HEAD"
            || self.branch.chars().any(char::is_control)
        {
            return Err("Enter a valid branch name".to_owned());
        }
        if let Some(name) = &self.name
            && (name.trim().is_empty()
                || matches!(name.as_str(), "." | "..")
                || name.contains(['/', '\\'])
                || name.ends_with([' ', '.'])
                || !matches!(
                    Path::new(name).components().next(),
                    Some(std::path::Component::Normal(_))
                )
                || name.chars().any(char::is_control))
        {
            return Err("Folder name must be one directory name".to_owned());
        }
        if self
            .start_ref
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.chars().any(char::is_control))
        {
            return Err("Enter a starting branch, tag or commit".to_owned());
        }
        Ok(())
    }
}

/// A Git command service scoped to an execution host.
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

    /// Inspect the Git state of `cwd`. Git failures produce an empty status so
    /// callers only offer the safe session close action.
    pub fn status(&self, cwd: &str) -> WorktreeStatus {
        let mut status = WorktreeStatus::default();
        if self
            .read(cwd, &["rev-parse", "--is-inside-work-tree"])
            .as_deref()
            != Some("true")
        {
            return status;
        }
        status.in_repo = true;
        if let (Some(git_dir), Some(common)) = (
            self.read(cwd, &["rev-parse", "--absolute-git-dir"]),
            self.read(
                cwd,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ),
        ) {
            status.is_linked_worktree = git_dir != common;
        }
        status.branch = self.read(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        status.dirty = self
            .read(cwd, &["status", "--porcelain"])
            .is_some_and(|out| !out.is_empty());
        if let Some(count) = self
            .read(cwd, &["rev-list", "--count", "@{u}..HEAD"])
            .and_then(|out| out.parse().ok())
        {
            status.has_upstream = true;
            status.unpushed = count;
        }
        status
    }

    /// Detach HEAD while retaining the worktree and all commits.
    /// # Errors
    /// Returns an error if Git cannot detach the worktree HEAD.
    pub fn detach_head(&self, worktree_path: &str) -> Result<(), String> {
        self.run(worktree_path, &["checkout", "--detach"])
    }

    /// Count the main and linked worktrees for the repository containing `cwd`.
    pub fn worktree_count(&self, cwd: &str) -> usize {
        self.worktree_entries(cwd)
            .map_or(0, |entries| entries.len())
    }

    /// Resolve the repository's default branch, falling back to the main
    /// worktree branch when `origin/HEAD` is unavailable.
    pub fn trunk_branch(&self, cwd: &str) -> Option<String> {
        self.read(
            cwd,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        )
        .and_then(|head| head.strip_prefix("refs/remotes/origin/").map(str::to_owned))
        .or_else(|| {
            let main = self.main_worktree(cwd)?;
            self.read(&main, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        })
    }

    /// Remove a linked worktree from its main worktree. `force` is required for
    /// a dirty worktree and is never inferred by this service.
    /// # Errors
    /// Returns an error if the main worktree cannot be located or Git refuses removal.
    pub fn remove_worktree(&self, worktree_path: &str, force: bool) -> Result<(), String> {
        let main = self
            .main_worktree(worktree_path)
            .ok_or_else(|| "could not locate the main worktree".to_owned())?;
        let mut args = vec!["worktree".to_owned(), "remove".to_owned()];
        if force {
            args.push("--force".to_owned());
        }
        args.push("--".to_owned());
        args.push(worktree_path.to_owned());
        self.run_args(&main, &args)
    }

    /// Delete a branch from a live repository. Force maps exactly to `branch -D`.
    /// # Errors
    /// Returns an error if Git refuses to delete the branch.
    pub fn delete_branch(&self, repo_dir: &str, branch: &str, force: bool) -> Result<(), String> {
        self.run(
            repo_dir,
            &["branch", if force { "-D" } else { "-d" }, "--", branch],
        )
    }

    pub fn worktree_root(&self, cwd: &str) -> Option<String> {
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

    /// Count added and removed lines in the worktree diff. Binary entries are
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

    /// Suggest a grouped session name for a worktree, or a basename for a
    /// plain directory.
    pub fn suggested_session_name(&self, cwd: &str) -> String {
        let Some(worktree) = self.worktree_root(cwd) else {
            return session_name_for_path(cwd).to_owned();
        };
        let branch = self.read(&worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let group = self.main_worktree(&worktree).as_deref().map_or_else(
            || session_name_for_path(&worktree).to_owned(),
            |path| session_name_for_path(path).to_owned(),
        );
        let leaf = branch
            .as_deref()
            .and_then(|branch| branch.rsplit('/').next())
            .filter(|branch| !branch.is_empty())
            .map_or_else(
                || session_name_for_path(&worktree).to_owned(),
                str::to_owned,
            );
        format!("{group}/{leaf}")
    }

    pub fn discover_worktree_picker_entries(&self, project_path: &str) -> Vec<WorktreePickerEntry> {
        let new_worktree = WorktreePickerEntry {
            label: "New worktree".to_owned(),
            is_new: true,
            ..WorktreePickerEntry::default()
        };
        let Some(worktrees) = self.worktree_entries(project_path) else {
            return vec![main_worktree_entry(project_path)];
        };
        let mut entries = vec![new_worktree];
        entries.extend(worktrees);
        entries
    }

    pub fn mark_occupied_worktrees(
        &self,
        entries: &mut [WorktreePickerEntry],
        open_cwds: &[String],
    ) {
        let open = open_cwds
            .iter()
            .filter_map(|path| path_identity(path))
            .collect::<std::collections::HashSet<_>>();
        for entry in entries {
            entry.occupied = entry
                .path
                .as_deref()
                .and_then(path_identity)
                .is_some_and(|identity| open.contains(&identity));
        }
    }

    /// # Errors
    /// Returns an error if the branch or destination is invalid, or Git cannot create the checkout and branch.
    pub fn add_worktree(&self, repo_dir: &str, branch: &str) -> Result<String, String> {
        self.create_worktree(
            repo_dir,
            &WorktreeRequest {
                branch: branch.to_owned(),
                name: None,
                start_ref: None,
            },
        )
    }

    /// # Errors
    /// Returns an error for invalid inputs or failed Git operations.
    /// If branch creation fails, the error reports any failure to remove the new clean checkout.
    pub fn create_worktree(
        &self,
        repo_dir: &str,
        request: &WorktreeRequest,
    ) -> Result<String, String> {
        request.validate()?;
        if self
            .read(
                repo_dir,
                &[
                    "check-ref-format",
                    &format!("refs/heads/{}", request.branch),
                ],
            )
            .is_none()
        {
            return Err("Invalid branch name".to_owned());
        }
        // Resolve once so a moving branch cannot change the requested starting commit mid-create.
        let start_ref = request.start_ref.as_deref().unwrap_or("HEAD");
        let commit = self
            .read(
                repo_dir,
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{start_ref}^{{commit}}"),
                ],
            )
            .ok_or_else(|| format!("Starting ref {start_ref:?} does not resolve to a commit"))?;
        let path = self.new_worktree_path(repo_dir, &request.branch, request.name.as_deref())?;
        let args = vec![
            "worktree".to_owned(),
            "add".to_owned(),
            "--detach".to_owned(),
            "--".to_owned(),
            path.clone(),
            commit,
        ];
        let output = self
            .output_args(repo_dir, &args)
            .map_err(|error| format!("run git: {error}"))?;
        if !output.success {
            return Err(output.stderr.trim().to_owned());
        }
        // Git can create -b's branch before discovering a destination collision. Create the
        // checkout first; on branch failure remove only our clean checkout, never a branch.
        if let Err(error) = self.run(&path, &["checkout", "-b", &request.branch]) {
            return match self.run(repo_dir, &["worktree", "remove", "--", &path]) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; checkout retained at {path}: {cleanup}")),
            };
        }
        Ok(path)
    }

    pub fn main_worktree(&self, cwd: &str) -> Option<String> {
        // Git lists the main repository first. In a bare repository this is the Git
        // directory itself, so its parent is not a repository we can run commands in.
        self.worktree_entries(cwd)?.into_iter().next()?.path
    }

    fn worktree_entries(&self, cwd: &str) -> Option<Vec<WorktreePickerEntry>> {
        let output = self
            .output(cwd, &["worktree", "list", "--porcelain", "-z"])
            .filter(|output| output.success)?;
        // The runner transports UTF-8 text; never act on a lossy worktree path.
        if output.stdout.contains('\u{fffd}') {
            return None;
        }
        Some(parse_git_worktree_list(&output.stdout))
    }

    fn new_worktree_path(
        &self,
        repo_dir: &str,
        branch: &str,
        name: Option<&str>,
    ) -> Result<String, String> {
        let main = self
            .main_worktree(repo_dir)
            .unwrap_or_else(|| repo_dir.to_owned());
        let main = Path::new(&main);
        let parent = main
            .parent()
            .ok_or_else(|| "repository has no parent directory".to_owned())?;
        let repo_name = main
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "could not read repository name".to_owned())?;
        Ok(parent
            .join(name.map_or_else(
                || format!("{}-{}", repo_name, branch.replace('/', "-")),
                str::to_owned,
            ))
            .to_string_lossy()
            .into_owned())
    }

    fn read(&self, cwd: &str, args: &[&str]) -> Option<String> {
        let args = git_args(cwd, args);
        self.runner
            .run("git", &args)
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

    fn run(&self, cwd: &str, args: &[&str]) -> Result<(), String> {
        self.run_args(
            cwd,
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
    }

    fn run_args(&self, cwd: &str, args: &[String]) -> Result<(), String> {
        let output = self.output_args(cwd, args)?;
        output
            .success
            .then_some(())
            .ok_or_else(|| output.stderr.trim().to_owned())
    }

    fn output(&self, cwd: &str, args: &[&str]) -> Option<crate::runner::CommandOutput> {
        self.output_args(
            cwd,
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
        .ok()
    }

    fn output_args(
        &self,
        cwd: &str,
        args: &[String],
    ) -> Result<crate::runner::CommandOutput, String> {
        self.runner
            .run("git", &git_args_owned(cwd, args))
            .map_err(|error| error.to_string())
    }
}

fn git_args(cwd: &str, args: &[&str]) -> Vec<String> {
    let mut output = Vec::with_capacity(args.len().saturating_add(2));
    output.push("-C".to_owned());
    output.push(cwd.to_owned());
    output.extend(args.iter().map(|arg| (*arg).to_owned()));
    output
}

fn git_args_owned(cwd: &str, args: &[String]) -> Vec<String> {
    let mut output = Vec::with_capacity(args.len().saturating_add(2));
    output.push("-C".to_owned());
    output.push(cwd.to_owned());
    output.extend(args.iter().cloned());
    output
}

fn path_identity(path: &str) -> Option<String> {
    let identity = std::fs::canonicalize(path)
        .ok()?
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    return Some(identity.to_lowercase());
    #[cfg(not(windows))]
    Some(identity)
}

fn parse_git_worktree_list(text: &str) -> Vec<WorktreePickerEntry> {
    let mut entries = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    for line in text.split_terminator('\0').chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(path) = path.take() {
                let branch = branch
                    .take()
                    .and_then(|branch| branch.rsplit('/').next().map(str::to_owned))
                    .unwrap_or_else(|| "detached".to_owned());
                entries.push(WorktreePickerEntry {
                    label: format!("{} ({branch})", session_name_for_path(&path)),
                    path: Some(path),
                    ..WorktreePickerEntry::default()
                });
            }
        } else if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.to_owned());
        }
    }
    entries
}

/// Git state of a session's working directory, used to decide which Ditch
/// cleanup actions are safe to offer.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub in_repo: bool,
    pub is_linked_worktree: bool,
    pub branch: Option<String>,
    pub dirty: bool,
    pub unpushed: u32,
    pub has_upstream: bool,
}

#[must_use]
pub fn status(cwd: &str) -> WorktreeStatus {
    Git::new().status(cwd)
}

/// # Errors
/// Returns an error if Git cannot detach the worktree HEAD.
pub fn detach_head(worktree_path: &str) -> Result<(), String> {
    Git::new().detach_head(worktree_path)
}

#[must_use]
pub fn worktree_count(cwd: &str) -> usize {
    Git::new().worktree_count(cwd)
}

#[must_use]
pub fn trunk_branch(cwd: &str) -> Option<String> {
    Git::new().trunk_branch(cwd)
}

/// # Errors
/// Returns an error if the main worktree cannot be located or Git refuses removal.
pub fn remove_worktree(worktree_path: &str, force: bool) -> Result<(), String> {
    Git::new().remove_worktree(worktree_path, force)
}

/// # Errors
/// Returns an error if Git refuses to delete the branch.
pub fn delete_branch(repo_dir: &str, branch: &str, force: bool) -> Result<(), String> {
    Git::new().delete_branch(repo_dir, branch, force)
}

#[must_use]
pub fn worktree_root(cwd: &str) -> Option<String> {
    Git::new().worktree_root(cwd)
}

#[must_use]
pub fn head_branch(cwd: &str) -> Option<String> {
    Git::new().head_branch(cwd)
}

#[must_use]
pub fn diff_counts(cwd: &str) -> Option<(u64, u64)> {
    Git::new().diff_counts(cwd)
}

#[must_use]
pub fn suggested_session_name(cwd: &str) -> String {
    Git::new().suggested_session_name(cwd)
}

#[must_use]
pub fn discover_worktree_picker_entries(project_path: &str) -> Vec<WorktreePickerEntry> {
    Git::new().discover_worktree_picker_entries(project_path)
}

pub fn mark_occupied_worktrees(entries: &mut [WorktreePickerEntry], open_cwds: &[String]) {
    Git::new().mark_occupied_worktrees(entries, open_cwds);
}

/// # Errors
/// Returns an error if the branch or destination is invalid, or Git cannot create the checkout and branch.
pub fn add_worktree(repo_dir: &str, branch: &str) -> Result<String, String> {
    Git::new().add_worktree(repo_dir, branch)
}

#[must_use]
pub fn main_worktree(cwd: &str) -> Option<String> {
    Git::new().main_worktree(cwd)
}
