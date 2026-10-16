#![cfg(unix)]
use std::{fs, path::Path};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, GeneratedPullRequest, NativeSessionConfig, generate_commit_message,
    generate_pull_request,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;

/// A Codex stand-in that checks the query is the quick, tool-free one about the changes, and
/// answers with `answer.json`.
fn codex(root: &Path, answer: &serde_json::Value) -> std::io::Result<NativeSessionConfig> {
    let script = root.join("codex.py");
    bootty_test_support::write_executable(
        &script,
        r"#!/usr/bin/env python3
import os,pathlib,sys
args=sys.argv[1:]
assert 'exec' in args and '--ephemeral' in args
assert args[args.index('--sandbox')+1]=='read-only'
assert args[args.index('--model')+1]=='quick-model'
assert 'features.shell_tool=false' in args
assert '+second line' in args[-1]
account=pathlib.Path(os.environ['CODEX_HOME'])
pathlib.Path(args[args.index('--output-last-message')+1]).write_text((account/'answer.json').read_text())
",
    )?;
    fs::write(root.join("answer.json"), answer.to_string())?;
    let mut config = NativeSessionConfig::new(AgentKind::Codex, root);
    config.program = script.to_string_lossy().into_owned();
    config.account_directory = Some(root.to_string_lossy().into_owned());
    Ok(config)
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
    let config = codex(root.path(), &answer).unwrap();
    let written = generate_commit_message(&config, "quick-model", CHANGES, || false)
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
    let config = codex(root.path(), &answer).unwrap();
    let written = generate_pull_request(&config, "quick-model", CHANGES, || false);
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
    let mut config = codex(root.path(), &json!({})).unwrap();
    config.provider = AgentKind::Pi;
    assert_eq!(
        generate_commit_message(&config, "quick-model", CHANGES, || false),
        Err("Writing a commit message requires a Codex quick model".to_owned())
    );
}
