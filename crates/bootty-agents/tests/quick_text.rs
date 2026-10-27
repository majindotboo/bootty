#![cfg(unix)]
#![cfg(test)]

#[path = "support/quick_query.rs"]
pub mod quick_query;

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, GeneratedPullRequest, QuickQuery, generate_commit_message_using,
    generate_pull_request_using,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;

fn query() -> QuickQuery<'static> {
    QuickQuery {
        cancelled: &|| false,
        output: &|command, _| Ok(quick_query::output(&command, "+second line")),
    }
}

const CHANGES: &str = "Staged changes:\n+second line\n";

/// Codex's subject and body become one message, the body after a blank line; a subject that
/// isn't one line is refused.
#[rstest]
#[case::with_body(json!({"subject":"feat: add a second line","body":"Notes need two lines.\n"}), Ok("feat: add a second line\n\nNotes need two lines."))]
#[case::subject_only(json!({"subject":"fix: typo","body":""}), Ok("fix: typo"))]
#[case::two_line_subject(json!({"subject":"fix:\ntypo","body":""}), Err("Codex wrote an unusable commit message"))]
#[case::not_json(json!("just words"), Err("Codex returned an invalid commit message"))]
fn commit_messages_come_from_the_staged_changes(
    #[case] answer: serde_json::Value,
    #[case] expected: Result<&str, &str>,
) {
    let root = TempDir::new().unwrap();
    let config = quick_query::config(root.path(), &answer);
    let written = generate_commit_message_using(&config, "quick-model", CHANGES, &query())
        .map(|message| message.message());
    assert_eq!(written, expected.map(str::to_owned).map_err(str::to_owned));
}

/// A pull request's title and description come from the branch; an empty title is refused.
#[rstest]
#[case::written(json!({"title":"Add a second line","body":"Notes had one line.\n\nThis adds another."}), true)]
#[case::empty_title(json!({"title":"","body":"Something"}), false)]
fn pull_request_text_comes_from_the_branch(
    #[case] answer: serde_json::Value,
    #[case] accepted: bool,
) {
    let root = TempDir::new().unwrap();
    let config = quick_query::config(root.path(), &answer);
    let written = generate_pull_request_using(&config, "quick-model", CHANGES, &query());
    if accepted {
        assert_eq!(
            written,
            Ok(serde_json::from_value::<GeneratedPullRequest>(answer).unwrap())
        );
    } else {
        assert_eq!(
            written,
            Err("Codex wrote unusable pull request text".to_owned())
        );
    }
}

/// Only Codex writes these; another provider is refused before anything runs.
#[rstest]
fn writing_needs_codex() {
    let root = TempDir::new().unwrap();
    let mut config = quick_query::config(root.path(), &json!({}));
    config.provider = AgentKind::Pi;
    assert_eq!(
        generate_commit_message_using(&config, "quick-model", CHANGES, &query()),
        Err("Writing a commit message requires a Codex quick model".to_owned())
    );
}
