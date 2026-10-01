use std::{fs, path::Path};

use assert_fs::{TempDir, prelude::*};
use bootty_agents::{AgentKind, TerminalSessionUsage, discover_terminal_history};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::json;

const ID: &str = "01a0e479-da0a-74e2-80d8-aa627339fdbb";

fn header(provider: AgentKind, cwd: &Path) -> serde_json::Value {
    match provider {
        AgentKind::Codex => json!({"type":"session_meta","payload":{"id":ID,"cwd":cwd}}),
        AgentKind::Claude => {
            json!({"type":"user","sessionId":ID,"cwd":cwd,"message":{"role":"user","content":"First task"}})
        }
        AgentKind::Pi => json!({"type":"session","version":3,"id":ID,"cwd":cwd}),
    }
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn discovers_provider_identity_title_and_usage(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let file = root.child("project/session.jsonl");
    let prompt = match provider {
        AgentKind::Codex => {
            json!({"type":"event_msg","payload":{"type":"user_message","message":"First task"}})
        }
        AgentKind::Claude | AgentKind::Pi => {
            json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"First task"}]}})
        }
    };
    let usage = match provider {
        AgentKind::Codex => {
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":12,"output_tokens":3,"cached_input_tokens":4,"total_tokens":15}}}})
        }
        AgentKind::Claude => {
            json!({"type":"assistant","message":{"role":"assistant","usage":{"input_tokens":12,"output_tokens":3,"cache_read_input_tokens":4,"total_tokens":15}}})
        }
        AgentKind::Pi => {
            json!({"type":"message","message":{"role":"assistant","usage":{"input":12,"output":3,"cacheRead":4,"totalTokens":15}}})
        }
    };
    file.write_str(&format!(
        "{}\n{prompt}\n{usage}\n",
        header(provider, root.path())
    ))
    .unwrap();
    let records = discover_terminal_history(provider, root.path()).unwrap();
    assert_eq!(records.len(), 1);
    let record = records.first().unwrap();
    assert_eq!(record.id, ID);
    assert_eq!(record.provider, provider);
    assert_eq!(record.cwd, root.path());
    assert_eq!(record.history_path, file.path());
    assert_eq!(record.title, "First task");
    assert_eq!(
        record.usage,
        Some(TerminalSessionUsage {
            input_tokens: Some(12),
            output_tokens: Some(3),
            cached_input_tokens: Some(4),
            total_tokens: Some(15),
        })
    );
    assert!(record.updated_at > 0);
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn tail_metadata_survives_large_and_incomplete_records(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let title = match provider {
        AgentKind::Codex => {
            json!({"type":"event_msg","payload":{"type":"user_message","message":"Tail task"}})
        }
        AgentKind::Claude => json!({"type":"custom-title","customTitle":"Tail task"}),
        AgentKind::Pi => json!({"type":"session_info","name":"Tail task"}),
    };
    let file = root.child("session.jsonl");
    file.write_str(&format!(
        "{}\n{}\n{title}\n{{\"type\":",
        header(provider, root.path()),
        "x".repeat(600_000)
    ))
    .unwrap();
    let records = discover_terminal_history(provider, root.path()).unwrap();
    assert_eq!(records.first().unwrap().title, "Tail task");
    assert!(
        fs::read_to_string(file.path())
            .unwrap()
            .ends_with("{\"type\":")
    );
}

#[rstest]
fn missing_corrupt_unsupported_and_sidechain_histories_are_skipped() {
    let root = TempDir::new().unwrap();
    assert_eq!(
        discover_terminal_history(AgentKind::Pi, &root.path().join("missing")).unwrap(),
        Vec::new()
    );
    root.child("invalid.jsonl").write_str("not JSON\n").unwrap();
    root.child("future.jsonl")
        .write_str(&json!({"type":"session","version":99,"id":ID,"cwd":root.path()}).to_string())
        .unwrap();
    root.child("relative.jsonl")
        .write_str(&json!({"type":"session","version":3,"id":ID,"cwd":"relative"}).to_string())
        .unwrap();
    root.child("bad-id.jsonl")
        .write_str(
            &json!({"type":"session","version":3,"id":"--dangerous-argument","cwd":root.path()})
                .to_string(),
        )
        .unwrap();
    assert_eq!(
        discover_terminal_history(AgentKind::Pi, root.path()).unwrap(),
        Vec::new()
    );
    root.child("sidechain.jsonl")
        .write_str(
            &json!({"type":"user","isSidechain":true,"sessionId":ID,"cwd":root.path()}).to_string(),
        )
        .unwrap();
    assert_eq!(
        discover_terminal_history(AgentKind::Claude, root.path()).unwrap(),
        Vec::new()
    );
}

#[rstest]
fn duplicate_identity_keeps_one_newest_file() {
    let root = TempDir::new().unwrap();
    for name in ["first.jsonl", "second.jsonl"] {
        root.child(name)
            .write_str(&header(AgentKind::Pi, root.path()).to_string())
            .unwrap();
    }
    let records = discover_terminal_history(AgentKind::Pi, root.path()).unwrap();
    assert_eq!(records.len(), 1);
    let latest = ["first.jsonl", "second.jsonl"]
        .into_iter()
        .map(|name| {
            let path = root.path().join(name);
            let modified = fs::metadata(&path)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis();
            (modified, path)
        })
        .max()
        .unwrap();
    assert_eq!(records.first().unwrap().history_path, latest.1);
}

#[rstest]
fn discovery_limits_the_number_of_returned_files() {
    let root = TempDir::new().unwrap();
    for number in 0_u128..140 {
        let id = uuid::Uuid::from_u128(number).to_string();
        root.child(format!("{id}.jsonl"))
            .write_str(
                &json!({
                    "type":"session", "version":3, "id":id, "cwd":root.path(),
                })
                .to_string(),
            )
            .unwrap();
    }
    assert_eq!(
        discover_terminal_history(AgentKind::Pi, root.path())
            .unwrap()
            .len(),
        128
    );
}

#[cfg(unix)]
#[rstest]
fn transcript_symlinks_are_not_followed() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let history = outside.child("session.jsonl");
    history
        .write_str(&header(AgentKind::Pi, root.path()).to_string())
        .unwrap();
    std::os::unix::fs::symlink(history.path(), root.path().join("session.jsonl")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("project")).unwrap();
    assert_eq!(
        discover_terminal_history(AgentKind::Pi, root.path()).unwrap(),
        Vec::new()
    );
    assert!(discover_terminal_history(AgentKind::Pi, &root.path().join("project")).is_err());
}

proptest! {
    #[test]
    fn titles_are_bounded_unicode_without_controls(title in ".{0,300}") {
        let root = TempDir::new().unwrap();
        let record = json!({"type":"session_info","name":title});
        root.child("session.jsonl").write_str(&format!("{}\n{record}\n", header(AgentKind::Pi, root.path()))).unwrap();
        let records = discover_terminal_history(AgentKind::Pi, root.path()).unwrap();
        let title = &records.first().unwrap().title;
        prop_assert!(title.chars().count() <= 96);
        prop_assert!(!title.chars().any(char::is_control));
    }
}
