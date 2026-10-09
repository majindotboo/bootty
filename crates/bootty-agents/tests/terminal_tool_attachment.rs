#![cfg(unix)]

use std::{
    fs,
    sync::{Arc, Mutex},
    time::Instant,
};

use bootty_agents::{
    AgentCommandExecutor, AgentKind, AgentLaunch, TerminalAgentService, ToolBridge,
    ToolBridgeContext, ToolCapture, ToolCapturedCommand, ToolLease, ToolPolicy, ToolProtocol,
    ToolScope, ToolSpawnContext, ToolSpawnRequest,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};
use serde_json::{Value, json};

#[derive(Default)]
struct Commands(Mutex<Vec<CommandInvocation>>);
impl AgentCommandExecutor for Commands {
    fn execute(
        &self,
        invocation: CommandInvocation,
        _: Instant,
        _: CommandCancellation,
    ) -> CommandOutcome {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(invocation);
        CommandOutcome::Success {
            value: snapshot(),
            warnings: Vec::new(),
        }
    }
}

fn target(kind: ResourceKind, generation: u64) -> CommandTarget {
    CommandTarget {
        kind,
        handle: format!("issued-{kind:?}"),
        generation,
    }
}

#[fixture]
fn launch() -> AgentLaunch {
    AgentLaunch {
        program: "codex".to_owned(),
        cwd: Some("/project".to_owned()),
        arguments: vec![
            "--model".to_owned(),
            "selected".to_owned(),
            "--".to_owned(),
            "literal prompt".to_owned(),
        ],
        ephemeral: false,
        account_directory: None,
    }
}

fn attachment(commands: &Arc<Commands>) -> Result<(ToolBridge, ToolLease), String> {
    let bridge = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: target(ResourceKind::Binding, 19),
            },
            caller: Caller::Socket,
            policy: ToolPolicy::own_terminal(),
            captures: Vec::new(),
            spawn: None,
        },
        &std::env::current_exe().map_err(|error| error.to_string())?,
        commands.clone(),
    )?;
    let lease = bridge.lease().clone();
    Ok((bridge, lease))
}

#[rstest]
fn retained_browser_grants_survive_unrelated_settings_and_end_with_provider_disable() {
    let directory = assert_fs::TempDir::new().unwrap();
    let service = TerminalAgentService::open(directory.path().join("agents.json")).unwrap();
    let binding = target(ResourceKind::Binding, 19);
    let commands = Arc::new(Commands::default());
    let tools = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: binding.clone(),
            },
            caller: Caller::Socket,
            policy: ToolPolicy {
                browser_capture: true,
                ..ToolPolicy::own_terminal()
            },
            captures: Vec::new(),
            spawn: None,
        },
        &std::env::current_exe().unwrap(),
        commands,
    )
    .unwrap();
    let tools = service
        .retain_tool_attachment(AgentKind::Codex, tools)
        .unwrap();
    let session = target(ResourceKind::Session, 23);
    tools
        .lease()
        .bind(&binding, target(ResourceKind::Terminal, 23))
        .unwrap();
    tools.lease().bind_native_session(session.clone()).unwrap();
    assert!(tools.lease().browser_attachments_supported());
    let page = bootty_agents::NativeBrowserAttachment {
        window: target(ResourceKind::ApplicationWindow, 29),
        page: 17,
        document: "1234567890abcdef1234567890abcdef".into(),
    };
    tools
        .lease()
        .attach_browser(&session, Some(page.clone()))
        .unwrap();
    service.set_computer_capture_enabled(false);
    service.set_agent_spawning_enabled(false);
    assert_eq!(tools.lease().browser_attachment(), Some(page));
    service.set_provider_tools_enabled(AgentKind::Codex, false);
    assert_eq!(
        tools.lease().browser_access(),
        bootty_agents::NativeBrowserAccess::Unavailable
    );
}

fn read(lease: &ToolLease, commands: &Commands) -> Result<Value, String> {
    ToolProtocol::new(lease.clone())
        .handle(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"terminal_read"}}"#,
            Instant::now(),
            commands,
        )
        .ok_or_else(|| "Expected tool reply".to_owned())
}

fn capture_attachment(commands: &Arc<Commands>) -> Result<(ToolBridge, ToolLease), String> {
    let bridge = ToolBridge::prepare(
        ToolBridgeContext {
            scope: ToolScope {
                provider: AgentKind::Codex,
                binding: target(ResourceKind::Binding, 19),
            },
            caller: Caller::Socket,
            policy: ToolPolicy {
                computer_capture: true,
                spawn_children: true,
                ..ToolPolicy::own_terminal()
            },
            captures: vec![ToolCapturedCommand {
                capture: ToolCapture::Computer,
                invocation: {
                    let mut invocation =
                        CommandInvocation::new("computer.capture", Vec::new(), Caller::Internal);
                    invocation.target = Some(target(ResourceKind::ApplicationWindow, 29));
                    invocation
                },
            }],
            spawn: Some(ToolSpawnContext { profile: None }),
        },
        &std::env::current_exe().map_err(|error| error.to_string())?,
        commands.clone(),
    )?;
    let lease = bridge.lease().clone();
    Ok((bridge, lease))
}

fn capture(lease: &ToolLease, commands: &dyn AgentCommandExecutor) -> Result<Value, String> {
    ToolProtocol::new(lease.clone())
        .handle(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"computer_snapshot"}}"#,
            Instant::now(),
            commands,
        )
        .ok_or_else(|| "Expected capture reply".to_owned())
}

#[rstest]
fn enabled_capture_survives_default_disabled_spawning(launch: AgentLaunch) {
    let directory = assert_fs::TempDir::new().unwrap();
    let service =
        Arc::new(TerminalAgentService::open(directory.path().join("agents.json")).unwrap());
    service.set_computer_capture_enabled(true);
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = capture_attachment(&commands).unwrap();
    let prepared = service
        .prepare_with_tools(
            AgentKind::Codex,
            launch,
            bridge,
            Some("Persistent terminal".to_owned()),
        )
        .unwrap();
    service
        .register(
            prepared,
            target(ResourceKind::Terminal, 23),
            "binding".to_owned(),
        )
        .unwrap();
    assert!(lease.enabled(Some(ToolCapture::Computer)));
    assert!(!lease.spawn_enabled());
    assert_eq!(
        capture(&lease, commands.as_ref()).unwrap()["result"]["isError"],
        false
    );
    let calls = commands.0.lock().unwrap().clone();
    let [call] = calls.as_slice() else {
        panic!("Expected one host capture")
    };
    assert_eq!(call.command, "computer.capture");
    assert_eq!(call.arguments, Vec::<String>::new());
    assert_eq!(call.caller, Caller::Socket);
    assert_eq!(
        call.target,
        Some(target(ResourceKind::ApplicationWindow, 29))
    );
}

#[rstest]
#[case::capture(false)]
#[case::spawning(true)]
fn disabling_one_feature_cancels_pending_capture_and_preserves_the_other(
    launch: AgentLaunch,
    #[case] disable_spawning: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let service =
        Arc::new(TerminalAgentService::open(directory.path().join("agents.json")).unwrap());
    service.set_computer_capture_enabled(true);
    service.set_agent_spawning_enabled(true);
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = capture_attachment(&commands).unwrap();
    let prepared = service
        .prepare_with_tools(
            AgentKind::Codex,
            launch.clone(),
            bridge,
            Some("Persistent terminal".to_owned()),
        )
        .unwrap();
    service
        .register(
            prepared,
            target(ResourceKind::Terminal, 23),
            "binding".to_owned(),
        )
        .unwrap();
    let disable = |_: CommandInvocation, _: Instant, cancellation: CommandCancellation| {
        if disable_spawning {
            service.set_agent_spawning_enabled(false);
        } else {
            service.set_computer_capture_enabled(false);
        }
        assert!(cancellation.is_cancelled());
        CommandOutcome::Success {
            value: json!({"text":"discard this cancelled capture"}),
            warnings: Vec::new(),
        }
    };
    assert_eq!(
        capture(&lease, &disable).unwrap()["result"]["isError"],
        true
    );
    assert!(lease.enabled(None));
    assert_eq!(lease.enabled(Some(ToolCapture::Computer)), disable_spawning);
    assert_eq!(lease.spawn_enabled(), !disable_spawning);
    let request = ToolSpawnRequest::Shell {
        name: "child".to_owned(),
        title: None,
    };
    assert_eq!(lease.authorize_spawn(&request).is_ok(), !disable_spawning);
    service.set_computer_capture_enabled(true);
    service.set_agent_spawning_enabled(true);
    assert_eq!(lease.enabled(Some(ToolCapture::Computer)), disable_spawning);
    assert_eq!(lease.spawn_enabled(), !disable_spawning);

    let (bridge, new_lease) = capture_attachment(&commands).unwrap();
    let prepared = service
        .prepare_with_tools(
            AgentKind::Codex,
            launch,
            bridge,
            Some("Persistent terminal".to_owned()),
        )
        .unwrap();
    service
        .register(
            prepared,
            target(ResourceKind::Terminal, 24),
            "binding".to_owned(),
        )
        .unwrap();
    assert!(new_lease.enabled(Some(ToolCapture::Computer)));
    assert!(new_lease.spawn_enabled());
}

#[rstest]
fn disabling_prepared_capture_never_widens_it_when_reenabled(launch: AgentLaunch) {
    let directory = assert_fs::TempDir::new().unwrap();
    let service =
        Arc::new(TerminalAgentService::open(directory.path().join("agents.json")).unwrap());
    service.set_computer_capture_enabled(true);
    service.set_agent_spawning_enabled(true);
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = capture_attachment(&commands).unwrap();
    let prepared = service
        .prepare_with_tools(
            AgentKind::Codex,
            launch,
            bridge,
            Some("Persistent terminal".to_owned()),
        )
        .unwrap();
    service.set_computer_capture_enabled(false);
    service.set_computer_capture_enabled(true);
    service
        .register(
            prepared,
            target(ResourceKind::Terminal, 23),
            "binding".to_owned(),
        )
        .unwrap();
    assert!(!lease.enabled(Some(ToolCapture::Computer)));
    assert!(lease.spawn_enabled());
    assert!(lease.enabled(None));
}

#[rstest]
fn attached_runtime_options_precede_prompt_and_never_enter_retained_configuration(
    launch: AgentLaunch,
) {
    let directory = assert_fs::TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let service = Arc::new(
        TerminalAgentService::open(directory.path().join("agents.json"))
            .unwrap_or_else(|error| panic!("{error}")),
    );
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = attachment(&commands).unwrap_or_else(|error| panic!("{error}"));
    let injected = bridge.arguments();
    let prepared = service
        .prepare_with_tools(
            AgentKind::Codex,
            launch.clone(),
            bridge,
            Some("Persistent terminal".to_owned()),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(prepared.launch, launch);
    let argv = prepared.argv();
    let expected = std::iter::once(launch.program)
        .chain(injected)
        .chain(launch.arguments)
        .collect::<Vec<_>>();
    assert_eq!(argv, expected);
    assert_eq!(
        read(&lease, &commands).unwrap_or_else(|error| panic!("{error}"))["result"]["isError"],
        true
    );
    let terminal = target(ResourceKind::Terminal, 23);
    let record = service
        .register(prepared, terminal.clone(), "binding".to_owned())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(record.launch.arguments, ["--model", "selected"]);
    let retained = fs::read_to_string(directory.path().join("agents.json"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(!retained.contains("mcp_servers"));
    assert!(!retained.contains("bt-tool-"));
    assert!(!retained.contains("literal prompt"));
    assert_eq!(
        read(&lease, &commands).unwrap_or_else(|error| panic!("{error}"))["result"]["isError"],
        false
    );
    let calls = commands
        .0
        .lock()
        .unwrap_or_else(|error| panic!("{error}"))
        .clone();
    let [call] = calls.as_slice() else {
        panic!("one terminal read: {}", calls.len())
    };
    assert_eq!(call.caller, Caller::Socket);
    assert_eq!(call.target, Some(terminal));
}

#[rstest]
#[case("retire")]
#[case("failed_retire")]
#[case("disable")]
#[case("shutdown")]
#[case("replace")]
fn lifecycle_revokes_tools_immediately_without_removing_metadata(
    launch: AgentLaunch,
    #[case] event: &str,
) {
    let directory = assert_fs::TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let path = directory.path().join("agents.json");
    let service =
        Arc::new(TerminalAgentService::open(&path).unwrap_or_else(|error| panic!("{error}")));
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = attachment(&commands).unwrap_or_else(|error| panic!("{error}"));
    let prepared = service
        .prepare_with_tools(
            AgentKind::Codex,
            launch.clone(),
            bridge,
            Some("Persistent terminal".to_owned()),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    service
        .register(
            prepared,
            target(ResourceKind::Terminal, 23),
            "binding".to_owned(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    match event {
        "retire" => {
            service
                .retire(&target(ResourceKind::Terminal, 23))
                .unwrap_or_else(|error| panic!("{error}"));
        }
        "failed_retire" => {
            fs::remove_file(&path).unwrap_or_else(|error| panic!("{error}"));
            fs::create_dir(&path).unwrap_or_else(|error| panic!("{error}"));
            assert!(service.retire(&target(ResourceKind::Terminal, 23)).is_err());
        }
        "disable" => {
            service.set_provider_tools_enabled(AgentKind::Codex, false);
            service.set_provider_tools_enabled(AgentKind::Codex, true);
        }
        "shutdown" => service.shutdown(),
        "replace" => {
            let prepared = TerminalAgentService::prepare_unobserved(
                AgentKind::Codex,
                launch,
                "Replacement".to_owned(),
            )
            .unwrap_or_else(|error| panic!("{error}"));
            service
                .register(
                    prepared,
                    target(ResourceKind::Terminal, 24),
                    "binding".to_owned(),
                )
                .unwrap_or_else(|error| panic!("{error}"));
        }
        _ => panic!("unknown event"),
    }
    assert!(!lease.enabled(None));
    assert_eq!(
        read(&lease, &commands).unwrap_or_else(|error| panic!("{error}"))["result"]["isError"],
        true
    );
    assert_eq!(service.records().len(), 1);
}

#[rstest]
#[case::initial(false, false)]
#[case::pending(true, false)]
#[case::pending_reenabled(true, true)]
fn disabling_blocks_initial_and_prepared_attachments_without_losing_committed_metadata(
    launch: AgentLaunch,
    #[case] prepared_before_disable: bool,
    #[case] reenabled: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let service = Arc::new(
        TerminalAgentService::open(directory.path().join("agents.json"))
            .unwrap_or_else(|error| panic!("{error}")),
    );
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = attachment(&commands).unwrap_or_else(|error| panic!("{error}"));
    if prepared_before_disable {
        let prepared = service
            .prepare_with_tools(
                AgentKind::Codex,
                launch,
                bridge,
                Some("Persistent terminal".to_owned()),
            )
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(prepared.tools_enabled());
        service.set_provider_tools_enabled(AgentKind::Codex, false);
        assert!(!prepared.tools_enabled());
        assert!(!lease.enabled(None));
        if reenabled {
            service.set_provider_tools_enabled(AgentKind::Codex, true);
            assert!(!prepared.tools_enabled());
        }
        assert!(
            service
                .register(
                    prepared,
                    target(ResourceKind::Terminal, 23),
                    "binding".to_owned()
                )
                .is_err()
        );
        assert_eq!(service.records().len(), 1);
    } else {
        service.set_provider_tools_enabled(AgentKind::Codex, false);
        assert!(
            service
                .prepare_with_tools(
                    AgentKind::Codex,
                    launch,
                    bridge,
                    Some("Persistent terminal".to_owned())
                )
                .is_err()
        );
        assert_eq!(service.records().len(), 0);
    }
    assert!(!lease.enabled(None));
}

#[rstest]
#[case::count(false)]
#[case::bytes(true)]
fn oversized_merged_arguments_revoke_prepared_authority_without_registering(
    launch: AgentLaunch,
    #[case] bytes: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let service = Arc::new(
        TerminalAgentService::open(directory.path().join("agents.json"))
            .unwrap_or_else(|error| panic!("{error}")),
    );
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = attachment(&commands).unwrap_or_else(|error| panic!("{error}"));
    let mut launch = launch;
    launch.arguments = if bytes {
        let mut remaining = (64_usize * 1024)
            .saturating_sub(launch.program.len())
            .saturating_sub(launch.cwd.as_ref().map_or(0, String::len))
            .saturating_sub(1);
        let mut arguments = Vec::new();
        while remaining > 0 {
            let size = remaining.min(8192);
            arguments.push("x".repeat(size));
            remaining = remaining.saturating_sub(size);
        }
        arguments
    } else {
        vec!["literal".to_owned(); 64]
    };
    assert!(launch.validate().is_ok());
    assert!(
        service
            .prepare_with_tools(
                AgentKind::Codex,
                launch,
                bridge,
                Some("Persistent terminal".to_owned())
            )
            .is_err()
    );
    assert!(!lease.enabled(None));
    assert_eq!(service.records().len(), 0);
}

#[rstest]
fn failed_registration_revokes_tools_and_keeps_prior_catalog(launch: AgentLaunch) {
    let directory = assert_fs::TempDir::new().unwrap_or_else(|error| panic!("{error}"));
    let path = directory.path().join("agents.json");
    let service =
        Arc::new(TerminalAgentService::open(&path).unwrap_or_else(|error| panic!("{error}")));
    let commands = Arc::new(Commands::default());
    let (bridge, lease) = attachment(&commands).unwrap_or_else(|error| panic!("{error}"));
    let prepared = service
        .prepare_with_tools(
            AgentKind::Codex,
            launch,
            bridge,
            Some("Persistent terminal".to_owned()),
        )
        .unwrap_or_else(|error| panic!("{error}"));
    fs::create_dir(path).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        service
            .register(
                prepared,
                target(ResourceKind::Terminal, 23),
                "binding".to_owned()
            )
            .is_err()
    );
    assert!(!lease.enabled(None));
    assert_eq!(service.records().len(), 0);
}

#[expect(
    clippy::unwrap_used,
    reason = "Construct a fixed valid one-pixel PNG fixture"
)]
fn snapshot() -> Value {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 0, 0, 255]).unwrap();
        writer.finish().unwrap();
    }
    json!({"result":"snapshot","png_base64":STANDARD.encode(bytes),"pixel_width":1,"pixel_height":1,
        "target":{"window_id":42,"process_id":123,"bundle_id":"dev.bootty.test", "launch_time":1000.0,
            "bounds":{"x":0.0,"y":0.0,"width":1.0,"height":1.0},"title":null}})
}
