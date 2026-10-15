#![cfg(test)]
//! Bootty's worktree retention on the host that holds the checkouts.

use std::{fs, path::Path, process::Command};

use assert_fs::TempDir;
use bootty_git::{
    CleanupRequest, Git, KeptWorktree, Retention, SessionCheckout,
    cleanup::{Claim, OwnedWorktree, removal_order},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_read(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A repository with an origin, and Bootty's worktree directory beside it.
struct Host {
    root: TempDir,
}

impl Host {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let origin = root.path().join("origin.git");
        let main = root.path().join("project");
        fs::create_dir_all(&main).unwrap();
        git(
            root.path(),
            &["init", "-q", "--bare", "-b", "main", "origin.git"],
        );
        git(&main, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.email", "test@bootty.dev"),
            ("user.name", "Bootty Test"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&main, &["config", key, value]);
        }
        fs::write(main.join("README"), "hello").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "init"]);
        git(
            &main,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&main, &["push", "-q", "-u", "origin", "main"]);
        Self { root }
    }

    fn main(&self) -> std::path::PathBuf {
        self.root.path().join("project")
    }

    fn directory(&self) -> std::path::PathBuf {
        self.root.path().join("worktrees")
    }

    /// A Bootty checkout on its own branch, pushed unless `local_commit` adds one only it has.
    fn checkout(&self, name: &str, local_commit: bool) -> String {
        let path = self.directory().join("project").join(name);
        git(
            &self.main(),
            &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
        );
        git(&path, &["push", "-q", "-u", "origin", name]);
        if local_commit {
            fs::write(path.join(name), "work").unwrap();
            git(&path, &["add", "."]);
            git(&path, &["commit", "-q", "-m", "unpushed work"]);
        }
        path.to_string_lossy().into_owned()
    }

    fn request(&self, limit: u16, sessions: Vec<SessionCheckout>) -> CleanupRequest {
        CleanupRequest {
            directory: Some(self.directory().to_string_lossy().into_owned()),
            limit,
            sessions,
            // Every checkout is past its first hour.
            now: chrono_now().saturating_add(2 * 60 * 60),
        }
    }

    fn branches(&self) -> Vec<String> {
        git_read(&self.main(), &["branch", "--format=%(refname:short)"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn chrono_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn session(cwd: &str, retention: Retention, last_activity: i64, open: bool) -> SessionCheckout {
    SessionCheckout {
        cwd: cwd.to_owned(),
        retention,
        last_activity,
        open,
    }
}

/// A checkout no session uses goes, with its branch, unless it holds work no remote has.
#[rstest]
#[case::clean(false, false, None)]
#[case::uncommitted(true, false, Some("It has uncommitted changes"))]
#[case::unpushed(false, true, Some("It has 1 commit no remote has"))]
fn orphaned_checkouts_go_unless_they_hold_unsaved_work(
    #[case] dirty: bool,
    #[case] local_commit: bool,
    #[case] kept: Option<&str>,
) {
    let host = Host::new();
    let path = host.checkout("orphan", local_commit);
    if dirty {
        fs::write(Path::new(&path).join("README"), "edited").unwrap();
    }
    let report = Git::new()
        .clean_worktrees(&host.request(10, Vec::new()))
        .unwrap();
    match kept {
        None => {
            assert_eq!((report.removed, report.remaining), (vec![path.clone()], 0));
            assert!(!Path::new(&path).exists());
            assert_eq!(host.branches(), ["main"]);
        }
        Some(reason) => {
            assert_eq!(
                (report.kept, report.remaining),
                (
                    vec![KeptWorktree {
                        path: path.clone(),
                        reason: reason.to_owned()
                    }],
                    1
                )
            );
            assert!(Path::new(&path).exists());
        }
    }
}

/// A checkout made in the last hour may be about to get its session.
#[rstest]
fn a_new_checkout_waits_for_its_session() {
    let host = Host::new();
    let path = host.checkout("fresh", false);
    let mut request = host.request(10, Vec::new());
    request.now = chrono_now();
    let report = Git::new().clean_worktrees(&request).unwrap();
    assert_eq!((report.removed.len(), report.remaining), (0, 1));
    assert!(Path::new(&path).exists());
}

/// Past the limit, archived work goes before settled work, oldest first. Active and open work
/// stays even when that leaves too many.
#[rstest]
fn the_limit_removes_the_oldest_settled_and_archived_checkouts_first() {
    let host = Host::new();
    let active = host.checkout("active", false);
    let open = host.checkout("open", false);
    let settled_old = host.checkout("settled-old", false);
    let settled_new = host.checkout("settled-new", false);
    let archived = host.checkout("archived", false);
    let sessions = vec![
        session(&active, Retention::Active, 1, false),
        session(&open, Retention::Settled, 1, true),
        session(&settled_old, Retention::Settled, 10, false),
        session(&settled_new, Retention::Settled, 20, false),
        session(&archived, Retention::Archived, 30, false),
    ];
    let report = Git::new()
        .clean_worktrees(&host.request(3, sessions.clone()))
        .unwrap();
    assert_eq!(
        (report.removed, report.remaining),
        (vec![archived, settled_old], 3)
    );
    let report = Git::new()
        .clean_worktrees(&host.request(1, sessions))
        .unwrap();
    assert_eq!((report.removed, report.remaining), (vec![settled_new], 2));
}

fn claim() -> impl Strategy<Value = Claim> {
    prop_oneof![
        Just(Claim::Orphan),
        Just(Claim::Protected),
        (
            prop_oneof![Just(Retention::Archived), Just(Retention::Settled)],
            0_i64..100
        )
            .prop_map(|(retention, last_activity)| Claim::Retained {
                retention,
                last_activity
            }),
    ]
}

proptest! {
    /// The order never offers protected or new checkouts, puts every old orphan first, and ranks
    /// the rest archived before settled, then by age.
    #[test]
    fn removal_order_ranks_orphans_then_archived_then_settled_by_age(
        claims in prop::collection::vec((claim(), 0_i64..10_000), 0..12),
    ) {
        let now = 10_000;
        let worktrees = claims
            .iter()
            .enumerate()
            .map(|(index, (claim, created))| OwnedWorktree {
                path: index.to_string(),
                created: *created,
                claim: *claim,
            })
            .collect::<Vec<_>>();
        let order = removal_order(&worktrees, now);
        prop_assert!(order.iter().all(|index| worktrees[*index].claim != Claim::Protected));
        let rank = |index: usize| match worktrees[index].claim {
            Claim::Orphan => (0, worktrees[index].created),
            Claim::Retained { retention: Retention::Archived, last_activity } => (1, last_activity),
            Claim::Retained { last_activity, .. } => (2, last_activity),
            Claim::Protected => (3, 0),
        };
        prop_assert!(order.windows(2).all(|pair| rank(pair[0]) <= rank(pair[1])));
        let expected = worktrees
            .iter()
            .filter(|worktree| match worktree.claim {
                Claim::Orphan => now.saturating_sub(worktree.created) >= 60 * 60,
                Claim::Protected => false,
                Claim::Retained { .. } => true,
            })
            .count();
        prop_assert_eq!(order.len(), expected);
    }
}
