use std::time::Instant;

use bootty_control::CommandCancellation;
use bootty_git as project;
use bootty_host::{
    CancellableCommandRunner, CommandCancellation as HostCommandCancellation, CommandRunner,
};

/// The git cleanup selected for a session before its mux session is closed.
///
/// This is a mux workflow input rather than a dialog concern. The app translates the selected
/// dialog row into this value before starting the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchAction {
    DetachWorktree,
    KillOnly,
    RemoveWorktree {
        force: bool,
    },
    RemoveWorktreeAndBranch {
        force: bool,
        branch: String,
        repo: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchCleanupOutcome {
    Complete,
    NoAction(String),
    Partial { branch: String, error: String },
}

/// Run git cleanup before the backend session is killed.
///
/// The main worktree is resolved up front because `cwd` stops resolving inside the repository
/// once a linked worktree is removed. A failure before destructive work keeps the session alive;
/// a branch deletion failure after worktree removal is reported as partial progress.
#[must_use]
pub fn run_ditch_cleanup(cwd: Option<&str>, action: &DitchAction) -> DitchCleanupOutcome {
    let git = project::Git::new();
    run_ditch_cleanup_with_git(&git, cwd, action)
}

/// Run Git cleanup with the command lifetime captured by the caller.
///
/// The runner checks cancellation before starting each Git process and while it is running. A
/// deadline that expires during a destructive operation is reported as a failure; this leaves the
/// backend session open for the caller to reconcile instead of claiming cleanup succeeded.
#[must_use]
pub fn run_ditch_cleanup_with_deadline(
    cwd: Option<&str>,
    action: &DitchAction,
    deadline: Instant,
    cancellation: CommandCancellation,
) -> DitchCleanupOutcome {
    let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
        HostCommandCancellation::default(),
        deadline,
        move || cancellation.is_cancelled(),
    );
    let git = project::Git::with_runner(runner);
    run_ditch_cleanup_with_git(&git, cwd, action)
}

fn run_ditch_cleanup_with_git<R: CommandRunner>(
    git: &project::Git<R>,
    cwd: Option<&str>,
    action: &DitchAction,
) -> DitchCleanupOutcome {
    let Some(cwd) = cwd else {
        return DitchCleanupOutcome::Complete;
    };
    match action {
        DitchAction::KillOnly => DitchCleanupOutcome::Complete,
        DitchAction::DetachWorktree => git
            .detach_head(cwd)
            .map_or_else(DitchCleanupOutcome::NoAction, |()| {
                DitchCleanupOutcome::Complete
            }),
        DitchAction::RemoveWorktree { force } => git
            .remove_worktree(cwd, *force)
            .map_or_else(DitchCleanupOutcome::NoAction, |()| {
                DitchCleanupOutcome::Complete
            }),
        DitchAction::RemoveWorktreeAndBranch {
            force,
            branch,
            repo,
        } => {
            // A retry can arrive after the worktree was removed but branch deletion failed. Skip
            // the missing worktree and finish the branch operation from its stable repo path.
            let removed = if std::path::Path::new(cwd).exists() {
                if let Err(error) = git.remove_worktree(cwd, *force) {
                    return DitchCleanupOutcome::NoAction(error);
                }
                true
            } else {
                false
            };
            if let Err(error) = git.delete_branch(repo, branch, *force) {
                return if removed {
                    DitchCleanupOutcome::Partial {
                        branch: branch.clone(),
                        error,
                    }
                } else {
                    DitchCleanupOutcome::NoAction(error)
                };
            }
            DitchCleanupOutcome::Complete
        }
    }
}
