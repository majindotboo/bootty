use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use bootty_git::facts::{BACKGROUND_FACT_INTERVAL, COLD_REFRESH_SPACING, FOCUSED_FACT_INTERVAL};
use bootty_git::{
    CommandOutput, CommandRunner, Git, GitFactsCache, GitSessionFactsInput, WorktreeRevisionCache,
};
use pretty_assertions::assert_eq;

type RecordedCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

#[derive(Clone, Default)]
struct RecordingRunner {
    calls: RecordedCalls,
}

impl RecordingRunner {
    fn branch_calls(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "symbolic-ref"))
            .count()
    }

    fn diff_calls(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, args)| args.iter().any(|arg| arg == "diff"))
            .count()
    }

    fn wait_for_diff_calls(&self, expected: usize) {
        for _ in 0..10_000 {
            if self.diff_calls() >= expected {
                return;
            }
            thread::yield_now();
        }
        assert_eq!(self.diff_calls(), expected);
    }
}

#[rstest::rstest]
#[case(true, FOCUSED_FACT_INTERVAL)]
#[case(false, BACKGROUND_FACT_INTERVAL)]
fn unchanged_branch_checks_follow_focus_cadence(
    #[case] selected: bool,
    #[case] interval: Duration,
) {
    let runner = RecordingRunner::default();
    let cache = GitFactsCache::with_remote_runner(runner.clone());
    let start = Instant::now();
    cache.refresh("session", "/remote/repo", selected, start);
    // Reading the published branch also establishes that the worker has cleared live_running.
    for _ in 0..10_000 {
        if cache.get("session", start).unwrap().branch.is_some() {
            break;
        }
        thread::yield_now();
    }
    assert!(cache.get("session", start).unwrap().branch.is_some());
    assert_eq!(runner.branch_calls(), 1);

    for millis in (250..u64::try_from(interval.as_millis()).unwrap()).step_by(250) {
        cache.refresh(
            "session",
            "/remote/repo",
            selected,
            start
                .checked_add(Duration::from_millis(millis))
                .expect("test timestamp"),
        );
    }
    assert_eq!(runner.branch_calls(), 1);

    cache.refresh(
        "session",
        "/remote/repo",
        selected,
        start.checked_add(interval).expect("test interval"),
    );
    for _ in 0..10_000 {
        if runner.branch_calls() == 2 {
            break;
        }
        thread::yield_now();
    }
    assert_eq!(runner.branch_calls(), 2);
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((program.to_owned(), args.to_vec()));
        Ok(CommandOutput {
            success: true,
            stdout: if args.iter().any(|arg| arg == "diff") {
                "1\t0\tfile.rs\n"
            } else {
                "/remote/repo\n"
            }
            .to_owned(),
            stderr: String::new(),
        })
    }
}

#[derive(Clone)]
struct ProjectRootRunner;

impl CommandRunner for ProjectRootRunner {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = match (program, args.get(2..).unwrap_or_default()) {
            ("git", ["symbolic-ref", "--quiet", "--short", "HEAD"]) => "feature/ui",
            ("git", ["rev-parse", "--show-toplevel"]) => "/remote/feature-worktree",
            ("git", ["worktree", "list", "--porcelain", "-z"]) => {
                "worktree /remote/project\0HEAD 111\0branch refs/heads/main\0\0worktree /remote/feature-worktree\0HEAD 222\0branch refs/heads/feature/ui\0\0"
            }
            _ => "",
        };
        Ok(CommandOutput {
            success: !output.is_empty(),
            stdout: output.to_owned(),
            stderr: String::new(),
        })
    }
}

#[rstest::rstest]
#[case("/remote/feature-worktree")]
#[case("/remote/feature-worktree/crates/ui")]
fn session_facts_publish_the_owning_hosts_project_root_for_linked_worktrees(#[case] cwd: &str) {
    let cache = GitFactsCache::with_remote_runner(ProjectRootRunner);
    let now = Instant::now();
    let input = GitSessionFactsInput {
        scope_key: "remote-host".to_owned(),
        session_id: "$1".to_owned(),
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        process: None,
        selected: true,
    };
    cache.refresh_session(&input, now);
    for _ in 0..10_000 {
        let facts = cache.refresh_session(&input, now);
        if facts.branch.is_some() {
            assert_eq!(facts.project_root.as_deref(), Some("/remote/project"));
            assert_eq!(facts.branch.as_deref(), Some("feature/ui"));
            return;
        }
        thread::yield_now();
    }
    panic!("Git worker did not publish its project facts");
}

#[test]
fn injected_runner_keeps_git_paths_on_the_target_host() {
    let runner = RecordingRunner::default();
    let git = Git::with_runner(runner.clone());

    assert_eq!(
        git.worktree_root("/remote/repo"),
        Some("/remote/repo".to_owned())
    );
    let calls = runner
        .calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(calls[0].0, "git");
    assert_eq!(calls[0].1.first().map(String::as_str), Some("-C"));
    assert_eq!(calls[0].1.get(1).map(String::as_str), Some("/remote/repo"));
}

#[test]
fn remote_fact_cache_disables_local_revision_watching() {
    let runner = RecordingRunner::default();
    let cache = GitFactsCache::with_remote_runner(runner);
    assert_eq!(cache.worktree_revision("/remote/repo"), 0);
    assert_eq!(
        WorktreeRevisionCache::disabled().revision("/remote/repo"),
        0
    );
}

#[test]
fn cold_refreshes_are_spaced_across_sessions() {
    let runner = RecordingRunner::default();
    let cache = GitFactsCache::with_remote_runner(runner.clone());
    let now = Instant::now();

    cache.refresh("first", "/remote/first", false, now);
    runner.wait_for_diff_calls(1);

    // Both rows are presented in the same frame. The first cold pass owns that frame's deep
    // budget, even after its worker has already completed.
    cache.refresh("second", "/remote/second", false, now);
    for _ in 0..10_000 {
        thread::yield_now();
    }
    assert_eq!(runner.diff_calls(), 1);
}

#[test]
fn completed_deep_refreshes_are_spaced_across_sessions() {
    let runner = RecordingRunner::default();
    let cache = GitFactsCache::with_remote_runner(runner.clone());
    let start = Instant::now();

    cache.refresh("first", "/remote/first", true, start);
    runner.wait_for_diff_calls(1);
    wait_for_published_diff(&cache, "first", start);
    let second_cold = start
        .checked_add(COLD_REFRESH_SPACING)
        .and_then(|time| time.checked_add(Duration::from_nanos(1)))
        .expect("cold refresh timestamp");
    cache.refresh("second", "/remote/second", true, second_cold);
    runner.wait_for_diff_calls(2);
    wait_for_published_diff(&cache, "second", second_cold);

    let next = second_cold + FOCUSED_FACT_INTERVAL + Duration::from_nanos(1);
    cache.refresh("first", "/remote/first", true, next);
    runner.wait_for_diff_calls(3);
    // The second row is due at the same instant, but the shared deep spacing leaves it cached.
    cache.refresh("second", "/remote/second", true, next);
    for _ in 0..10_000 {
        thread::yield_now();
    }
    assert_eq!(runner.diff_calls(), 3);
}

fn wait_for_published_diff(cache: &GitFactsCache<RecordingRunner>, key: &str, now: Instant) {
    // A runner call precedes publication. Only published counts establish that the
    // worker has cleared its running flag before the next simulated frame.
    for _ in 0..10_000 {
        if cache
            .get(key, now)
            .and_then(|facts| facts.diff_added)
            .is_some()
        {
            return;
        }
        thread::yield_now();
    }
    assert_eq!(
        cache.get(key, now).and_then(|facts| facts.diff_added),
        Some(1)
    );
}

#[test]
fn empty_session_does_not_start_git_or_reuse_old_facts() {
    let runner = RecordingRunner::default();
    let cache = GitFactsCache::with_remote_runner(runner.clone());
    let facts = cache.refresh_session(
        &GitSessionFactsInput {
            scope_key: "local".to_owned(),
            session_id: "$1".to_owned(),
            cwd: None,
            pane_pid: None,
            process: Some(String::new()),
            selected: false,
        },
        Instant::now(),
    );

    assert!(facts.branch.is_none());
    assert_eq!(facts.branch_status, bootty_git::BranchStatus::Unknown);
    assert!(facts.display_process.is_none());
    assert_eq!(runner.diff_calls(), 0);
    assert!(
        runner
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[rstest::rstest]
#[case("native", None)]
#[case("$1", Some("shell"))]
fn display_process_preserves_tmux_only_contract(
    #[case] session_id: &str,
    #[case] expected: Option<&str>,
) {
    let cache = GitFactsCache::with_remote_runner(RecordingRunner::default());
    let facts = cache.refresh_session(
        &GitSessionFactsInput {
            scope_key: "local".to_owned(),
            session_id: session_id.to_owned(),
            cwd: None,
            pane_pid: None,
            process: Some("shell".to_owned()),
            selected: false,
        },
        Instant::now(),
    );

    assert_eq!(facts.display_process.as_deref(), expected);
}

struct ControlledRead {
    diff: bool,
    response: std::sync::mpsc::Sender<String>,
    finished: std::sync::mpsc::Receiver<()>,
}

impl ControlledRead {
    fn complete(self, branch: &str, counts: &str) -> anyhow::Result<()> {
        self.response
            .send(if self.diff { counts } else { branch }.to_owned())?;
        self.finished.recv()?;
        Ok(())
    }
}

struct ControlledRunner {
    reads: std::sync::mpsc::Sender<ControlledRead>,
    finished: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl Clone for ControlledRunner {
    fn clone(&self) -> Self {
        Self {
            reads: self.reads.clone(),
            finished: Mutex::new(None),
        }
    }
}

impl Drop for ControlledRunner {
    fn drop(&mut self) {
        // Each worker owns its runner clone until after publishing its result.
        if let Some(finished) = self
            .finished
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = finished.send(());
        }
    }
}

impl CommandRunner for ControlledRunner {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        anyhow::ensure!(program == "git", "unexpected program: {program}");
        let diff = args.iter().any(|arg| arg == "diff");
        anyhow::ensure!(
            diff || args.iter().any(|arg| arg == "symbolic-ref"),
            "unexpected arguments: {args:?}"
        );
        let (response, result) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        *self
            .finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(finished);
        self.reads
            .send(ControlledRead {
                diff,
                response,
                finished: completion,
            })
            .map_err(|_| anyhow::anyhow!("test reader disconnected"))?;
        Ok(CommandOutput {
            success: true,
            stdout: result.recv()?,
            stderr: String::new(),
        })
    }
}

#[rstest::rstest]
fn replaced_git_facts_reject_old_workers_even_when_the_path_returns(
    #[values(false, true)] pruned: bool,
) {
    let (reads, requests) = std::sync::mpsc::channel();
    let cache = GitFactsCache::with_remote_runner(ControlledRunner {
        reads,
        finished: Mutex::new(None),
    });
    let start = Instant::now();
    cache.refresh("session", "/remote/repo", true, start);
    let old = [requests.recv().unwrap(), requests.recv().unwrap()];

    let now = start
        .checked_add(bootty_git::facts::FACT_CACHE_TTL)
        .unwrap()
        .checked_add(Duration::from_secs(1))
        .unwrap();
    if pruned {
        cache.prune(now);
    } else {
        // The session leaves and returns to the same cwd while the first reads are blocked.
        cache.refresh("session", "", true, start);
    }
    cache.refresh("session", "/remote/repo", true, now);
    for request in [requests.recv().unwrap(), requests.recv().unwrap()] {
        request.complete("current\n", "7\t3\tfile.rs\n").unwrap();
    }
    let expected = cache.get("session", now).unwrap();
    assert_eq!(expected.branch.as_deref(), Some("current"));
    assert_eq!(
        (expected.diff_added, expected.diff_removed),
        (Some(7), Some(3))
    );
    for request in old {
        request.complete("obsolete\n", "99\t88\tfile.rs\n").unwrap();
    }
    assert_eq!(cache.get("session", now), Some(expected));
}
