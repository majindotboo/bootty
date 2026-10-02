use std::io::Cursor;

use bootty_control::{CommandOutcome, TerminalToolOperation, serve_terminal_tools};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::{Value, json};

fn message(method: &str, params: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":"request","method":method,"params":params})
}

fn response(
    request: &Value,
    invoke: impl FnMut(TerminalToolOperation) -> CommandOutcome,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let mut output = Vec::new();
    serve_terminal_tools(Cursor::new(format!("{request}\n")), &mut output, invoke)?;
    if output.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::from_slice(&output)?))
    }
}

#[rstest]
#[case("2024-11-05", "2024-11-05")]
#[case("2025-06-18", "2025-06-18")]
#[case("2025-11-25", "2025-06-18")]
fn initialization_negotiates_supported_versions(#[case] requested: &str, #[case] expected: &str) {
    let response = response(
        &message("initialize", &json!({"protocolVersion":requested})),
        |_| panic!("initialize has no tool authority"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        response.pointer("/result/protocolVersion").unwrap(),
        expected
    );
    assert_eq!(
        response
            .pointer("/result/capabilities/tools/listChanged")
            .unwrap(),
        false
    );
}

#[rstest]
fn every_list_and_call_rechecks_the_owner_and_reports_revocation() {
    let mut enabled = true;
    let mut operations = Vec::new();
    let list = message("tools/list", &json!({}));
    let call = message(
        "tools/call",
        &json!({"name":"bootty_terminal_read","arguments":{}}),
    );
    for requested in [&list, &call, &list, &call] {
        let response = response(requested, |operation| {
            operations.push(operation);
            if enabled {
                CommandOutcome::Success {
                    value: match operation {
                        TerminalToolOperation::List => json!({"enabled":true}),
                        TerminalToolOperation::Read => json!({"text":"own screen"}),
                        TerminalToolOperation::Spawn => panic!("read-only catalog cannot spawn"),
                    },
                    warnings: Vec::new(),
                }
            } else {
                CommandOutcome::Denied {
                    message: "revoked".to_owned(),
                }
            }
        })
        .unwrap()
        .unwrap();
        if requested == &list {
            assert_eq!(
                response
                    .pointer("/result/tools")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                usize::from(enabled)
            );
        } else {
            assert_eq!(response.pointer("/result/isError").unwrap(), !enabled);
            enabled = false;
        }
    }
    assert_eq!(
        operations,
        vec![
            TerminalToolOperation::List,
            TerminalToolOperation::Read,
            TerminalToolOperation::List,
            TerminalToolOperation::Read
        ]
    );
}

#[rstest]
#[case("terminal.write", json!({}))]
#[case("bootty_terminal_read", json!({"target":"another-terminal"}))]
#[case("bootty_terminal_read", json!({"command":"session.create"}))]
#[case("bootty_terminal_read", json!(null))]
#[case("bootty_terminal_spawn", json!({"cwd":"/another-checkout"}))]
#[case("bootty_terminal_spawn", json!({"target":"another-space"}))]
#[case("bootty_terminal_spawn", json!({"argv":["sh"]}))]
fn target_overrides_and_arbitrary_commands_never_reach_the_owner(
    #[case] name: &str,
    #[case] arguments: Value,
) {
    let response = response(
        &message("tools/call", &json!({"name":name,"arguments":arguments})),
        |_| panic!("invalid input reached owner"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(response.pointer("/error/code").unwrap(), -32602);
}

#[rstest]
fn spawn_catalog_and_calls_recheck_revocation() {
    let list = message("tools/list", &json!({}));
    let call = message(
        "tools/call",
        &json!({"name":"bootty_terminal_spawn","arguments":{}}),
    );
    let mut enabled = true;
    let mut operations = Vec::new();
    for requested in [&list, &call, &list, &call] {
        let response = response(requested, |operation| {
            operations.push(operation);
            if enabled {
                CommandOutcome::Success {
                    value: json!({"enabled":true,"spawn_enabled":true,"created":true}),
                    warnings: Vec::new(),
                }
            } else {
                CommandOutcome::Denied {
                    message: "revoked".to_owned(),
                }
            }
        })
        .unwrap()
        .unwrap();
        if requested == &list {
            let tools = response
                .pointer("/result/tools")
                .unwrap()
                .as_array()
                .unwrap();
            assert_eq!(tools.len(), if enabled { 2 } else { 0 });
            if enabled {
                let spawn = tools
                    .iter()
                    .find(|tool| {
                        tool.get("name").and_then(Value::as_str) == Some("bootty_terminal_spawn")
                    })
                    .unwrap();
                assert_eq!(spawn.pointer("/annotations/readOnlyHint").unwrap(), false);
                assert_eq!(
                    spawn.pointer("/inputSchema/additionalProperties").unwrap(),
                    false
                );
            }
        } else {
            assert_eq!(response.pointer("/result/isError").unwrap(), !enabled);
            enabled = false;
        }
    }
    assert_eq!(
        operations,
        vec![
            TerminalToolOperation::List,
            TerminalToolOperation::Spawn,
            TerminalToolOperation::List,
            TerminalToolOperation::Spawn
        ]
    );
}

#[rstest]
#[case("notifications/initialized")]
#[case("notifications/cancelled")]
#[case("notifications/unknown")]
fn notifications_do_not_receive_responses(#[case] method: &str) {
    assert_eq!(
        response(&json!({"jsonrpc":"2.0","method":method}), |_| panic!(
            "notification reached owner"
        ))
        .unwrap(),
        None
    );
}

#[rstest]
fn unknown_methods_return_standard_errors() {
    let response = response(&message("resources/list", &json!({})), |_| {
        panic!("unknown method reached owner")
    })
    .unwrap()
    .unwrap();
    assert_eq!(response.pointer("/error/code").unwrap(), -32601);
    assert_eq!(response.pointer("/id").unwrap(), "request");
}

#[rstest]
fn malformed_json_does_not_corrupt_the_next_frame() {
    let input = format!(
        "{{invalid\n{}\n{}\n",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        message("ping", &json!({}))
    );
    let mut output = Vec::new();
    serve_terminal_tools(Cursor::new(input), &mut output, |_| {
        panic!("framing reached owner")
    })
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    let responses: Vec<Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(
        responses.first().unwrap().pointer("/error/code").unwrap(),
        -32700
    );
    assert_eq!(responses.get(1).unwrap().get("result").unwrap(), &json!({}));
}

proptest! {
    #[test]
    fn oversized_frames_fail_before_invoking_tools(extra in 1_usize..1024) {
        let input = vec![b'x'; (64_usize * 1024).saturating_add(extra)];
        let mut output = Vec::new();
        let result = serve_terminal_tools(Cursor::new(input), &mut output, |_| panic!("oversized request reached owner"));
        prop_assert!(result.is_err());
        prop_assert!(output.is_empty());
    }
}
