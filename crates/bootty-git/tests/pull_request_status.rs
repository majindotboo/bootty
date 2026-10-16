use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Instant,
};

use bootty_git::{
    Git, GitFactsCache,
    facts::FOCUSED_PULL_REQUEST_INTERVAL,
    github::{ChecksState, PullRequestState, PullRequestStatus},
    runner::{CommandOutput, CommandRunner},
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

/// A host whose Git is on branch `feature` and whose gh answers `pr view` with `answer`.
#[derive(Clone)]
struct Host {
    answer: Arc<Mutex<Result<Value, String>>>,
    asks: Arc<AtomicUsize>,
}

impl Host {
    fn new(answer: Result<Value, &str>) -> Self {
        Self {
            answer: Arc::new(Mutex::new(answer.map_err(str::to_owned))),
            asks: Arc::default(),
        }
    }
}

impl CommandRunner for Host {
    fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        anyhow::ensure!(program == "git", "unexpected program {program}");
        let branch = args.iter().any(|arg| arg == "symbolic-ref");
        Ok(CommandOutput {
            success: branch,
            stdout: if branch { "feature\n" } else { "" }.to_owned(),
            stderr: String::new(),
        })
    }

    fn run_in(&self, _cwd: &str, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
        anyhow::ensure!(
            program == "gh"
                && args.get(..3) == Some(&["pr".into(), "view".into(), "feature".into()]),
            "unexpected command {program} {args:?}"
        );
        let answer = self
            .answer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.asks.fetch_add(1, Ordering::SeqCst);
        Ok(match answer {
            Ok(value) => CommandOutput {
                success: true,
                stdout: value.to_string(),
                stderr: String::new(),
            },
            Err(stderr) => CommandOutput {
                success: false,
                stdout: String::new(),
                stderr,
            },
        })
    }
}

fn pull_request(state: &str, draft: bool, checks: &Value) -> Value {
    json!({"number": 42, "state": state, "isDraft": draft, "url": "https://github.com/o/r/pull/42",
        "statusCheckRollup": checks})
}

/// A pull request's state reads as GitHub reports it, and its checks as one: any failure fails,
/// anything still running is pending, and only finished ones pass.
#[rstest]
#[case::open_passing(pull_request("OPEN", false, &json!([
    {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
    {"__typename":"StatusContext","state":"SUCCESS"}])), PullRequestState::Open, ChecksState::Passing)]
#[case::draft_pending(pull_request("OPEN", true, &json!([
    {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
    {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""}])), PullRequestState::Draft, ChecksState::Pending)]
#[case::merged_failing(pull_request("MERGED", false, &json!([
    {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""},
    {"__typename":"StatusContext","state":"ERROR"}])), PullRequestState::Merged, ChecksState::Failing)]
#[case::closed_without_checks(pull_request("CLOSED", false, &json!([])), PullRequestState::Closed, ChecksState::None)]
fn branches_read_their_pull_request(
    #[case] answer: Value,
    #[case] state: PullRequestState,
    #[case] checks: ChecksState,
) {
    let git = Git::with_runner(Host::new(Ok(answer)));
    assert_eq!(
        git.branch_pull_request("/repo", "feature"),
        Ok(Some(PullRequestStatus {
            number: 42,
            state,
            checks,
            url: "https://github.com/o/r/pull/42".to_owned(),
        }))
    );
}

/// A branch without a pull request has none; gh failing for another reason says why.
#[rstest]
#[case::none("no pull requests found for branch \"feature\"", Ok(None))]
#[case::signed_out(
    "To get started with GitHub CLI, please run:  gh auth login",
    Err("To get started with GitHub CLI, please run:  gh auth login".to_owned())
)]
fn branches_without_an_answer_say_why(
    #[case] stderr: &str,
    #[case] expected: Result<Option<PullRequestStatus>, String>,
) {
    let git = Git::with_runner(Host::new(Err(stderr)));
    assert_eq!(git.branch_pull_request("/repo", "feature"), expected);
}

/// The facts cache asks about the current branch's pull request in the background, asks again
/// only after its interval, and keeps the last answer when gh then fails.
#[rstest]
fn facts_carry_the_branch_pull_request() {
    let host = Host::new(Ok(pull_request("OPEN", false, &json!([]))));
    let cache = GitFactsCache::with_remote_runner(host.clone());
    let start = Instant::now();
    let refresh_until = |now: Instant, done: &dyn Fn() -> bool| {
        for _ in 0..100_000 {
            let facts = cache.refresh("session", "/remote/repo", true, now);
            if done() {
                return facts;
            }
            thread::yield_now();
        }
        panic!("the background ask never finished");
    };
    let facts = refresh_until(start, &|| {
        cache
            .get("session", start)
            .and_then(|facts| facts.pull_request)
            .is_some()
    });
    assert_eq!(facts.branch.as_deref(), Some("feature"));
    assert_eq!(host.asks.load(Ordering::SeqCst), 1);
    *host
        .answer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Err("offline".to_owned());
    let later = start.checked_add(FOCUSED_PULL_REQUEST_INTERVAL).unwrap();
    refresh_until(later, &|| host.asks.load(Ordering::SeqCst) == 2);
    let kept = refresh_until(later, &|| true);
    assert_eq!(kept.pull_request.map(|status| status.number), Some(42));
}
