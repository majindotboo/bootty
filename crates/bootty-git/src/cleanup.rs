//! Bootty keeps a bounded number of the worktrees it created.
//!
//! A checkout is Bootty's when it is a linked worktree at `<directory>/<repository>/<name>`.
//! This runs on the host that holds the checkouts: in the app for local ones, in the daemon for
//! a remote host.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{runner::CommandRunner, worktree::Git};

/// Deliberate limit: a checkout without a session is left alone for its first hour, so a session
/// being created in it can't lose it. Raise it if session creation can take longer.
const ORPHAN_GRACE_SECONDS: i64 = 60 * 60;

/// What a saved session's state says about keeping its checkout.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    /// Archived work is the first to go when there are too many checkouts.
    Archived,
    Settled,
    /// Active work is never removed.
    Active,
}

/// One saved session, as the cleanup on its host sees it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionCheckout {
    pub cwd: String,
    pub retention: Retention,
    /// Unix seconds.
    pub last_activity: i64,
    /// A terminal is open in the session; its checkout stays.
    pub open: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CleanupRequest {
    /// Parent of Bootty's checkouts; a leading `~` is this host's home. `None` uses the host
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// At most this many checkouts stay. 0 keeps every checkout a session still has.
    pub limit: u16,
    /// Every saved session on this host, from every Space.
    pub sessions: Vec<SessionCheckout>,
    /// Unix seconds.
    pub now: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed: Vec<String>,
    /// Checkouts that were due for removal but stayed, and why.
    pub kept: Vec<KeptWorktree>,
    /// Bootty's checkouts still on this host.
    pub remaining: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct KeptWorktree {
    pub path: String,
    pub reason: String,
}

/// Who still needs a checkout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// No saved session uses it: its session was deleted, or it was never used.
    Orphan,
    /// An active or open session uses it.
    Protected,
    /// Only settled or archived sessions use it.
    Retained {
        retention: Retention,
        last_activity: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedWorktree {
    pub path: String,
    /// Unix seconds.
    pub created: i64,
    pub claim: Claim,
}

/// The checkouts to try removing, in order.
///
/// Every orphan older than its grace period comes first, oldest first; then archived and settled
/// work, oldest activity first. The caller removes orphans unconditionally and the rest only
/// while more than the limit remain. Protected work never appears.
#[must_use]
pub fn removal_order(worktrees: &[OwnedWorktree], now: i64) -> Vec<usize> {
    let mut orphans = Vec::new();
    let mut retained = Vec::new();
    for (index, worktree) in worktrees.iter().enumerate() {
        match worktree.claim {
            Claim::Orphan if now.saturating_sub(worktree.created) >= ORPHAN_GRACE_SECONDS => {
                orphans.push((worktree.created, index));
            }
            Claim::Retained {
                retention,
                last_activity,
            } if retention != Retention::Active => {
                retained.push(((retention, last_activity), index));
            }
            Claim::Orphan | Claim::Retained { .. } | Claim::Protected => {}
        }
    }
    orphans.sort_unstable();
    retained.sort_unstable();
    orphans
        .into_iter()
        .map(|(_, index)| index)
        .chain(retained.into_iter().map(|(_, index)| index))
        .collect()
}

/// The strongest claim the sessions in a checkout make on it.
fn claim(sessions: &[&SessionCheckout]) -> Claim {
    if sessions.is_empty() {
        return Claim::Orphan;
    }
    if sessions
        .iter()
        .any(|session| session.open || session.retention == Retention::Active)
    {
        return Claim::Protected;
    }
    Claim::Retained {
        retention: sessions
            .iter()
            .map(|session| session.retention)
            .max()
            .unwrap_or(Retention::Archived),
        last_activity: sessions
            .iter()
            .map(|session| session.last_activity)
            .max()
            .unwrap_or_default(),
    }
}

impl<R: CommandRunner> Git<R> {
    /// Remove orphaned checkouts and, past the limit, the oldest settled and archived ones. A
    /// checkout with uncommitted changes or commits no remote has always stays, and the report
    /// says why. A removed checkout's branch goes too when Git's safe delete accepts it.
    /// # Errors
    /// Returns an error when the directory can't be resolved or read.
    pub fn clean_worktrees(&self, request: &CleanupRequest) -> Result<CleanupReport, String> {
        let directory = crate::worktree::worktree_directory(request.directory.as_deref())?;
        let worktrees = owned_worktrees(&directory, &request.sessions)?;
        let mut report = CleanupReport {
            remaining: worktrees.len(),
            ..CleanupReport::default()
        };
        let limit = usize::from(request.limit);
        for index in removal_order(&worktrees, request.now) {
            let Some(worktree) = worktrees.get(index) else {
                continue;
            };
            if worktree.claim != Claim::Orphan && (limit == 0 || report.remaining <= limit) {
                break;
            }
            match self.remove_owned_worktree(&worktree.path) {
                Ok(()) => {
                    report.remaining = report.remaining.saturating_sub(1);
                    report.removed.push(worktree.path.clone());
                }
                Err(reason) => report.kept.push(KeptWorktree {
                    path: worktree.path.clone(),
                    reason,
                }),
            }
        }
        Ok(report)
    }

    /// Remove one of Bootty's checkouts if nothing in it would be lost.
    fn remove_owned_worktree(&self, path: &str) -> Result<(), String> {
        let status = self.status(path);
        if !status.in_repo || !status.is_linked_worktree {
            return Err("It isn't a linked worktree".to_owned());
        }
        if status.dirty {
            return Err("It has uncommitted changes".to_owned());
        }
        match self.commits_not_on_remotes(path) {
            Some(0) => {}
            Some(1) => return Err("It has 1 commit no remote has".to_owned()),
            Some(count) => return Err(format!("It has {count} commits no remote has")),
            None => return Err("Git couldn't tell whether its commits are pushed".to_owned()),
        }
        let main = self
            .main_worktree(path)
            .ok_or("Git couldn't find its main checkout")?;
        self.remove_worktree(path, false)
            .map_err(|error| format!("Git kept it: {error}"))?;
        // Git's safe delete keeps a branch it can't prove merged or pushed.
        if let Some(branch) = status.branch {
            _ = self.delete_branch(&main, &branch, false);
        }
        Ok(())
    }
}

/// Bootty's checkouts under `directory`, each with the claim its sessions make on it.
fn owned_worktrees(
    directory: &Path,
    sessions: &[SessionCheckout],
) -> Result<Vec<OwnedWorktree>, String> {
    let repositories = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Couldn't read {}: {error}", directory.display())),
    };
    let session_paths = sessions
        .iter()
        .filter_map(|session| Some((std::fs::canonicalize(&session.cwd).ok()?, session)))
        .collect::<Vec<_>>();
    let mut worktrees = Vec::new();
    for repository in repositories.flatten() {
        let Ok(checkouts) = std::fs::read_dir(repository.path()) else {
            continue;
        };
        for checkout in checkouts.flatten() {
            let path = checkout.path();
            // A linked worktree's `.git` is a file pointing at the main repository.
            let Ok(marker) = std::fs::symlink_metadata(path.join(".git")) else {
                continue;
            };
            if !marker.is_file() || checkout.file_type().is_ok_and(|kind| kind.is_symlink()) {
                continue;
            }
            let Some(text) = path.to_str().map(str::to_owned) else {
                continue;
            };
            let root = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let users = session_paths
                .iter()
                .filter(|(cwd, _)| cwd.starts_with(&root))
                .map(|(_, session)| *session)
                .collect::<Vec<_>>();
            worktrees.push(OwnedWorktree {
                path: text,
                created: modified_seconds(&marker),
                claim: claim(&users),
            });
        }
    }
    Ok(worktrees)
}

fn modified_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}
