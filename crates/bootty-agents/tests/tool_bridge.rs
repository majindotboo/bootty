use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bootty_agents::{
    AgentCommandExecutor, AgentKind, MAX_TOOL_MESSAGE_BYTES, ToolCapture, ToolCapturedCommand,
    ToolLease, ToolPolicy, ToolProtocol, ToolScope,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};
use serde_json::{Value, json};

#[fixture]
fn binding() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Binding,
        handle: "captured binding".into(),
        generation: 19,
    }
}

fn terminal() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "captured terminal".into(),
        generation: 23,
    }
}

fn lease(
    binding: CommandTarget,
    policy: ToolPolicy,
    captures: Vec<ToolCapturedCommand>,
) -> Result<ToolLease, String> {
    ToolLease::issue(
        ToolScope {
            provider: AgentKind::Codex,
            binding,
        },
        Caller::Socket,
        policy,
        captures,
    )
}

fn success() -> CommandOutcome {
    CommandOutcome::Success {
        value: json!({"text":"retained screen"}),
        warnings: Vec::new(),
    }
}

fn recording(calls: Arc<Mutex<Vec<CommandInvocation>>>) -> impl AgentCommandExecutor {
    move |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        let value = match invocation.command.as_str() {
            "spaces.inspect" => json!({"name":"Captured","backend":"native","host":"Local"}),
            "terminal.activities" => {
                json!({"terminals":[{"name":"shell","session":"Captured","target":terminal()}],"truncated":false})
            }
            "agents.native.status" => {
                json!({"id":"native:codex:7","title":"Inspect changes","status":"working"})
            }
            "agents.native.activities" => {
                json!([{"id":"native:codex:7","title":"Inspect changes","status":"working"}])
            }
            "agents.native.models" => {
                json!([{"id":"model-a","display_name":"Model A","reasoning_efforts":["low","high"]}])
            }
            "agents.native.provider" => {
                json!({"provider":"codex","model":"model-a","reasoning_effort":"high","fast_mode":true,"permissions":"supervised","permission_modes":["supervised","full-access"]})
            }
            "agents.native.profiles" => {
                json!({"provider":"codex","captured_profile":"captured","profiles":[{"id":"captured","name":"Captured"}]})
            }
            "agents.native.activity" => {
                json!({"id":"native:codex:7","provider":"codex","status":"working","total":1,"items":[{"id":"answer","role":"assistant","text":"Recent response","text_truncated":false,"complete":true,"tool":null}]})
            }
            _ => json!({"text":"retained screen"}),
        };
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(invocation);
        CommandOutcome::Success {
            value,
            warnings: Vec::new(),
        }
    }
}

fn request(
    protocol: &ToolProtocol,
    params: &Value,
    commands: &dyn AgentCommandExecutor,
) -> Result<Value, String> {
    let bytes = serde_json::to_vec(
        &json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":params}),
    )
    .map_err(|error| error.to_string())?;
    protocol
        .handle(&bytes, Instant::now(), commands)
        .ok_or_else(|| "Expected tool response".to_owned())
}

fn catalog(protocol: &ToolProtocol) -> Result<Value, String> {
    protocol
        .handle(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            Instant::now(),
            &recording(Arc::default()),
        )
        .and_then(|response| {
            response
                .get("result")
                .and_then(|result| result.get("tools"))
                .cloned()
        })
        .ok_or_else(|| "Expected catalog response".to_owned())
}

#[rstest]
#[case::info("get_workspace_info", "spaces.inspect")]
#[case::terminals("list_terminals", "terminal.activities")]
fn workspace_reads_keep_the_captured_binding_and_revoke_with_the_lease(
    binding: CommandTarget,
    #[case] tool: &str,
    #[case] command: &str,
    #[values(false, true)] native: bool,
) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    let protocol = ToolProtocol::new(grant.clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let commands = recording(calls.clone());
    let params = json!({"name":tool});
    assert_eq!(
        request(&protocol, &params, &commands).unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
    grant.bind(&binding, terminal()).unwrap();
    if native {
        grant
            .bind_native_session(CommandTarget {
                kind: ResourceKind::Session,
                handle: "native:codex:7".into(),
                generation: 7,
            })
            .unwrap();
    }
    let response = request(&protocol, &params, &commands).unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"],
        if tool == "get_workspace_info" {
            json!({"name":"Captured","backend":"native","host":"Local"})
        } else {
            json!({"terminals":[{"name":"shell","session":"Captured","target":terminal()}],"truncated":false})
        }
    );
    let invocations = calls.lock().unwrap().clone();
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].command, command);
    assert_eq!(invocations[0].target, Some(binding));
    assert_eq!(invocations[0].arguments, Vec::<String>::new());
    assert_eq!(
        request(
            &protocol,
            &json!({"name":tool,"arguments":{"space":"foreign"}}),
            &commands
        )
        .unwrap()["result"]["isError"],
        true
    );
    grant.revoke();
    assert_eq!(
        request(&protocol, &params, &commands).unwrap()["result"]["isError"],
        true
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[rstest]
fn launch_scope_is_bound_once_and_does_not_follow_current_focus(binding: CommandTarget) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    let protocol = ToolProtocol::new(grant.clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let commands = recording(calls.clone());
    let params = json!({"name":"terminal_read"});
    assert_eq!(
        request(&protocol, &params, &commands).unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
    let mut foreign = binding.clone();
    foreign.generation = foreign.generation.checked_add(1).unwrap();
    assert!(grant.bind(&foreign, terminal()).is_err());
    grant.bind(&binding, terminal()).unwrap();
    assert!(grant.bind(&binding, terminal()).is_err());
    assert_eq!(
        request(&protocol, &params, &commands).unwrap()["result"]["isError"],
        false
    );
    let invocation = calls.lock().unwrap()[0].clone();
    assert_eq!(invocation.command, "terminal.capture");
    assert_eq!(invocation.target, Some(terminal()));
    assert_eq!(invocation.caller, Caller::Socket);
    assert_eq!(invocation.arguments, ["plain", "screen", "4096"]);
    grant.revoke();
    assert_eq!(catalog(&protocol).unwrap(), json!([]));
    assert_eq!(
        request(&protocol, &params, &commands).unwrap()["result"]["isError"],
        true
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[rstest]
#[case("get_agent_status", json!({}), "agents.native.status", ResourceKind::Session)]
#[case("list_models", json!({"provider":"codex"}), "agents.native.models", ResourceKind::Session)]
#[case("list_agents", json!({}), "agents.native.activities", ResourceKind::Binding)]
#[case("get_agent_activity", json!({"limit":16}), "agents.native.activity", ResourceKind::Session)]
#[case("inspect_provider", json!({"provider":"codex"}), "agents.native.provider", ResourceKind::Session)]
#[case("list_providers", json!({}), "agents.native.provider", ResourceKind::Session)]
#[case("list_profiles", json!({}), "agents.native.profiles", ResourceKind::Session)]
fn native_reads_use_the_persisted_conversation_and_revoke_with_its_lease(
    binding: CommandTarget,
    #[case] tool: &str,
    #[case] arguments: Value,
    #[case] command: &str,
    #[case] resource: ResourceKind,
) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    grant.bind(&binding, terminal()).unwrap();
    let protocol = ToolProtocol::new(grant.clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let commands = recording(calls.clone());
    let params = json!({"name":tool,"arguments":arguments});
    assert_eq!(
        request(&protocol, &params, &commands).unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
    let target = CommandTarget {
        kind: ResourceKind::Session,
        handle: "native:codex:7".into(),
        generation: 7,
    };
    grant.bind_native_session(target.clone()).unwrap();
    assert!(grant.bind_native_session(target.clone()).is_err());
    assert!(
        catalog(&protocol)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == tool)
    );
    let response = request(&protocol, &params, &commands).unwrap();
    let result = &response["result"];
    assert_eq!(result["isError"], false);
    let structured = &result["structuredContent"];
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        *structured
    );
    match tool {
        "list_profiles" => {
            assert_eq!(structured["provider"], "codex");
            assert_eq!(structured["captured_profile"], "captured");
            assert_eq!(
                structured["profiles"],
                json!([{"id":"captured","name":"Captured"}])
            );
        }
        "get_agent_status" => {
            assert_eq!(structured["id"], "native:codex:7");
            assert_eq!(structured["title"], "Inspect changes");
            assert_eq!(structured["status"], "working");
        }
        "list_agents" => {
            assert_eq!(structured["count"], 1);
            assert_eq!(structured["agents"][0]["id"], "native:codex:7");
        }
        "get_agent_activity" => {
            assert_eq!(structured["id"], "native:codex:7");
            assert_eq!(structured["total"], 1);
            assert_eq!(structured["items"][0]["text"], "Recent response");
        }
        "inspect_provider" | "list_providers" => {
            let provider = if tool == "list_providers" {
                assert_eq!(structured["providers"].as_array().unwrap().len(), 1);
                &structured["providers"][0]
            } else {
                structured
            };
            assert_eq!(provider["provider"], "codex");
            assert_eq!(provider["model"], "model-a");
            assert_eq!(provider["reasoning_effort"], "high");
            assert_eq!(provider["fast_mode"], true);
            assert_eq!(provider["permissions"], "supervised");
        }
        _ => {
            assert_eq!(structured["count"], 1);
            assert_eq!(structured["provider"], "codex");
            assert_eq!(
                structured["models"][0]["reasoning_efforts"],
                json!(["low", "high"])
            );
        }
    }
    let captured = calls.lock().unwrap()[0].clone();
    assert_eq!(captured.command, command);
    if resource == ResourceKind::Binding {
        assert_eq!(captured.target, Some(binding));
        assert_eq!(captured.arguments, Vec::<String>::new());
    } else {
        assert_eq!(captured.target, Some(target));
        let mut expected = vec!["native:codex:7", "7"];
        if tool == "get_agent_activity" {
            expected.push("16");
        }
        assert_eq!(captured.arguments, expected);
    }
    assert_eq!(captured.caller, Caller::Socket);
    for arguments in [
        json!({"provider":"pi"}),
        json!({"target":"other"}),
        json!({"account":"other"}),
        json!({"command":"agents.native.interrupt"}),
    ] {
        assert_eq!(
            request(
                &protocol,
                &json!({"name":tool,"arguments":arguments}),
                &commands
            )
            .unwrap()["result"]["isError"],
            true
        );
    }
    assert_eq!(calls.lock().unwrap().len(), 1);
    let revoked = grant;
    let revoke_during_read =
        move |_: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
            revoked.revoke();
            assert!(cancellation.is_cancelled());
            success()
        };
    assert_eq!(
        request(&protocol, &params, &revoke_during_read).unwrap()["result"]["isError"],
        true
    );
    assert!(
        !catalog(&protocol)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == tool)
    );
}

#[rstest]
#[case(json!({"name":"terminal_read","arguments":{"target":"other"}}))]
#[case(json!({"name":"terminal_read","caller":"internal"}))]
#[case(json!({"name":"terminal_read","arguments":{"path":"/private"}}))]
#[case(json!({"name":"terminal_read","arguments":{"command":"terminal.spawn"}}))]
#[case(json!({"name":"terminal_read","arguments":{"max_lines":0}}))]
#[case(json!({"name":"terminal_read","arguments":{"max_lines":4097}}))]
#[case(json!({"name":"spawn","arguments":{}}))]
#[case(json!({"name":"get_agent_activity","arguments":{"limit":0}}))]
#[case(json!({"name":"get_agent_activity","arguments":{"limit":33}}))]
#[case(json!({"name":"get_agent_activity","arguments":{"limit":-1}}))]
#[case(json!({"name":"get_agent_activity","arguments":{"limit":1.5}}))]
#[case(json!({"name":"get_agent_activity","arguments":{"agentId":"sibling"}}))]
fn client_inputs_cannot_expand_host_authority(binding: CommandTarget, #[case] params: Value) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    grant.bind(&binding, terminal()).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(
        request(
            &ToolProtocol::new(grant),
            &params,
            &recording(calls.clone())
        )
        .unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[rstest]
#[case(false)]
#[case(true)]
fn revocation_or_disable_discards_an_already_started_read(
    binding: CommandTarget,
    #[case] disable: bool,
) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    grant.bind(&binding, terminal()).unwrap();
    let during_read = grant.clone();
    let commands = move |_: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
        assert!(cancellation.try_start());
        if disable {
            during_read.restrict(ToolPolicy::default());
        } else {
            during_read.revoke();
        }
        success()
    };
    assert_eq!(
        request(
            &ToolProtocol::new(grant),
            &json!({"name":"terminal_read"}),
            &commands
        )
        .unwrap()["result"]["isError"],
        true
    );
}

#[rstest]
fn captures_require_both_feature_policy_and_a_host_captured_command(binding: CommandTarget) {
    let policy = ToolPolicy {
        own_terminal_read: true,
        browser_capture: true,
        computer_capture: false,
        spawn_children: false,
    };
    let mut invocation = CommandInvocation::new(
        "browser.snapshot",
        vec!["17".into(), "1234567890abcdef1234567890abcdef".into()],
        Caller::Internal,
    );
    invocation.target = Some(CommandTarget {
        kind: ResourceKind::ApplicationWindow,
        handle: "host-window".into(),
        generation: 7,
    });
    let captures = vec![ToolCapturedCommand {
        capture: ToolCapture::Browser,
        invocation: invocation.clone(),
    }];
    assert_eq!(
        catalog(&ToolProtocol::new(
            lease(binding.clone(), policy, Vec::new()).unwrap()
        ))
        .unwrap()
        .as_array()
        .unwrap()
        .len(),
        6
    );
    let grant = lease(binding.clone(), policy, captures).unwrap();
    grant.bind(&binding, terminal()).unwrap();
    let protocol = ToolProtocol::new(grant.clone());
    assert_eq!(
        catalog(&protocol)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "get_workspace_info",
            "list_terminals",
            "terminal_read",
            "browser_snapshot",
            "computer_snapshot",
            "computer_input"
        ]
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let commands = recording(calls.clone());
    assert_eq!(
        request(
            &protocol,
            &json!({"name":"browser_snapshot","arguments":{"target":"foreign"}}),
            &commands
        )
        .unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(
        request(&protocol, &json!({"name":"browser_snapshot"}), &commands).unwrap()["result"]["isError"],
        false
    );
    invocation.caller = Caller::Socket;
    assert_eq!(calls.lock().unwrap()[0], invocation);
    let session = browser_session();
    grant.bind_native_session(session.clone()).unwrap();
    grant.attach_browser(&session, None).unwrap();
    assert_eq!(
        request(&protocol, &json!({"name":"browser_snapshot"}), &commands).unwrap()["result"]["isError"],
        true
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    grant.restrict(ToolPolicy::own_terminal());
    grant.restrict(policy);
    assert!(
        !catalog(&protocol)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "browser_snapshot")
    );
}

#[rstest]
#[case(ResourceKind::Binding, 7, vec!["17".into(), "1234567890abcdef1234567890abcdef".into()])]
#[case(ResourceKind::ApplicationWindow, 0, vec!["17".into(), "1234567890abcdef1234567890abcdef".into()])]
#[case(ResourceKind::ApplicationWindow, 7, vec!["17".into()])]
#[case(ResourceKind::ApplicationWindow, 7, vec!["0".into(), "1234567890abcdef1234567890abcdef".into()])]
#[case(ResourceKind::ApplicationWindow, 7, vec!["17".into(), "foreign-document".into()])]
#[case(ResourceKind::ApplicationWindow, 7, vec!["17".into(), "1234567890abcdef1234567890abcdef".into(), "foreign".into()])]
fn browser_grants_require_an_exact_window_page_and_document(
    binding: CommandTarget,
    #[case] kind: ResourceKind,
    #[case] generation: u64,
    #[case] arguments: Vec<String>,
) {
    let invocation = CommandInvocation {
        target: Some(CommandTarget {
            kind,
            handle: "host-window".into(),
            generation,
        }),
        ..CommandInvocation::new("browser.snapshot", arguments, Caller::Internal)
    };
    assert!(
        lease(
            binding,
            ToolPolicy {
                browser_capture: true,
                ..ToolPolicy::own_terminal()
            },
            vec![ToolCapturedCommand {
                capture: ToolCapture::Browser,
                invocation
            }]
        )
        .is_err()
    );
}

#[fixture]
fn browser_session() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Session,
        handle: "native:codex:browser".into(),
        generation: 7,
    }
}

#[fixture]
fn browser_grant(
    binding: CommandTarget,
    browser_session: CommandTarget,
) -> Result<ToolLease, String> {
    let grant = lease(
        binding.clone(),
        ToolPolicy {
            browser_capture: true,
            ..ToolPolicy::own_terminal()
        },
        Vec::new(),
    )?;
    grant.bind(&binding, terminal())?;
    grant.bind_native_session(browser_session)?;
    Ok(grant)
}

fn browser_page(page: u64) -> bootty_agents::NativeBrowserAttachment {
    bootty_agents::NativeBrowserAttachment {
        window: CommandTarget {
            kind: ResourceKind::ApplicationWindow,
            handle: "host-window".into(),
            generation: 17,
        },
        page,
        document: "1234567890abcdef1234567890abcdef".into(),
    }
}

#[rstest]
fn browser_catalog_does_not_grant_access_until_explicitly_attached(
    browser_grant: Result<ToolLease, String>,
    browser_session: CommandTarget,
) {
    let grant = browser_grant.unwrap();
    let protocol = ToolProtocol::new(grant.clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let commands = recording(calls.clone());
    assert!(
        catalog(&protocol)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "browser_snapshot")
    );
    let read = json!({"name":"browser_snapshot"});
    assert_eq!(
        request(&protocol, &read, &commands).unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
    let attachment = browser_page(19);
    grant
        .attach_browser(&browser_session, Some(attachment.clone()))
        .unwrap();
    assert_eq!(
        request(&protocol, &read, &commands).unwrap()["result"]["isError"],
        false
    );
    let observed = calls.lock().unwrap().clone();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].command, "browser.snapshot");
    assert_eq!(observed[0].caller, Caller::Socket);
    assert_eq!(observed[0].target, Some(attachment.window));
    assert_eq!(observed[0].arguments, vec!["19", &attachment.document]);
    grant.attach_browser(&browser_session, None).unwrap();
    assert_eq!(
        request(&protocol, &read, &commands).unwrap()["result"]["isError"],
        true
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[rstest]
fn browser_reads_withhold_retired_documents(
    browser_grant: Result<ToolLease, String>,
    browser_session: CommandTarget,
    #[values(false, true)] accepted: bool,
    #[values(false, true)] replace: bool,
) {
    let grant = browser_grant.unwrap();
    grant
        .attach_browser(&browser_session, Some(browser_page(19)))
        .unwrap();
    let during = grant.clone();
    let commands = move |_: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
        if accepted {
            assert!(cancellation.try_start());
        }
        during
            .attach_browser(&browser_session, replace.then(|| browser_page(20)))
            .unwrap();
        assert_eq!(cancellation.is_cancelled(), !accepted);
        CommandOutcome::Success {
            value: json!({"text":"PRIVATE_BROWSER_TEXT"}),
            warnings: Vec::new(),
        }
    };
    let response = request(
        &ToolProtocol::new(grant),
        &json!({"name":"browser_snapshot"}),
        &commands,
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(!response.to_string().contains("PRIVATE_BROWSER_TEXT"));
}

#[rstest]
fn browser_grants_cannot_cross_sessions_or_survive_renewal(
    binding: CommandTarget,
    browser_session: CommandTarget,
    browser_grant: Result<ToolLease, String>,
) {
    let grant = browser_grant.unwrap();
    let mut foreign = browser_session.clone();
    foreign.generation = foreign.generation.checked_add(1).unwrap();
    assert!(
        grant
            .attach_browser(&foreign, Some(browser_page(19)))
            .is_err()
    );
    assert!(
        grant
            .attach_browser(&browser_session, Some(browser_page(0)))
            .is_err()
    );
    grant
        .attach_browser(&browser_session, Some(browser_page(19)))
        .unwrap();
    let renewed = lease(
        binding.clone(),
        ToolPolicy {
            browser_capture: true,
            ..ToolPolicy::own_terminal()
        },
        Vec::new(),
    )
    .unwrap();
    renewed.bind(&binding, terminal()).unwrap();
    renewed
        .bind_native_session(browser_session.clone())
        .unwrap();
    assert_eq!(renewed.browser_attachment(), None);
    grant.restrict(ToolPolicy::own_terminal());
    assert_eq!(grant.browser_attachment(), None);
    assert!(
        grant
            .attach_browser(&browser_session, Some(browser_page(19)))
            .is_err()
    );
    grant.revoke();
    assert!(!grant.browser_attachments_supported());
}

#[rstest]
fn unsupported_capture_commands_are_rejected(binding: CommandTarget) {
    let result = ToolLease::issue(
        ToolScope {
            provider: AgentKind::Pi,
            binding,
        },
        Caller::Socket,
        ToolPolicy::own_terminal(),
        vec![ToolCapturedCommand {
            capture: ToolCapture::Computer,
            invocation: CommandInvocation::new("terminal.spawn", Vec::new(), Caller::Internal),
        }],
    );
    assert!(result.is_err());
}

#[rstest]
fn notifications_and_oversized_messages_never_execute(binding: CommandTarget) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    grant.bind(&binding, terminal()).unwrap();
    let protocol = ToolProtocol::new(grant);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let commands = recording(calls.clone());
    assert!(
        protocol
            .handle(
                br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"terminal_read"}}"#,
                Instant::now(),
                &commands
            )
            .is_none()
    );
    let response = protocol
        .handle(
            &vec![b' '; MAX_TOOL_MESSAGE_BYTES + 1],
            Instant::now(),
            &commands,
        )
        .unwrap();
    assert_eq!(response["error"]["code"], -32600);
    assert!(calls.lock().unwrap().is_empty());
}

proptest! {
    #[test]
    fn terminal_read_inputs_map_to_bounded_exact_target_commands(lines in 1_u32..=4096, history in any::<bool>()) {
        let binding = binding();
        let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
        grant.bind(&binding, terminal()).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let now = Instant::now();
        let calls_copy = calls.clone();
        let commands = move |invocation: CommandInvocation, deadline: Instant, _: CommandCancellation| {
            assert_eq!(deadline, now.checked_add(Duration::from_secs(5)).unwrap());
            calls_copy.lock().unwrap().push(invocation);
            success()
        };
        let scope = if history {"history"} else {"screen"};
        let bytes = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"terminal_read","arguments":{"scope":scope,"max_lines":lines}}})).unwrap();
        let result = ToolProtocol::new(grant).handle(&bytes, now, &commands).unwrap();
        prop_assert_eq!(&result["result"]["isError"], &json!(false));
        let calls = calls.lock().unwrap();
        prop_assert_eq!(&calls[0].arguments, &vec!["plain".to_owned(), scope.to_owned(), lines.to_string()]);
        prop_assert_eq!(&calls[0].target, &Some(terminal()));
        drop(calls);
    }

    #[test]
    fn attenuation_never_enables_a_capability(a in any::<(bool,bool,bool)>(), b in any::<(bool,bool,bool)>()) {
        let original = ToolPolicy {own_terminal_read:a.0, browser_capture:a.1, computer_capture:a.2, spawn_children:false};
        let requested = ToolPolicy {own_terminal_read:b.0, browser_capture:b.1, computer_capture:b.2, spawn_children:false};
        let result = original.attenuate(requested);
        prop_assert_eq!((result.own_terminal_read, result.browser_capture, result.computer_capture), (a.0 && b.0, a.1 && b.1, a.2 && b.2));
    }
}

#[cfg(unix)]
mod private_transport {
    use super::*;
    use bootty_agents::{ToolBridge, ToolBridgeContext, tool_stdio};
    use pretty_assertions::assert_eq;
    use std::{
        fs,
        io::{BufRead as _, BufReader, Cursor, Write as _},
        os::unix::{fs::PermissionsExt as _, net::UnixStream},
        path::{Path, PathBuf},
    };

    const fn context(binding: CommandTarget, provider: AgentKind) -> ToolBridgeContext {
        ToolBridgeContext {
            scope: ToolScope { provider, binding },
            caller: Caller::Cli,
            policy: ToolPolicy::own_terminal(),
            captures: Vec::new(),
            spawn: None,
        }
    }

    fn connection(provider: AgentKind, arguments: &[String]) -> Result<PathBuf, String> {
        match provider {
            AgentKind::Codex => {
                let serialized = arguments
                    .iter()
                    .find_map(|argument| argument.split_once(".args=").map(|(_, value)| value))
                    .ok_or("Missing native Codex args override")?;
                let args: Vec<String> =
                    serde_json::from_str(serialized).map_err(|error| error.to_string())?;
                assert_eq!(args.first().map(String::as_str), Some("--agent-tool-stdio"));
                args.get(1)
                    .map(PathBuf::from)
                    .ok_or_else(|| "Missing connection path".to_owned())
            }
            AgentKind::Claude => {
                let path = arguments.get(1).ok_or("Missing private Claude config")?;
                let config: Value =
                    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
                        .map_err(|error| error.to_string())?;
                let servers = config
                    .get("mcpServers")
                    .and_then(Value::as_object)
                    .ok_or("Missing MCP servers")?;
                assert_eq!(servers.len(), 1);
                let server = servers.values().next().ok_or("Missing MCP server")?;
                assert_eq!(server.get("command"), Some(&json!("/host/bootty-dev")));
                assert_eq!(server.get("type"), Some(&json!("stdio")));
                let args = server
                    .get("args")
                    .and_then(Value::as_array)
                    .ok_or("Missing stdio args")?;
                assert_eq!(args.first(), Some(&json!("--agent-tool-stdio")));
                args.get(1)
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .ok_or_else(|| "Missing connection path".to_owned())
            }
            AgentKind::Pi => Path::new(arguments.get(1).ok_or("Missing Pi extension")?)
                .parent()
                .map(|parent| parent.join("connection.json"))
                .ok_or_else(|| "Missing private extension directory".to_owned()),
        }
    }

    #[rstest]
    #[case(AgentKind::Codex, "--config")]
    #[case(AgentKind::Claude, "--mcp-config")]
    #[case(AgentKind::Pi, "--extension")]
    fn native_launch_configuration_is_ephemeral_private_and_keeps_secret_out_of_arguments(
        binding: CommandTarget,
        #[case] provider: AgentKind,
        #[case] flag: &str,
    ) {
        let bridge = ToolBridge::prepare(
            context(binding, provider),
            Path::new("/host/bootty-dev"),
            Arc::new(recording(Arc::default())),
        )
        .unwrap();
        let arguments = bridge.arguments();
        assert_eq!(arguments[0], flag);
        let connection = connection(provider, &arguments).unwrap();
        let secret: Value = serde_json::from_slice(&fs::read(&connection).unwrap()).unwrap();
        let token = secret["token"].as_str().unwrap();
        assert_eq!(token.len(), 64);
        assert!(arguments.iter().all(|argument| !argument.contains(token)));
        assert_eq!(
            fs::metadata(&connection).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(connection.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        if provider != AgentKind::Codex {
            let artifact = Path::new(&arguments[1]);
            assert_eq!(artifact.parent(), connection.parent());
            assert_eq!(
                fs::metadata(artifact).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(!fs::read_to_string(artifact).unwrap().contains(token));
        }
    }

    #[rstest]
    fn private_stdio_round_trip_invokes_the_shared_path_and_authenticates_the_endpoint(
        binding: CommandTarget,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let bridge = ToolBridge::prepare(
            context(binding.clone(), AgentKind::Claude),
            Path::new("/host/bootty-dev"),
            Arc::new(recording(calls.clone())),
        )
        .unwrap();
        bridge.lease().bind(&binding, terminal()).unwrap();
        let connection = connection(AgentKind::Claude, &bridge.arguments()).unwrap();
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"terminal_read\"}}\n"
        );
        let mut output = Vec::new();
        tool_stdio(&connection, &mut Cursor::new(input.as_bytes()), &mut output).unwrap();
        let output_text = std::str::from_utf8(&output).unwrap();
        let response_lines = output_text.split_terminator('\n').collect::<Vec<_>>();
        assert_eq!(response_lines.len(), 3, "one JSON line per MCP response");
        let responses = response_lines
            .into_iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[2]["result"]["isError"], false);
        let calls_guard = calls.lock().unwrap();
        assert_eq!(calls_guard.len(), 1);
        assert_eq!(calls_guard[0].caller, Caller::Cli);
        assert_eq!(calls_guard[0].target, Some(terminal()));
        drop(calls_guard);
        let secret: Value = serde_json::from_slice(&fs::read(&connection).unwrap()).unwrap();
        assert!(
            !String::from_utf8(output)
                .unwrap()
                .contains(secret["token"].as_str().unwrap())
        );
        let mut unauthenticated = UnixStream::connect(secret["socket"].as_str().unwrap()).unwrap();
        unauthenticated.write_all(b"{\"token\":\"forged\",\"request\":{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"terminal_read\"}}}\n").unwrap();
        let mut response = String::new();
        assert_eq!(
            BufReader::new(unauthenticated)
                .read_line(&mut response)
                .unwrap(),
            0
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
        bridge.stop();
        assert!(!bridge.lease().enabled(None));
    }

    #[rstest]
    fn stdio_rejects_nonprivate_connections_and_bounded_truncated_input(binding: CommandTarget) {
        let bridge = ToolBridge::prepare(
            context(binding, AgentKind::Codex),
            Path::new("/host/bootty-dev"),
            Arc::new(recording(Arc::default())),
        )
        .unwrap();
        let connection = connection(AgentKind::Codex, &bridge.arguments()).unwrap();
        let mut output = Vec::new();
        assert!(tool_stdio(&connection, &mut Cursor::new(b"{}"), &mut output).is_err());
        assert!(
            tool_stdio(
                &connection,
                &mut Cursor::new(vec![b' '; MAX_TOOL_MESSAGE_BYTES + 1]),
                &mut output
            )
            .is_err()
        );
        fs::set_permissions(&connection, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(tool_stdio(&connection, &mut Cursor::new(b"{}\n"), &mut output).is_err());
        assert_eq!(output, Vec::<u8>::new());
    }
}

fn app_mention() -> bootty_agents::NativeApplicationMention {
    bootty_agents::NativeApplicationMention {
        id: "mentioned-app".into(),
        target: bootty_computer::ComputerTarget {
            window_id: 42,
            process_id: 19,
            bundle_id: "test.App".into(),
            launch_time: 1.,
            bounds: bootty_computer::DisplayBounds {
                x: 0.,
                y: 0.,
                width: 100.,
                height: 100.,
            },
            title: Some("Test".into()),
        },
        prompt_range: 0..4,
    }
}
#[rstest]
#[case(false)]
#[case(true)]
fn application_input_uses_only_the_prompt_scope_and_discards_revoked_results(
    binding: CommandTarget,
    #[case] revoke: bool,
) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    grant.bind(&binding, terminal()).unwrap();
    let session = CommandTarget {
        kind: ResourceKind::Session,
        handle: "conversation".into(),
        generation: 3,
    };
    let mention = app_mention();
    grant.grant_applications(&session, &[mention]).unwrap();
    let protocol = ToolProtocol::new(grant.clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let received = calls.clone();
    let commands = |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        received.lock().unwrap().push(invocation);
        if revoke {
            grant.grant_applications(&session, &[]).unwrap();
        }
        success()
    };
    let result=request(&protocol,&json!({"name":"computer_input","arguments":{"application":"mentioned-app","action":{"action":"type_text","text":"hello"}}}),&commands).unwrap();
    assert_eq!(result["result"]["isError"], revoke);
    let invocation = calls.lock().unwrap()[0].clone();
    assert_eq!(invocation.command, "agents.native.computer");
    assert_eq!(invocation.target.as_ref(), Some(&session));
    assert_eq!(invocation.arguments[2], "mentioned-app");
    assert_eq!(invocation.caller, Caller::Socket);
    let before = calls.lock().unwrap().len();
    let invalid=request(&protocol,&json!({"name":"computer_input","arguments":{"application":"other-app","target":app_mention().target,"action":{"action":"type_text","text":"hello"}}}),&commands).unwrap();
    assert_eq!(invalid["result"]["isError"], true);
    assert_eq!(calls.lock().unwrap().len(), before);
}
#[rstest]
fn new_prompt_revokes_pending_application_authority(binding: CommandTarget) {
    let grant = lease(binding.clone(), ToolPolicy::own_terminal(), Vec::new()).unwrap();
    grant.bind(&binding, terminal()).unwrap();
    let session = CommandTarget {
        kind: ResourceKind::Session,
        handle: "conversation".into(),
        generation: 3,
    };
    grant
        .grant_applications(&session, &[app_mention()])
        .unwrap();
    let access = grant.application_access(&session, "mentioned-app").unwrap();
    let token = CommandCancellation::new();
    let guard = access.begin(token.clone()).unwrap();
    grant.grant_applications(&session, &[]).unwrap();
    assert!(token.is_cancelled());
    assert!(!access.current());
    assert!(access.begin(CommandCancellation::new()).is_err());
    drop(guard);
}
