use std::{
    fs,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bootty_agents::{
    AgentCommandExecutor, AgentEventKind, AgentEventPublisher, AgentIntegration, AgentInvocation,
    AgentKind, AgentPaneResolver, AgentService, AgentSource, AgentStatus, IntegrationStatus,
    install_integration, integration_status, uninstall_integration,
};
use bootty_control::{
    Caller, CommandCancellation, CommandCatalogSource, CommandInvocation, CommandOutcome,
    CommandTarget, ResourceKind,
};
use proptest::prelude::*;
use serde_json::{Value, json};

#[derive(Default)]
struct Events(Mutex<Vec<(String, String, Value)>>);

struct ScopeResolver;

impl AgentPaneResolver for ScopeResolver {
    fn scope_for_pane(&self, _pane: &str) -> Option<String> {
        Some("space-a".to_owned())
    }
}

impl AgentEventPublisher for Events {
    fn publish(
        &self,
        identity: &str,
        _generation: u64,
        topic: &str,
        payload: Value,
        _deadline: Instant,
        _cancellation: &CommandCancellation,
    ) -> Result<(), String> {
        self.0.lock().map_err(|error| error.to_string())?.push((
            identity.to_owned(),
            topic.to_owned(),
            payload,
        ));
        Ok(())
    }
}

fn service(calls: Arc<Mutex<Vec<CommandInvocation>>>, event_sink: Arc<Events>) -> AgentService {
    let executor: Arc<dyn AgentCommandExecutor> = Arc::new(
        move |invocation: CommandInvocation, _deadline, _cancellation| {
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(invocation.clone());
            match invocation.command.as_str() {
                "new_tab" => CommandOutcome::Success {
                    value: json!({
                        "created": {
                            "kind": "terminal",
                            "handle": "new-pane",
                            "generation": "1"
                        }
                    }),
                    warnings: Vec::new(),
                },
                _ => CommandOutcome::success(),
            }
        },
    );
    AgentService::new(executor, event_sink)
}

fn request(
    command: &str,
    arguments: Vec<String>,
    target: Option<CommandTarget>,
    target_supplied: bool,
) -> AgentInvocation {
    AgentInvocation {
        invocation: CommandInvocation {
            command: command.to_owned(),
            arguments,
            caller: Caller::CommandPalette,
            target,
            confirmation: None,
        },
        target_supplied,
        scope: None,
        launch_context: bootty_agents::AgentLaunchContext::default(),
        deadline: Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        cancellation: CommandCancellation::new(),
    }
}

fn target(kind: ResourceKind, handle: &str) -> CommandTarget {
    CommandTarget {
        kind,
        handle: handle.to_owned(),
        generation: 1,
    }
}

#[test]
fn descriptors_cover_all_provider_commands() {
    let ids = bootty_agents::command_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        ids.len()
    );
    for provider in ["pi", "codex", "claude"] {
        for operation in ["resume", "fork"] {
            assert!(ids.contains(&format!("agents.{provider}.{operation}")));
        }
        assert!(
            ids.iter()
                .any(|id| id == &format!("agents.{provider}.start"))
        );
        assert!(
            ids.iter()
                .any(|id| id == &format!("agents.{provider}.ingest"))
        );
    }
    assert!(ids.contains(&"agents.codex.interrupt".to_owned()));
    assert!(ids.contains(&"agents.pi.abort".to_owned()));
    assert!(ids.contains(&"agents.claude.abort".to_owned()));
    assert!(!ids.contains(&"agents.claude.stop".to_owned()));
}

proptest! {
    #[test]
    fn advertised_argument_bounds_are_enforced_before_provider_effects(
        descriptor in prop::sample::select(bootty_agents::command_descriptors()),
        extra in 1usize..8,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Events::default());
        let service = service(calls.clone(), events.clone());
        let schema = &descriptor.arguments.arguments;
        let mut lengths = vec![schema.len().saturating_add(extra)];
        lengths.extend(schema.iter().position(|argument| argument.required));
        for length in lengths {
            let outcome = service.invoke(&request(
                &descriptor.id,
                vec!["argument".to_owned(); length],
                None,
                false,
            ));
            prop_assert!(matches!(outcome, CommandOutcome::Failed { ref code, .. } if code == "invalid_arguments"), "{}: {:?}", descriptor.id, outcome);
        }
        pretty_assertions::assert_eq!(calls.lock().unwrap().len(), 0);
        pretty_assertions::assert_eq!(events.0.lock().unwrap().len(), 0);
    }
}

#[rstest::rstest]
#[case("agents.claude.stop")]
#[case("agents.codex.abort")]
#[case("agents.codex.follow_up")]
#[case("agents.pi.interrupt")]
fn unadvertised_provider_operations_cannot_execute(#[case] command: &str) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(calls.clone(), Arc::new(Events::default()));
    let outcome = service.invoke(&request(command, Vec::new(), None, false));
    assert!(matches!(outcome, CommandOutcome::Failed { code, .. } if code == "unknown_command"));
    pretty_assertions::assert_eq!(calls.lock().unwrap().len(), 0);
}

#[test]
fn provider_events_keep_typed_per_pane_state_and_raw_topics() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Events::default());
    let service = service(calls, events.clone());
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let cancellation = CommandCancellation::new();

    let outcome = service.ingest(
        AgentKind::Pi,
        Some("%pi"),
        json!({"type": "tool_execution_start", "toolName": "bash", "sessionId": "s1"}),
        deadline,
        &cancellation,
    );
    assert!(matches!(outcome, CommandOutcome::Success { .. }));
    let state = service.snapshot(AgentKind::Pi, Some("%pi"));
    assert_eq!(state.source, AgentSource::Existing);
    assert_eq!(state.status, AgentStatus::Tool("bash".to_owned()));
    assert_eq!(state.session_id.as_deref(), Some("s1"));
    assert_eq!(state.last_event.as_deref(), Some("tool_execution_start"));

    let codex = service.ingest(
        AgentKind::Codex,
        Some("%codex"),
        json!({"hook_event_name": "SessionStart", "session_id": "thread-1"}),
        deadline,
        &cancellation,
    );
    assert!(matches!(codex, CommandOutcome::Success { .. }));
    assert_eq!(
        service
            .snapshot(AgentKind::Codex, Some("%codex"))
            .thread_id
            .as_deref(),
        Some("thread-1")
    );

    let claude = service.ingest(
        AgentKind::Claude,
        Some("%claude"),
        json!({"hook_event_name": "Notification", "session_id": "session-1"}),
        deadline,
        &cancellation,
    );
    assert!(matches!(claude, CommandOutcome::Success { .. }));
    assert_eq!(
        service.snapshot(AgentKind::Claude, None).status,
        AgentStatus::Waiting
    );

    let published = events.0.lock().expect("published events lock");
    assert_eq!(published.len(), 3);
    assert_eq!(published[0].0, "agents.pi");
    assert_eq!(published[0].1, "agents.pi.event");
    assert_eq!(published[0].2["kind"], "native");
    assert_eq!(published[1].2["kind"], "hook");
    assert_eq!(published[2].2["payload"]["hook_event_name"], "Notification");
    assert_eq!(AgentEventKind::Native.as_str(), "native");
}

#[test]
fn pane_state_uses_host_scope_to_disambiguate_reused_backend_labels() {
    let events = Arc::new(Events::default());
    let executor: Arc<dyn AgentCommandExecutor> =
        Arc::new(|_invocation: CommandInvocation, _deadline, _cancellation| {
            CommandOutcome::success()
        });
    let service = AgentService::new_with_resolver(executor, events, Arc::new(ScopeResolver));
    let outcome = service.ingest(
        AgentKind::Pi,
        Some("%1"),
        json!({"type": "agent_start"}),
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        &CommandCancellation::new(),
    );
    assert!(matches!(outcome, CommandOutcome::Success { .. }));
    let states = service.pane_states(AgentKind::Pi);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0.scope, "space-a");
    assert_eq!(states[0].0.pane, "%1");
    assert_eq!(
        service
            .snapshot_scoped(AgentKind::Pi, Some("space-b"), Some("%1"))
            .source,
        AgentSource::None
    );
}

#[test]
fn command_forwarding_captures_targets_and_destructive_confirmation() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(calls.clone(), Arc::new(Events::default()));
    let pane = target(ResourceKind::Terminal, "%1");
    let start = service.invoke(&request(
        "agents.pi.start",
        vec!["/tmp/project".to_owned(), "pi-dev".to_owned()],
        None,
        false,
    ));
    assert!(matches!(start, CommandOutcome::Success { .. }));
    let calls_after_start = calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(calls_after_start[0].command, "new_tab");
    assert_eq!(calls_after_start[1].command, "terminal.paste");
    assert_eq!(
        calls_after_start[1]
            .target
            .as_ref()
            .map(|target| target.handle.as_str()),
        Some("new-pane")
    );
    assert!(
        calls_after_start[1].arguments[0]
            .starts_with("cd '/tmp/project' && exec env BOOTTY_AGENT_LAUNCH_CONTEXT=")
    );
    assert!(calls_after_start[1].arguments[0].ends_with(" 'pi-dev'"));
    drop(calls_after_start);

    let prompt = service.invoke(&request(
        "agents.pi.prompt",
        vec!["hello".to_owned()],
        Some(pane.clone()),
        true,
    ));
    assert!(matches!(prompt, CommandOutcome::Success { .. }));
    let implicit_start = calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    let implicit = service.invoke(&request(
        "agents.pi.prompt",
        vec!["captured".to_owned()],
        Some(pane.clone()),
        false,
    ));
    assert!(matches!(implicit, CommandOutcome::Success { .. }));
    let calls_after_implicit = calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        calls_after_implicit[implicit_start].command,
        "terminal.paste"
    );
    assert_eq!(
        calls_after_implicit[implicit_start].target,
        Some(pane.clone())
    );
    assert_eq!(
        calls_after_implicit[implicit_start + 1].command,
        "terminal.submit"
    );
    assert_eq!(calls_after_implicit[implicit_start + 1].target, Some(pane));
    drop(calls_after_implicit);
    let stop = service.invoke(&request(
        "agents.pi.stop",
        Vec::new(),
        Some(target(ResourceKind::Pane, "%1")),
        true,
    ));
    assert!(matches!(stop, CommandOutcome::Success { .. }));
    let calls = calls
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(calls.last().expect("kill call").command, "kill_pane");
    assert!(calls.last().expect("kill call").confirmation.is_some());
    assert_eq!(calls.last().expect("kill call").caller, Caller::Internal);
}

#[test]
fn incarnation_cancellation_prevents_stale_publication() {
    let service = service(
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Events::default()),
    );
    service.retire();
    let outcome = service.ingest(
        AgentKind::Pi,
        Some("%stale"),
        json!({"type": "agent_start"}),
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        &CommandCancellation::new(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Failed { ref code, .. } if code == "stale_agent_incarnation")
    );
    assert_eq!(
        service.pane_states(AgentKind::Pi),
        Vec::<(bootty_agents::AgentPaneKey, bootty_agents::AgentState)>::new()
    );
    let mut published = false;
    assert!(
        service
            .with_active_topic(
                AgentKind::Pi.module(),
                service.generation(),
                AgentKind::Pi.topic(),
                &mut || published = true,
            )
            .is_err()
    );
    assert!(!published);
}

#[test]
fn integrations_install_atomically_and_preserve_user_edits_on_uninstall() {
    let root = assert_fs::TempDir::new().unwrap();
    let home = root.path().join("home");
    let integration_dir = root.path().join("integration");
    fs::create_dir_all(&home).expect("home");
    let declaration =
        AgentIntegration::for_provider(AgentKind::Codex, &integration_dir).declaration;
    let pi_declaration =
        AgentIntegration::for_provider(AgentKind::Pi, &integration_dir).declaration;
    assert!(install_integration(&integration_dir, None, &pi_declaration).is_err());
    assert!(!integration_dir.exists());

    install_integration(&integration_dir, Some(&home), &declaration).expect("install");
    assert_eq!(
        integration_status(&integration_dir, Some(&home), &declaration),
        IntegrationStatus::Installed
    );
    let hook = integration_dir.join("codex/bootty-hook.sh");
    fs::write(&hook, "user-owned hook\n").expect("user hook edit");
    let settings = home.join(".codex/hooks.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&settings).expect("settings"))
        .expect("valid settings");
    config["user_setting"] = json!(true);
    fs::write(&settings, serde_json::to_vec_pretty(&config).expect("json"))
        .expect("user config edit");

    uninstall_integration(&integration_dir, Some(&home), &declaration).expect("uninstall");
    assert_eq!(
        fs::read_to_string(&hook).expect("preserved hook"),
        "user-owned hook\n"
    );
    let config: Value = serde_json::from_str(&fs::read_to_string(&settings).expect("settings"))
        .expect("valid settings");
    assert_eq!(config["user_setting"], true);
    assert!(config.get("hooks").is_none());
}

#[rstest::rstest]
fn repeated_merge_targets_preserve_updates_committed_after_preflight() {
    let root = assert_fs::TempDir::new().unwrap();
    let integration_dir = root.path().join("integration");
    let settings = root.path().join("settings.json");
    fs::write(&settings, r#"{"user_setting":true}"#).unwrap();
    let mut declaration =
        AgentIntegration::for_provider(AgentKind::Codex, &integration_dir).declaration;
    declaration.files.clear();
    declaration.merge = [json!({"first": 1}), json!({"second": 2})]
        .into_iter()
        .map(|value| bootty_agents::IntegrationMerge {
            path: settings.to_string_lossy().into_owned(),
            value,
        })
        .collect();

    install_integration(&integration_dir, Some(root.path()), &declaration).unwrap();
    let installed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    pretty_assertions::assert_eq!(
        installed,
        json!({"user_setting": true, "first": 1, "second": 2})
    );
    uninstall_integration(&integration_dir, Some(root.path()), &declaration).unwrap();
    let removed: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
    pretty_assertions::assert_eq!(removed, json!({"user_setting": true}));
}

#[cfg(unix)]
#[rstest::rstest]
fn integration_json_merges_preserve_symlink_targets() {
    let root = assert_fs::TempDir::new().unwrap();
    let integration_dir = root.path().join("integration");
    let settings = root.path().join("settings.json");
    let alias = root.path().join("settings-link.json");
    fs::write(&settings, r#"{"user_setting":true}"#).unwrap();
    std::os::unix::fs::symlink(&settings, &alias).unwrap();
    let mut declaration =
        AgentIntegration::for_provider(AgentKind::Codex, &integration_dir).declaration;
    declaration.files.clear();
    declaration.merge[0].path = alias.to_string_lossy().into_owned();

    install_integration(&integration_dir, Some(root.path()), &declaration).unwrap();
    assert!(
        fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let installed: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
    assert!(installed.get("hooks").is_some());
    uninstall_integration(&integration_dir, Some(root.path()), &declaration).unwrap();
    assert!(
        fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let removed: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
    pretty_assertions::assert_eq!(removed, json!({"user_setting": true}));
}

/// The hook passes the server its pane belongs to last: tmux's `$TMUX`, else the `$RMUX` Bootty's
/// rmux panes set while leaving `$TMUX` empty, else nothing.
#[cfg(unix)]
#[rstest::rstest]
#[case::tmux(
    AgentKind::Codex,
    (Some("/private/tmp/tmux-501/default,41,0"), None, "/private/tmp/tmux-501/default,41,0")
)]
#[case::rmux(
    AgentKind::Claude,
    (Some(""), Some("/tmp/rmux/bootty-wire3,42,1"), "/tmp/rmux/bootty-wire3,42,1")
)]
#[case::native(AgentKind::Claude, (None, None, ""))]
fn installed_hooks_execute_from_paths_with_spaces_and_shell_metacharacters(
    #[case] provider: AgentKind,
    #[case] server: (Option<&str>, Option<&str>, &str),
) {
    use std::os::unix::fs::PermissionsExt as _;

    let root = assert_fs::TempDir::new().unwrap();
    let integration_dir = root.path().join("Luan's $(invalid command) integrations");
    let declaration = AgentIntegration::for_provider(provider, &integration_dir).declaration;
    install_integration(&integration_dir, Some(root.path()), &declaration).unwrap();
    let fake = root.path().join("bootty");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$BOOTTY_HOOK_ARGUMENTS\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let recorded = root.path().join("arguments");
    let command = declaration.merge[0].value["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let (tmux, rmux, expected_server) = server;
    let mut hook = std::process::Command::new("/bin/sh");
    hook.args(["-c", command])
        .env("PATH", format!("{}:/usr/bin:/bin", root.path().display()))
        .env("BOOTTY_HOOK_ARGUMENTS", &recorded)
        .env_remove("TMUX")
        .env_remove("RMUX")
        .stdin(std::process::Stdio::null());
    for (name, value) in [("TMUX", tmux), ("RMUX", rmux)] {
        if let Some(value) = value {
            hook.env(name, value);
        }
    }
    let output = hook.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"{}\n");
    let arguments = fs::read_to_string(recorded).unwrap();
    assert!(arguments.contains(&format!("agents.{provider}.ingest")));
    assert_eq!(arguments.lines().last(), Some(expected_server));
}

#[rstest::rstest]
#[case(AgentKind::Pi)]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
fn provider_state_capacity_rejects_new_panes_and_recovers_after_shutdown(
    #[case] provider: AgentKind,
) {
    let service = service(
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Events::default()),
    );
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .unwrap_or_else(Instant::now);
    let cancellation = CommandCancellation::new();
    let start = json!({"type":"session_start", "hook_event_name":"SessionStart"});
    for index in 0..1024 {
        assert!(matches!(
            service.ingest(
                provider,
                Some(&index.to_string()),
                start.clone(),
                deadline,
                &cancellation
            ),
            CommandOutcome::Success { .. }
        ));
    }
    assert!(
        matches!(service.ingest(provider, Some("overflow"), start.clone(), deadline, &cancellation), CommandOutcome::Failed { code, .. } if code == "state_limit")
    );
    assert!(matches!(
        service.ingest(
            provider,
            Some("0"),
            json!({"type":"session_shutdown", "hook_event_name":"SessionEnd"}),
            deadline,
            &cancellation
        ),
        CommandOutcome::Success { .. }
    ));
    assert!(matches!(
        service.ingest(provider, Some("overflow"), start, deadline, &cancellation),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(service.pane_states(provider).len(), 1024);
}

#[rstest::rstest]
#[case("pi", "--fork")]
#[case("codex", "fork")]
#[case("claude", "--fork-session")]
fn fork_uses_reported_session_and_captured_parent_tab_without_touching_source(
    #[case] provider: &str,
    #[case] flag: &str,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(calls.clone(), Arc::new(Events::default()));
    let kind = match provider {
        "pi" => AgentKind::Pi,
        "codex" => AgentKind::Codex,
        _ => AgentKind::Claude,
    };
    let event = json!({"type":"session_start", "hook_event_name":"SessionStart", "session_id":"session-123", "cwd":"/work/project", "bootty_launch":{"program":"custom-agent", "cwd":"/work/project", "arguments":["--model","model-name"]}});
    let outcome = service.ingest_scoped(
        kind,
        Some("space-a"),
        Some("%1"),
        event,
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        &CommandCancellation::new(),
    );
    assert!(matches!(outcome, CommandOutcome::Success { .. }));
    let mut invocation = request(
        &format!("agents.{provider}.fork"),
        vec![],
        Some(target(ResourceKind::Terminal, "source")),
        true,
    );
    invocation.scope = Some("space-a".to_owned());
    invocation.launch_context.pane = Some("%1".to_owned());
    invocation.launch_context.new_tab = Some(target(ResourceKind::Session, "captured-parent"));
    let outcome = service.invoke(&invocation);
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls[0].command, "new_tab");
    assert_eq!(calls[0].target, invocation.launch_context.new_tab);
    assert_eq!(
        calls[1].target,
        Some(target(ResourceKind::Terminal, "new-pane"))
    );
    assert!(calls[1].arguments[0].contains("'custom-agent'"));
    assert!(calls[1].arguments[0].contains("'session-123'"));
    assert!(calls[1].arguments[0].contains(&format!("'{flag}'")));
    assert!(calls[1].arguments[0].starts_with("cd '/work/project'"));
    drop(calls);
}

#[rstest::rstest]
fn resume_without_an_explicit_or_target_reported_session_creates_nothing() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let service = service(calls.clone(), Arc::new(Events::default()));
    let outcome = service.invoke(&request("agents.codex.resume", vec![], None, false));
    assert!(matches!(outcome, CommandOutcome::Failed { .. }));
    assert!(calls.lock().unwrap().is_empty());
}

#[rstest::rstest]
#[case(AgentKind::Pi)]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
fn cancellation_during_scope_resolution_leaves_no_agent_state(#[case] provider: AgentKind) {
    let cancellation = CommandCancellation::new();
    let cancel = cancellation.clone();
    let resolver: Arc<dyn AgentPaneResolver> = Arc::new(move |_: &str| {
        let _ = cancel.cancel();
        Some("space".to_owned())
    });
    let events = Arc::new(Events::default());
    let service = AgentService::new_with_resolver(
        Arc::new(|_: CommandInvocation, _, _| CommandOutcome::success()),
        events.clone(),
        resolver,
    );
    let outcome = service.ingest(
        provider,
        Some("pane"),
        json!({"type": "agent_start", "hook_event_name": "SessionStart"}),
        Instant::now().checked_add(Duration::from_secs(60)).unwrap(),
        &cancellation,
    );
    pretty_assertions::assert_eq!(outcome, CommandOutcome::cancelled());
    pretty_assertions::assert_eq!(service.pane_states(provider), Vec::new());
    assert!(events.0.lock().unwrap().is_empty());
}

#[derive(Default)]
struct CancellingSubscriber(Mutex<Vec<bool>>);

impl AgentEventPublisher for CancellingSubscriber {
    fn publish(
        &self,
        _identity: &str,
        _generation: u64,
        _topic: &str,
        _payload: Value,
        _deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|error| error.to_string())?
            .push(cancellation.cancel());
        Err("subscriber rejected the event".to_owned())
    }
}

#[rstest::rstest]
#[case(AgentKind::Pi)]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
fn committed_agent_state_survives_subscriber_failure_and_late_cancellation(
    #[case] provider: AgentKind,
    #[values(false, true)] already_started: bool,
) {
    let cancellation = CommandCancellation::new();
    if already_started {
        assert!(cancellation.try_start());
    }
    let events = Arc::new(CancellingSubscriber::default());
    let service = AgentService::new(
        Arc::new(|_: CommandInvocation, _, _| CommandOutcome::success()),
        events.clone(),
    );
    let outcome = service.ingest(
        provider,
        Some("pane"),
        json!({"type": "agent_start", "hook_event_name": "SessionStart"}),
        Instant::now().checked_add(Duration::from_secs(60)).unwrap(),
        &cancellation,
    );
    let CommandOutcome::Success { value, warnings } = outcome else {
        panic!("state was committed: {outcome:?}");
    };
    pretty_assertions::assert_eq!(value, service.snapshot(provider, Some("pane")).to_value());
    pretty_assertions::assert_eq!(warnings.len(), 1);
    pretty_assertions::assert_eq!(warnings.first().unwrap().code, "event_publish_failed");
    pretty_assertions::assert_eq!(*events.0.lock().unwrap(), vec![false]);
    assert!(!cancellation.is_cancelled());
}

fn claude_after(events: &[Value]) -> bootty_agents::AgentState {
    let service = service(
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Events::default()),
    );
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    for event in events {
        let outcome = service.ingest(
            AgentKind::Claude,
            Some("%claude"),
            event.clone(),
            deadline,
            &CommandCancellation::new(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    service.snapshot(AgentKind::Claude, Some("%claude"))
}

#[rstest::rstest]
#[case::permission("permission_prompt", AgentStatus::Waiting)]
#[case::question("elicitation_dialog", AgentStatus::Waiting)]
#[case::idle("idle_prompt", AgentStatus::Idle)]
#[case::informational("auth_success", AgentStatus::Working)]
fn claude_waits_only_on_prompts_the_user_must_answer(
    #[case] notification_type: &str,
    #[case] expected: AgentStatus,
) {
    let state = claude_after(&[
        json!({"hook_event_name": "UserPromptSubmit"}),
        json!({"hook_event_name": "Notification", "notification_type": notification_type}),
    ]);
    pretty_assertions::assert_eq!(state.status, expected);
}

#[rstest::rstest]
#[case::short("Done: fixed the test.".to_owned(), "Done: fixed the test.".len())]
// Multi-byte text past the retained limit keeps a whole-character prefix.
#[case::oversized("é".repeat(40 * 1024), 64 * 1024)]
fn claude_stop_reports_the_finished_turn(#[case] message: String, #[case] retained: usize) {
    let transcript = "/Users/me/.claude/projects/p/session.jsonl";
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis();
    let state = claude_after(&[
        json!({"hook_event_name": "UserPromptSubmit", "transcript_path": transcript}),
        json!({"hook_event_name": "Stop", "last_assistant_message": message}),
    ]);
    pretty_assertions::assert_eq!(state.status, AgentStatus::Idle);
    pretty_assertions::assert_eq!(state.session_file.as_deref(), Some(transcript));
    pretty_assertions::assert_eq!(state.last_message.as_deref(), message.get(..retained));
    let ended = state.turn_ended_at.map(u128::from);
    assert!(
        ended.is_some_and(|ended| ended >= before),
        "turn end {ended:?} is missing or precedes the test start {before}"
    );
}

/// The Space that owns one pane, which a test moves.
struct MovableOwner(Mutex<&'static str>);

impl AgentPaneResolver for MovableOwner {
    fn scope_for_pane(&self, _pane: &str) -> Option<String> {
        self.0.lock().ok().map(|owner| (*owner).to_owned())
    }
}

/// A hook that arrives after its pane moved to another Space, before anything else noticed the
/// move, carries the pane's record there: one record, with the final message it already had.
#[test]
fn a_hook_after_its_pane_moved_takes_its_record_along() {
    let executor: Arc<dyn AgentCommandExecutor> =
        Arc::new(|_invocation: CommandInvocation, _deadline, _cancellation| {
            CommandOutcome::success()
        });
    let owner = Arc::new(MovableOwner(Mutex::new("space-a")));
    let resolver: Arc<dyn AgentPaneResolver> = owner.clone();
    let service = AgentService::new_with_resolver(executor, Arc::new(Events::default()), resolver);
    let report = |event: Value| {
        let outcome = service.ingest(
            AgentKind::Claude,
            Some("%7"),
            event,
            Instant::now()
                .checked_add(Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
            &CommandCancellation::new(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    };
    report(json!({"hook_event_name": "Stop", "last_assistant_message": "Done."}));
    *owner.0.lock().expect("owner") = "space-b";
    report(json!({"hook_event_name": "UserPromptSubmit"}));

    let records = service
        .pane_states(AgentKind::Claude)
        .into_iter()
        .map(|(key, state)| {
            (
                key.scope,
                state.status.as_str(),
                state.last_message.as_deref().map(str::to_owned),
            )
        })
        .collect::<Vec<_>>();
    pretty_assertions::assert_eq!(
        records,
        [(
            "space-b".to_owned(),
            "working".to_owned(),
            Some("Done.".to_owned())
        )]
    );
}

/// A state file that could not be written is retried, and a shutdown flush waits for a fresh try
/// rather than reporting the earlier failure once the disk accepts it again.
#[test]
fn a_shutdown_flush_retries_a_failed_write() {
    let directory = assert_fs::TempDir::new().expect("state directory");
    let blocked = directory.path().join("state");
    fs::write(&blocked, b"").expect("a file where the state directory belongs");
    let executor: Arc<dyn AgentCommandExecutor> =
        Arc::new(|_invocation: CommandInvocation, _deadline, _cancellation| {
            CommandOutcome::success()
        });
    let service = AgentService::new_with_resolver(
        executor,
        Arc::new(Events::default()),
        Arc::new(ScopeResolver),
    )
    .persisted_at(&blocked.join("agent-state.json"));
    let outcome = service.ingest(
        AgentKind::Claude,
        Some("%7"),
        json!({"hook_event_name": "Stop", "last_assistant_message": "Kept."}),
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        &CommandCancellation::new(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );

    let failed = bootty_agents::flush_agent_state(Duration::from_secs(5));
    assert!(failed.is_err(), "{failed:?}");
    fs::remove_file(&blocked).expect("free the state directory");
    bootty_agents::flush_agent_state(Duration::from_secs(5)).expect("the retry reaches disk");
    let saved = fs::read_to_string(blocked.join("agent-state.json")).expect("saved state");
    assert!(saved.contains("Kept."), "{saved}");
}

#[test]
fn reported_state_survives_a_restart_until_the_pane_reports_again() {
    let directory = assert_fs::TempDir::new().expect("state directory");
    let path = directory.path().join("agent-state.json");
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let persisted = || {
        let executor: Arc<dyn AgentCommandExecutor> =
            Arc::new(|_invocation: CommandInvocation, _deadline, _cancellation| {
                CommandOutcome::success()
            });
        AgentService::new_with_resolver(
            executor,
            Arc::new(Events::default()),
            Arc::new(ScopeResolver),
        )
        .persisted_at(&path)
    };
    let report = |service: &AgentService, event: Value| {
        let outcome = service.ingest(
            AgentKind::Claude,
            Some("%7"),
            event,
            deadline,
            &CommandCancellation::new(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    };
    // A file others could read is tightened once state is written to it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::write(&path, b"{}").expect("existing state file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("public mode");
    }
    let first = persisted();
    report(
        &first,
        json!({"hook_event_name": "UserPromptSubmit", "transcript_path": "/t/s.jsonl"}),
    );
    report(
        &first,
        json!({"hook_event_name": "Stop", "last_assistant_message": "Fixed it."}),
    );
    // A window reopened before the old one retires restores the newest state, flushed or not.
    let reopened = persisted();
    pretty_assertions::assert_eq!(
        reopened
            .snapshot(AgentKind::Claude, Some("%7"))
            .last_message
            .as_deref(),
        Some("Fixed it.")
    );
    // Its newer state is what reaches disk, whenever the old window lets go.
    report(
        &reopened,
        json!({"hook_event_name": "Stop", "last_assistant_message": "Checked again."}),
    );
    drop(first);
    drop(reopened);
    bootty_agents::flush_agent_state(Duration::from_secs(5)).expect("state reaches disk");
    let saved = fs::read_to_string(&path).expect("saved state");
    assert!(
        saved.contains("Checked again.") && !saved.contains("Fixed it."),
        "the older window's state overwrote the newer one: {saved}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&path)
            .expect("saved state")
            .permissions()
            .mode();
        pretty_assertions::assert_eq!(mode & 0o077, 0, "agent state must be private");
    }

    let restarted = persisted();
    let restored = restarted.snapshot(AgentKind::Claude, Some("%7"));
    pretty_assertions::assert_eq!(restored.source, AgentSource::Restored);
    pretty_assertions::assert_eq!(restored.status, AgentStatus::Idle);
    pretty_assertions::assert_eq!(restored.last_message.as_deref(), Some("Checked again."));
    pretty_assertions::assert_eq!(restored.session_file.as_deref(), Some("/t/s.jsonl"));
    assert!(restored.turn_ended_at.is_some(), "{restored:?}");

    report(&restarted, json!({"hook_event_name": "UserPromptSubmit"}));
    let current = restarted.snapshot(AgentKind::Claude, Some("%7"));
    pretty_assertions::assert_eq!(current.source, AgentSource::Existing);
    pretty_assertions::assert_eq!(current.last_message.as_deref(), Some("Checked again."));
}

#[test]
fn only_panes_a_complete_listing_proves_gone_are_forgotten() {
    let directory = assert_fs::TempDir::new().expect("state directory");
    let path = directory.path().join("agent-state.json");
    let executor: Arc<dyn AgentCommandExecutor> =
        Arc::new(|_invocation: CommandInvocation, _deadline, _cancellation| {
            CommandOutcome::success()
        });
    let service = AgentService::new_with_resolver(
        executor,
        Arc::new(Events::default()),
        Arc::new(ScopeResolver),
    )
    .persisted_at(&path);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    for pane in ["%1", "%2"] {
        let outcome = service.ingest(
            AgentKind::Claude,
            Some(pane),
            json!({"hook_event_name": "SessionStart"}),
            deadline,
            &CommandCancellation::new(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    let open = std::collections::BTreeSet::from(["space-a".to_owned()]);
    // A live Space without a complete listing proves nothing.
    service.retain_live_panes(&open, &std::collections::BTreeMap::new());
    pretty_assertions::assert_eq!(service.pane_states(AgentKind::Claude).len(), 2);

    service.retain_live_panes(
        &open,
        &std::collections::BTreeMap::from([(
            "space-a".to_owned(),
            std::collections::BTreeSet::from(["%1".to_owned()]),
        )]),
    );
    let panes = |service: &AgentService| {
        service
            .pane_states(AgentKind::Claude)
            .into_iter()
            .map(|(key, _)| key.pane)
            .collect::<Vec<_>>()
    };
    pretty_assertions::assert_eq!(panes(&service), vec!["%1".to_owned()]);
    drop(service);
    let executor: Arc<dyn AgentCommandExecutor> =
        Arc::new(|_invocation: CommandInvocation, _deadline, _cancellation| {
            CommandOutcome::success()
        });
    let restarted = AgentService::new_with_resolver(
        executor,
        Arc::new(Events::default()),
        Arc::new(ScopeResolver),
    )
    .persisted_at(&path);
    pretty_assertions::assert_eq!(panes(&restarted), vec!["%1".to_owned()]);
    // Closing the Space retires its records, listing or not.
    restarted.retain_live_panes(
        &std::collections::BTreeSet::new(),
        &std::collections::BTreeMap::new(),
    );
    pretty_assertions::assert_eq!(panes(&restarted), Vec::<String>::new());
}

#[test]
fn a_stop_counts_as_a_finished_turn_even_when_its_start_was_missed() {
    let state =
        claude_after(&[json!({"hook_event_name": "Stop", "last_assistant_message": "Done."})]);
    assert!(state.turn_ended_at.is_some(), "{state:?}");
}
