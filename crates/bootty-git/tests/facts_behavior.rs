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
            stdout: "/remote/repo\n".to_owned(),
            stderr: String::new(),
        })
    }
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
    let second_cold = start
        .checked_add(COLD_REFRESH_SPACING)
        .and_then(|time| time.checked_add(Duration::from_nanos(1)))
        .expect("cold refresh timestamp");
    cache.refresh("second", "/remote/second", true, second_cold);
    runner.wait_for_diff_calls(2);

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
