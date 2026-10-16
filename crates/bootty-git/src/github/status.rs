//! A branch's pull request at a glance: its number, where it stands, and how its checks went.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Git, runner::CommandRunner};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PullRequestStatus {
    pub number: u32,
    pub state: PullRequestState,
    pub checks: ChecksState,
    pub url: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChecksState {
    /// The pull request has no checks.
    None,
    Pending,
    Passing,
    Failing,
}

impl PullRequestStatus {
    /// Reads `gh pr view --json number,state,isDraft,url,statusCheckRollup`.
    /// # Errors
    /// Returns output gh didn't shape that way.
    pub fn from_gh(value: &Value) -> Result<Self, String> {
        let number = value
            .get("number")
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok())
            .ok_or("GitHub returned no pull request number")?;
        let state = match value.get("state").and_then(Value::as_str) {
            Some("MERGED") => PullRequestState::Merged,
            Some("CLOSED") => PullRequestState::Closed,
            Some("OPEN") if value.get("isDraft") == Some(&Value::Bool(true)) => {
                PullRequestState::Draft
            }
            Some("OPEN") => PullRequestState::Open,
            _ => return Err("GitHub returned an unknown pull request state".to_owned()),
        };
        let checks = value
            .get("statusCheckRollup")
            .and_then(Value::as_array)
            .map_or(ChecksState::None, |checks| summarize_checks(checks));
        Ok(Self {
            number,
            state,
            checks,
            url: value
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }
}

/// One failure fails them all; otherwise one still running keeps them pending.
fn summarize_checks(checks: &[Value]) -> ChecksState {
    let mut summary = ChecksState::None;
    for check in checks {
        let field = |name| check.get(name).and_then(Value::as_str);
        // Check runs report a status and, once completed, a conclusion; status contexts a state.
        let outcome = field("conclusion")
            .filter(|_| field("status").is_none_or(|status| status == "COMPLETED"))
            .or_else(|| field("state"))
            .unwrap_or("PENDING");
        match outcome {
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED"
            | "STARTUP_FAILURE" => return ChecksState::Failing,
            "SUCCESS" | "NEUTRAL" | "SKIPPED" | "STALE" => {
                if summary == ChecksState::None {
                    summary = ChecksState::Passing;
                }
            }
            _ => summary = ChecksState::Pending,
        }
    }
    summary
}

impl<R: CommandRunner> Git<R> {
    /// The pull request whose head is `branch`, as the host's gh account sees it from `cwd`;
    /// `None` when the branch has none.
    /// # Errors
    /// Returns gh missing, signed out or offline, a remote that isn't on GitHub, or output gh
    /// didn't shape as expected.
    pub fn branch_pull_request(
        &self,
        cwd: &str,
        branch: &str,
    ) -> Result<Option<PullRequestStatus>, String> {
        if branch.is_empty() || branch.starts_with('-') {
            return Ok(None);
        }
        let output = self
            .runner
            .run_in(
                cwd,
                "gh",
                &[
                    "pr".into(),
                    "view".into(),
                    branch.into(),
                    "--json".into(),
                    "number,state,isDraft,url,statusCheckRollup".into(),
                ],
            )
            .map_err(|error| error.to_string())?;
        if !output.success {
            return if output.stderr.contains("no pull requests found") {
                Ok(None)
            } else {
                Err(output.stderr.trim().to_owned())
            };
        }
        let value: Value =
            serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
        PullRequestStatus::from_gh(&value).map(Some)
    }
}
