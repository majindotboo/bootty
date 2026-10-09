use std::{
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

use bootty_agents::{
    AgentCommandExecutor, AgentKind, ToolBridge, ToolLease, ToolPolicy, ToolProtocol, ToolScope,
    ToolSpawnContext, ToolSpawnRequest,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, CommandWarning,
    ResourceKind,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};
use serde_json::{Value, json};

#[fixture]
fn binding() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Binding,
        handle: "captured-space".to_owned(),
        generation: 91,
    }
}

fn parent_terminal() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "parent-terminal".to_owned(),
        generation: 22,
    }
}

fn child_terminal() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "child-terminal".to_owned(),
        generation: 23,
    }
}

fn child_session() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Session,
        handle: "child-session".to_owned(),
        generation: 23,
    }
}

fn spawn_lease(binding: CommandTarget, policy: ToolPolicy) -> Result<ToolLease, String> {
    ToolLease::issue_with_spawn(
        ToolScope {
            provider: AgentKind::Claude,
            binding,
        },
        Caller::Socket,
        policy,
        Vec::new(),
        Some(ToolSpawnContext {
            profile: Some("work".to_owned()),
        }),
    )
}

const fn spawning_policy() -> ToolPolicy {
    ToolPolicy {
        own_terminal_read: true,
        spawn_children: true,
        browser_capture: true,
        computer_capture: true,
    }
}

const fn succeeded(value: Value) -> CommandOutcome {
    CommandOutcome::Success {
        value,
        warnings: Vec::new(),
    }
}

fn record(calls: Arc<Mutex<Vec<CommandInvocation>>>) -> impl AgentCommandExecutor {
    move |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(invocation);
        succeeded(json!({"identity":"host-issued-child","terminal":"issued-child"}))
    }
}

fn call(
    protocol: &ToolProtocol,
    name: &str,
    arguments: &Value,
    commands: &dyn AgentCommandExecutor,
) -> Result<Value, String> {
    let bytes=serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})).map_err(|error| error.to_string())?;
    protocol
        .handle(&bytes, Instant::now(), commands)
        .ok_or_else(|| "Expected request response".to_owned())
}

#[fixture]
fn shell_grant(binding: CommandTarget) -> Result<ToolLease, String> {
    let parent = spawn_lease(binding.clone(), spawning_policy())?;
    parent.bind(&binding, parent_terminal())?;
    let commands = |_: CommandInvocation, _: Instant, _: CommandCancellation| {
        succeeded(json!({"created":child_session(),"terminal":child_terminal()}))
    };
    let result = call(
        &ToolProtocol::new(parent.clone()),
        "spawn_shell",
        &json!({"name":"child"}),
        &commands,
    )?;
    if result.pointer("/result/isError").and_then(Value::as_bool) != Some(false) {
        return Err("Fixture shell creation was rejected".into());
    }
    Ok(parent)
}

#[rstest]
#[case("read_spawned_terminal", "terminal.capture", json!({}), vec!["plain", "screen", "128"])]
#[case("paste_spawned_terminal", "terminal.paste", json!({"text":"Literal 🥟\ntext"}), vec!["Literal 🥟\ntext"])]
#[case("submit_spawned_terminal", "terminal.submit", json!({}), vec![])]
#[case("interrupt_spawned_terminal", "terminal.write", json!({}), vec!["\u{3}"])]
#[case("close_spawned_terminal", "pane.close", json!({}), vec![])]
fn shell_tools_use_the_host_created_target_and_shared_command(
    shell_grant: Result<ToolLease, String>,
    #[case] tool: &str,
    #[case] command: &str,
    #[case] mut arguments: Value,
    #[case] expected: Vec<&str>,
) {
    let shell_grant = shell_grant.unwrap();
    arguments["terminal"] = json!(child_terminal());
    let commands = |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        assert_eq!(invocation.command, command);
        assert_eq!(invocation.target, Some(child_terminal()));
        assert_eq!(invocation.caller, Caller::Socket);
        assert_eq!(invocation.arguments, expected);
        assert_eq!(invocation.confirmation.is_some(), command == "pane.close");
        succeeded(json!({"text":"child screen"}))
    };
    assert_eq!(
        call(&ToolProtocol::new(shell_grant), tool, &arguments, &commands).unwrap()["result"]["isError"],
        false
    );
}

#[rstest]
#[case(Value::Null)]
#[case(json!("not a read argument"))]
fn shell_reads_reject_undeclared_input(
    shell_grant: Result<ToolLease, String>,
    #[case] text: Value,
) {
    let commands = |_: CommandInvocation, _: Instant, _: CommandCancellation| {
        panic!("Invalid read input must not reach the host")
    };
    let result = call(
        &ToolProtocol::new(shell_grant.unwrap()),
        "read_spawned_terminal",
        &json!({"terminal":child_terminal(),"text":text}),
        &commands,
    )
    .unwrap();
    assert_eq!(result["result"]["isError"], true);
}

#[rstest]
#[case::not_created(false, false)]
#[case::parent_terminal(true, true)]
fn shell_receipts_do_not_grant_input_to_existing_or_parent_terminals(
    binding: CommandTarget,
    #[case] created: bool,
    #[case] parent_target: bool,
) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    let terminal = if parent_target {
        parent_terminal()
    } else {
        child_terminal()
    };
    let commands = |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        assert_eq!(invocation.command, "agents.spawn");
        succeeded(
            json!({"created":if created {json!(child_session())} else {Value::Null},"terminal":terminal}),
        )
    };
    let protocol = ToolProtocol::new(parent);
    assert_eq!(
        call(
            &protocol,
            "spawn_shell",
            &json!({"name":"child"}),
            &commands
        )
        .unwrap()["result"]["isError"],
        false
    );
    assert_eq!(
        call(
            &protocol,
            "paste_spawned_terminal",
            &json!({"terminal":terminal,"text":"not authorized"}),
            &commands
        )
        .unwrap()["result"]["isError"],
        true
    );
}

#[rstest]
#[case::revoke(true)]
#[case::disable(false)]
fn withdrawing_shell_authority_discards_completed_reads(
    shell_grant: Result<ToolLease, String>,
    #[case] revoke: bool,
) {
    let shell_grant = shell_grant.unwrap();
    let parent = shell_grant.clone();
    let commands = move |_: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
        assert!(cancellation.try_start());
        if revoke {
            parent.revoke();
        } else {
            parent.restrict(ToolPolicy::own_terminal());
        }
        succeeded(json!({"text":"private terminal output"}))
    };
    let result = call(
        &ToolProtocol::new(shell_grant),
        "read_spawned_terminal",
        &json!({"terminal":child_terminal()}),
        &commands,
    )
    .unwrap();
    assert_eq!(result["result"]["isError"], true);
    assert!(!result.to_string().contains("private terminal output"));
}

#[rstest]
#[case::pending(false)]
#[case::accepted(true)]
fn shell_mutations_preserve_acceptance_after_parent_revocation(
    shell_grant: Result<ToolLease, String>,
    #[case] accepted: bool,
) {
    let shell_grant = shell_grant.unwrap();
    let parent = shell_grant.clone();
    let commands = move |_: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
        if accepted {
            assert!(cancellation.try_start());
        }
        parent.revoke();
        assert_eq!(cancellation.is_cancelled(), !accepted);
        if accepted {
            succeeded(json!(null))
        } else {
            CommandOutcome::cancelled()
        }
    };
    let result = call(
        &ToolProtocol::new(shell_grant),
        "submit_spawned_terminal",
        &json!({"terminal":child_terminal()}),
        &commands,
    )
    .unwrap();
    assert_eq!(result["result"]["isError"], !accepted);
}

proptest! {
    #[test]
    fn shell_paste_keeps_literal_unicode(text in any::<String>()) {
        let lease = shell_grant(binding()).unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let writes = Arc::clone(&observed);
        let commands = move |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
            writes.lock().unwrap().push(invocation);
            succeeded(json!(null))
        };
        let result = call(&ToolProtocol::new(lease), "paste_spawned_terminal", &json!({"terminal":child_terminal(),"text":text}), &commands).unwrap();
        prop_assert_eq!(&result["result"]["isError"], &json!(false));
        let calls = observed.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, vec![text]);
        assert_eq!(calls[0].target, Some(child_terminal()));
    }
}

#[rstest]
#[case::escaped_boundary(64 * 1024, false)]
#[case::over_boundary(64 * 1024 + 1, true)]
fn shell_paste_bounds_decoded_bytes_without_rejecting_json_escaping(
    shell_grant: Result<ToolLease, String>,
    #[case] bytes: usize,
    #[case] denied: bool,
) {
    let shell_grant = shell_grant.unwrap();
    let text = "\n".repeat(bytes);
    let commands = |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        assert!(!denied, "Oversized text must not reach the terminal");
        assert_eq!(invocation.arguments, vec![text.clone()]);
        succeeded(json!(null))
    };
    let result = call(
        &ToolProtocol::new(shell_grant),
        "paste_spawned_terminal",
        &json!({"terminal":child_terminal(),"text":text}),
        &commands,
    )
    .unwrap();
    assert_eq!(result["result"]["isError"], denied);
}

#[rstest]
#[case::pending(false)]
#[case::accepted(true)]
fn native_child_control_tracks_parent_revocation_through_acceptance(
    binding: CommandTarget,
    #[case] accepted: bool,
) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    parent
        .bind_native_session(CommandTarget {
            kind: ResourceKind::Session,
            handle: "native:claude:7".into(),
            generation: 7,
        })
        .unwrap();
    let incoming = CommandCancellation::new();
    let guard = parent.begin_child_control(incoming.clone()).unwrap();
    if accepted {
        assert!(incoming.try_start());
    }
    parent.revoke();
    assert_eq!(guard.cancellation().is_cancelled(), !accepted);
    assert!(
        parent
            .begin_child_control(CommandCancellation::new())
            .is_err()
    );
}

#[rstest]
#[case(json!({"id":"native:claude:8","generation":0}))]
#[case(json!({"id":"native:claude:8","generation":8,"target":"foreign"}))]
#[case(json!({"id":"native:claude:8","generation":8,"caller":"internal"}))]
#[case(json!({"id":"native:claude:8","generation":8,"operation":"stop"}))]
fn child_control_rejects_forged_authority_before_forwarding(
    binding: CommandTarget,
    #[case] arguments: Value,
) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    parent
        .bind_native_session(CommandTarget {
            kind: ResourceKind::Session,
            handle: "native:claude:7".into(),
            generation: 7,
        })
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(
        call(
            &ToolProtocol::new(parent),
            "stop_spawned_agent",
            &arguments,
            &record(calls.clone())
        )
        .unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[rstest]
#[case(json!({"kind":"shell","name":"child","argv":["sh","-c","anything"]}))]
#[case(json!({"kind":"shell","name":"child","cwd":"/other"}))]
#[case(json!({"kind":"shell","name":"child","identity":"client-issued"}))]
#[case(json!({"kind":"shell","name":"child","target":"other-space"}))]
#[case(json!({"kind":"agent","name":"child","provider":"claude","prompt":"work","program":"override"}))]
#[case(json!({"kind":"agent","name":"child","provider":"claude","prompt":"work","account_directory":"/other-account"}))]
#[case(json!({"kind":"agent","name":"child","provider":"claude","prompt":""}))]
#[case(json!({"kind":"agent","name":"child","provider":"claude","prompt":"work","policy":{"spawn_children":true}}))]
fn typed_spawn_rejects_client_authority_paths_and_arbitrary_commands(#[case] request: Value) {
    assert!(ToolSpawnRequest::parse(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[rstest]
#[case("spawn_shell",json!({"name":"child","title":"Review checkout"}))]
#[case("spawn_agent",json!({"name":"child","title":"Review checkout","provider":"claude","profile":"work","prompt":"Quotes; $HOME `uname`\nand another line"}))]
fn typed_spawn_uses_original_caller_and_exact_registered_parent(
    binding: CommandTarget,
    #[case] name: &str,
    #[case] arguments: Value,
) {
    let lease = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    lease.bind(&binding, parent_terminal()).unwrap();
    let attachment_id = lease.attachment_id().to_string();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let response = call(
        &ToolProtocol::new(lease),
        name,
        &arguments,
        &record(calls.clone()),
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], false);
    let invocation = calls.lock().unwrap()[0].clone();
    assert_eq!(invocation.command, "agents.spawn");
    assert_eq!(invocation.arguments.get(1), Some(&attachment_id));
    assert_eq!(invocation.caller, Caller::Socket);
    assert_eq!(invocation.target, Some(parent_terminal()));
    let forwarded: Value = serde_json::from_str(&invocation.arguments[0]).unwrap();
    assert_eq!(forwarded["name"], arguments["name"]);
    assert_eq!(forwarded["title"], arguments["title"]);
    if name == "spawn_agent" {
        assert_eq!(forwarded["prompt"], arguments["prompt"]);
    }
}

#[rstest]
#[case::pending(false, false)]
#[case::revoked(true, true)]
#[case::default_denied(true, false)]
fn pending_revoked_or_default_policy_creates_nothing(
    binding: CommandTarget,
    #[case] bound: bool,
    #[case] revoke: bool,
) {
    let policy = if revoke || !bound {
        spawning_policy()
    } else {
        ToolPolicy::own_terminal()
    };
    let lease = spawn_lease(binding.clone(), policy).unwrap();
    if bound {
        lease.bind(&binding, parent_terminal()).unwrap();
    }
    if revoke {
        lease.revoke();
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let response = call(
        &ToolProtocol::new(lease),
        "spawn_shell",
        &json!({"name":"child"}),
        &record(calls.clone()),
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(calls.lock().unwrap().len(), 0);
}

#[rstest]
#[case(json!({"name":"child","provider":"pi","prompt":"work"}))]
#[case(json!({"name":"child","provider":"claude","profile":"another-account","prompt":"work"}))]
#[case(json!({"name":"child","provider":"claude","prompt":"work","target":"foreign"}))]
fn unsupported_provider_profile_and_target_changes_never_reach_creation(
    binding: CommandTarget,
    #[case] arguments: Value,
) {
    let lease = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    lease.bind(&binding, parent_terminal()).unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let response = call(
        &ToolProtocol::new(lease),
        "spawn_agent",
        &arguments,
        &record(calls.clone()),
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(calls.lock().unwrap().len(), 0);
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn child_tool_authority_is_read_only_and_follows_ancestor_revocation(
    binding: CommandTarget,
    #[case] started: bool,
) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    let request = ToolSpawnRequest::Shell {
        name: "child".to_owned(),
        title: None,
    };
    let authority = parent.authorize_spawn(&request).unwrap();
    assert_eq!(authority.scope().binding, binding);
    assert_eq!(authority.caller(), Caller::Socket);
    let bridge = ToolBridge::prepare_child(
        authority,
        Path::new("/host/bootty-dev"),
        Arc::new(record(Arc::default())),
    )
    .unwrap();
    bridge.lease().bind(&binding, child_terminal()).unwrap();
    bridge
        .lease()
        .bind_native_session(CommandTarget {
            kind: ResourceKind::Session,
            handle: "native:codex:child".into(),
            generation: 1,
        })
        .unwrap();
    let child_protocol = ToolProtocol::new(bridge.lease().clone());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let child_commands = record(calls.clone());
    let catalog = child_protocol
        .handle(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            Instant::now(),
            &child_commands,
        )
        .unwrap();
    assert_eq!(
        catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].clone())
            .collect::<Vec<_>>(),
        [
            json!("get_agent_activity"),
            json!("get_agent_status"),
            json!("list_models"),
            json!("list_providers"),
            json!("inspect_provider"),
            json!("terminal_read")
        ]
    );
    assert_eq!(
        call(&child_protocol, "list_agents", &json!({}), &child_commands).unwrap()["result"]["isError"],
        true
    );
    assert_eq!(
        call(
            &child_protocol,
            "list_profiles",
            &json!({}),
            &child_commands
        )
        .unwrap()["result"]["isError"],
        true
    );
    assert_eq!(
        call(
            &child_protocol,
            "get_workspace_info",
            &json!({}),
            &child_commands
        )
        .unwrap()["result"]["isError"],
        true
    );
    assert_eq!(
        call(
            &child_protocol,
            "list_terminals",
            &json!({}),
            &child_commands
        )
        .unwrap()["result"]["isError"],
        true
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(!bridge.lease().spawn_enabled());
    assert!(!bridge.lease().browser_attachments_supported());
    assert!(
        bridge
            .lease()
            .attach_browser(&bridge.lease().native_session_target().unwrap(), None)
            .is_err()
    );
    assert!(
        !bridge
            .lease()
            .enabled(Some(bootty_agents::ToolCapture::Browser))
    );
    assert!(
        !bridge
            .lease()
            .enabled(Some(bootty_agents::ToolCapture::Computer))
    );
    assert!(bridge.lease().authorize_spawn(&request).is_err());
    let parent_copy = parent;
    let commands =
        move |invocation: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
            assert_eq!(invocation.target, Some(child_terminal()));
            if started {
                assert!(cancellation.try_start());
            }
            parent_copy.revoke();
            assert_eq!(cancellation.is_cancelled(), !started);
            succeeded(json!({"text":"private child read"}))
        };
    let response = call(
        &ToolProtocol::new(bridge.lease().clone()),
        "terminal_read",
        &json!({}),
        &commands,
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(!bridge.lease().enabled(None));
}

#[rstest]
fn accepted_spawn_result_keeps_issued_child_ids_after_parent_revocation(binding: CommandTarget) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    let accepted = parent.clone();
    let commands = move |_: CommandInvocation, _: Instant, _: CommandCancellation| {
        accepted.revoke();
        CommandOutcome::Success {
            value: json!({"identity":"host-issued-child","terminal":"issued-child"}),
            warnings: vec![CommandWarning {
                code: "child_tools_revoked".to_owned(),
                message:
                    "Child created; parent authority was revoked and child tools are unavailable"
                        .to_owned(),
            }],
        }
    };
    let response = call(
        &ToolProtocol::new(parent),
        "spawn_shell",
        &json!({"name":"child"}),
        &commands,
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], false);
    let outcome: CommandOutcome =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let CommandOutcome::Success { value, warnings } = outcome else {
        panic!("Accepted spawn lost issued IDs")
    };
    assert_eq!(value["identity"], "host-issued-child");
    assert_eq!(warnings.len(), 1);
}

proptest! {
    #[test]
    fn spawn_permission_is_never_added_by_attenuation(original in any::<bool>(), requested in any::<bool>()) {
        let policy=ToolPolicy{spawn_children:original,..ToolPolicy::own_terminal()};
        let narrowed=policy.attenuate(ToolPolicy{spawn_children:requested,..ToolPolicy::own_terminal()});
        prop_assert_eq!(narrowed.spawn_children,original && requested);
    }
}

#[rstest]
fn queued_spawn_rechecks_parent_authority_before_the_mutation_boundary(binding: CommandTarget) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    let queued = parent.clone();
    let mutations = Arc::new(Mutex::new(0_u32));
    let mutations_copy = mutations.clone();
    let commands = move |invocation: CommandInvocation, _: Instant, _: CommandCancellation| {
        queued.revoke();
        let request = ToolSpawnRequest::parse(invocation.arguments[0].as_bytes()).unwrap();
        match queued.authorize_spawn(&request) {
            Ok(_) => {
                let mut mutations = mutations_copy.lock().unwrap();
                *mutations = mutations.checked_add(1).unwrap();
                drop(mutations);
                succeeded(json!({"identity":"host-issued-child"}))
            }
            Err(message) => CommandOutcome::Unavailable { message },
        }
    };
    let response = call(
        &ToolProtocol::new(parent),
        "spawn_shell",
        &json!({"name":"child"}),
        &commands,
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(*mutations.lock().unwrap(), 0);
}

#[rstest]
#[case(json!({"kind":"shell","name":"x".repeat(257)}))]
#[case(json!({"kind":"shell","name":"child","title":"x".repeat(257)}))]
#[case(json!({"kind":"agent","name":"child","provider":"claude","prompt":"x".repeat(65537)}))]
fn typed_request_bounds_are_validated_before_host_creation(#[case] request: Value) {
    assert!(ToolSpawnRequest::parse(&serde_json::to_vec(&request).unwrap()).is_err());
}

#[cfg(unix)]
#[rstest]
#[case(false)]
#[case(true)]
fn child_reads_never_exceed_the_parent_read_policy(
    binding: CommandTarget,
    #[case] read_enabled: bool,
) {
    let parent = spawn_lease(
        binding.clone(),
        ToolPolicy {
            own_terminal_read: read_enabled,
            ..spawning_policy()
        },
    )
    .unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    let authority = parent
        .authorize_spawn(&ToolSpawnRequest::Shell {
            name: "child".to_owned(),
            title: None,
        })
        .unwrap();
    let bridge = ToolBridge::prepare_child(
        authority,
        Path::new("/host/bootty-dev"),
        Arc::new(record(Arc::default())),
    )
    .unwrap();
    bridge.lease().bind(&binding, child_terminal()).unwrap();
    assert_eq!(bridge.lease().enabled(None), read_enabled);
    parent.restrict(ToolPolicy {
        own_terminal_read: false,
        ..spawning_policy()
    });
    assert!(!bridge.lease().enabled(None));
    parent.restrict(spawning_policy());
    assert!(!bridge.lease().enabled(None));
}

#[rstest]
#[case::revoked_before_acceptance(false, false)]
#[case::disabled_before_acceptance(false, true)]
#[case::revoked_after_acceptance(true, false)]
#[case::disabled_after_acceptance(true, true)]
fn the_same_tracked_guard_token_reaches_the_shared_creation_acceptance_gate(
    binding: CommandTarget,
    #[case] accepted: bool,
    #[case] disabled: bool,
) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    let request = ToolSpawnRequest::Shell {
        name: "child".to_owned(),
        title: None,
    };
    let incoming = CommandCancellation::new();
    let guard = parent
        .begin_spawn_with_cancellation(&request, incoming.clone())
        .unwrap();
    let (sender, receiver) = bootty_control::app_command_channel(1, Arc::new(|| {}));
    let mut create = CommandInvocation::new(
        "session.create",
        vec!["child".to_owned(), "/captured/project".to_owned()],
        guard.caller(),
    );
    create.target = Some(binding);
    let response = sender
        .for_caller(guard.caller())
        .submit(create, Instant::now(), guard.cancellation())
        .unwrap();
    let queued = receiver.try_recv().unwrap();
    if accepted {
        assert!(queued.cancellation.try_start());
    }
    if disabled {
        parent.restrict(ToolPolicy::own_terminal());
    } else {
        parent.revoke();
    }
    assert_eq!(queued.cancellation.is_cancelled(), !accepted);
    assert_eq!(incoming.is_cancelled(), !accepted);
    let mutation_accepted = accepted || queued.cancellation.try_start();
    assert_eq!(mutation_accepted, accepted);
    let outcome = if mutation_accepted {
        CommandOutcome::Success {
            value: json!({"identity":"host-issued-child"}),
            warnings: vec![CommandWarning {
                code: "child_tools_revoked".to_owned(),
                message: "Created child; its inherited tools are unavailable".to_owned(),
            }],
        }
    } else {
        CommandOutcome::cancelled()
    };
    queued.response.send(outcome).unwrap();
    let outcome = response.try_recv().unwrap();
    if accepted {
        let CommandOutcome::Success { value, warnings } = outcome else {
            panic!("Accepted child IDs were hidden")
        };
        assert_eq!(value["identity"], "host-issued-child");
        assert_eq!(warnings.len(), 1);
    } else {
        assert!(!matches!(outcome, CommandOutcome::Success { .. }));
    }
    drop(guard);
}

#[rstest]
fn dropping_a_pending_spawn_guard_cancels_the_unaccepted_queue_request(binding: CommandTarget) {
    let parent = spawn_lease(binding.clone(), spawning_policy()).unwrap();
    parent.bind(&binding, parent_terminal()).unwrap();
    let request = ToolSpawnRequest::Shell {
        name: "child".to_owned(),
        title: None,
    };
    let guard = parent.begin_spawn(&request).unwrap();
    let pending = guard.cancellation();
    drop(guard);
    assert!(pending.is_cancelled());
    assert!(!pending.try_start());
    // Completing/dropping pending requests frees the bounded catalog for a subsequent request.
    let guards = (0..8)
        .map(|_| parent.begin_spawn(&request).unwrap())
        .collect::<Vec<_>>();
    assert!(parent.begin_spawn(&request).is_err());
    drop(guards);
    assert!(parent.begin_spawn(&request).is_ok());
}
