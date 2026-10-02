use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use assert_fs::TempDir;
use bootty_agents::{
    AgentKind, AgentLaunch, TerminalAgentRecord, TerminalAgentService, TerminalToolRequest,
    attach_terminal_tool_arguments, terminal_tools_supported,
};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
    TerminalToolOperation,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};
use serde_json::json;

type TerminalFixture = Result<(TempDir, TerminalAgentService, TerminalAgentRecord), String>;

#[fixture]
fn terminal() -> TerminalFixture {
    let root = TempDir::new().map_err(|error| error.to_string())?;
    let service = TerminalAgentService::open(root.path().join("terminal.json"))?;
    let record = TerminalAgentRecord {
        provider: AgentKind::Codex,
        binding_id: "workspace-a".to_owned(),
        target: CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "opaque-terminal-a".to_owned(),
            generation: 7,
        },
        launch: AgentLaunch {
            program: "codex".to_owned(),
            cwd: Some("/project".to_owned()),
            arguments: Vec::new(),
            ephemeral: false,
        },
        session_id: None,
    };
    Ok((root, service, record))
}

fn request(id: String, operation: TerminalToolOperation) -> TerminalToolRequest {
    TerminalToolRequest {
        attachment_id: id,
        provider: AgentKind::Codex,
        binding_id: "workspace-a".to_owned(),
        operation,
    }
}

fn invoke(
    service: &TerminalAgentService,
    request: &TerminalToolRequest,
    calls: &Mutex<Vec<CommandInvocation>>,
) -> CommandOutcome {
    service.invoke_terminal_tool(
        request,
        &|invocation, _, _| {
            calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(invocation);
            CommandOutcome::Success {
                value: json!({"text":"own screen"}),
                warnings: Vec::new(),
            }
        },
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        CommandCancellation::new(),
    )
}

#[rstest]
fn pending_launch_has_a_read_catalog_but_no_target(terminal: TerminalFixture) {
    let (_root, service, _) = terminal.unwrap();
    let id = service
        .reserve_terminal_tools(AgentKind::Codex, "workspace-a")
        .unwrap();
    let calls = Mutex::new(Vec::new());
    assert!(
        matches!(invoke(&service, &request(id.clone(), TerminalToolOperation::List), &calls), CommandOutcome::Success { value, .. } if value.get("enabled").and_then(serde_json::Value::as_bool) == Some(true))
    );
    assert!(matches!(
        invoke(&service, &request(id, TerminalToolOperation::Read), &calls),
        CommandOutcome::Unavailable { .. }
    ));
    assert!(
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[rstest]
fn read_uses_one_exact_terminal_and_revocation_takes_effect(terminal: TerminalFixture) {
    let (_root, service, record) = terminal.unwrap();
    let id = service
        .reserve_terminal_tools(record.provider, &record.binding_id)
        .unwrap();
    service.register(record.clone()).unwrap();
    service
        .complete_terminal_tools(&id, &record.target)
        .unwrap();
    let calls = Mutex::new(Vec::new());
    assert!(matches!(
        invoke(
            &service,
            &request(id.clone(), TerminalToolOperation::Read),
            &calls
        ),
        CommandOutcome::Success { .. }
    ));
    let expected = CommandInvocation {
        command: "terminal.read".to_owned(),
        arguments: Vec::new(),
        caller: Caller::Socket,
        target: Some(record.target),
        confirmation: None,
    };
    assert_eq!(
        *calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![expected]
    );
    service.revoke_terminal_tools(&id);
    for operation in [TerminalToolOperation::Read, TerminalToolOperation::List] {
        assert!(matches!(
            invoke(&service, &request(id.clone(), operation), &calls),
            CommandOutcome::Denied { .. }
        ));
    }
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1
    );
}

#[rstest]
fn reopening_retained_metadata_does_not_restore_tool_authority(terminal: TerminalFixture) {
    let (root, service, record) = terminal.unwrap();
    let id = service
        .reserve_terminal_tools(record.provider, &record.binding_id)
        .unwrap();
    service.register(record.clone()).unwrap();
    service
        .complete_terminal_tools(&id, &record.target)
        .unwrap();
    let reopened = TerminalAgentService::open(root.path().join("terminal.json")).unwrap();
    let calls = Mutex::new(Vec::new());
    assert!(matches!(
        invoke(&reopened, &request(id, TerminalToolOperation::Read), &calls),
        CommandOutcome::Denied { .. }
    ));
    assert!(
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn another_workspace_cannot_use_an_attachment(binding in "[a-z]{1,24}") {
        let (_root, service, record) = terminal().unwrap();
        let id = service.reserve_terminal_tools(record.provider, &record.binding_id).unwrap();
        service.register(record.clone()).unwrap();
        service.complete_terminal_tools(&id, &record.target).unwrap();
        let mut request = request(id, TerminalToolOperation::Read);
        request.binding_id = binding;
        let calls = Mutex::new(Vec::new());
        prop_assert!(matches!(invoke(&service, &request, &calls), CommandOutcome::Denied { .. }), "A different binding must be denied");
        prop_assert!(calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
    }
}

#[rstest]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn another_provider_cannot_use_an_attachment(
    terminal: TerminalFixture,
    #[case] provider: AgentKind,
) {
    let (_root, service, record) = terminal.unwrap();
    let id = service
        .reserve_terminal_tools(record.provider, &record.binding_id)
        .unwrap();
    service.register(record.clone()).unwrap();
    service
        .complete_terminal_tools(&id, &record.target)
        .unwrap();
    let mut request = request(id, TerminalToolOperation::Read);
    request.provider = provider;
    let calls = Mutex::new(Vec::new());
    assert!(matches!(
        invoke(&service, &request, &calls),
        CommandOutcome::Denied { .. }
    ));
    assert!(
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[rstest]
fn changed_launch_ownership_invalidates_the_attachment(terminal: TerminalFixture) {
    let (_root, service, mut record) = terminal.unwrap();
    let id = service
        .reserve_terminal_tools(record.provider, &record.binding_id)
        .unwrap();
    service.register(record.clone()).unwrap();
    service
        .complete_terminal_tools(&id, &record.target)
        .unwrap();
    record.binding_id = "workspace-b".to_owned();
    service.register(record).unwrap();
    let calls = Mutex::new(Vec::new());
    assert!(matches!(
        invoke(&service, &request(id, TerminalToolOperation::Read), &calls),
        CommandOutcome::Denied { .. }
    ));
    assert!(
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    );
}

#[rstest]
#[case(TerminalToolOperation::List)]
#[case(TerminalToolOperation::Read)]
fn a_closed_or_stale_terminal_reports_the_backend_failure(
    terminal: TerminalFixture,
    #[case] operation: TerminalToolOperation,
) {
    let (_root, service, record) = terminal.unwrap();
    let id = service
        .reserve_terminal_tools(record.provider, &record.binding_id)
        .unwrap();
    service.register(record.clone()).unwrap();
    service
        .complete_terminal_tools(&id, &record.target)
        .unwrap();
    let outcome = service.invoke_terminal_tool(
        &request(id, operation),
        &|invocation: CommandInvocation, _, _| {
            assert_eq!(invocation.target, Some(record.target.clone()));
            CommandOutcome::StaleTarget {
                message: "terminal is closed".to_owned(),
            }
        },
        Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        CommandCancellation::new(),
    );
    assert!(matches!(outcome, CommandOutcome::StaleTarget { .. }));
}

#[rstest]
fn a_completed_attachment_cannot_move_to_another_terminal(terminal: TerminalFixture) {
    let (_root, service, mut record) = terminal.unwrap();
    let id = service
        .reserve_terminal_tools(record.provider, &record.binding_id)
        .unwrap();
    service.register(record.clone()).unwrap();
    service
        .complete_terminal_tools(&id, &record.target)
        .unwrap();
    record.target.generation = record.target.generation.saturating_add(1);
    service.register(record.clone()).unwrap();
    assert!(
        service
            .complete_terminal_tools(&id, &record.target)
            .is_err()
    );
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
fn mcp_attachment_preserves_literal_provider_options(#[case] provider: AgentKind) {
    let mut launch = AgentLaunch {
        program: provider.default_program().to_owned(),
        cwd: None,
        arguments: vec![
            "--model".to_owned(),
            "chosen-model".to_owned(),
            "--".to_owned(),
            "prompt\ntext".to_owned(),
        ],
        ephemeral: false,
    };
    let original = launch.arguments.clone();
    assert!(terminal_tools_supported(provider, &launch));
    attach_terminal_tool_arguments(
        &mut launch,
        provider,
        Path::new("/literal path/bootty"),
        "workspace-a",
        "id-a",
        "bootty-dev-fixture",
    )
    .unwrap();
    assert!(launch.arguments.ends_with(&original));
    assert!(
        launch
            .retained(provider)
            .arguments
            .iter()
            .all(|argument| !argument.contains("mcp_servers") && argument != "--mcp-config")
    );
    let config: serde_json::Value = if provider == AgentKind::Claude {
        serde_json::from_str(launch.arguments.get(1).unwrap()).unwrap()
    } else {
        json!({})
    };
    if provider == AgentKind::Claude {
        assert_eq!(
            config
                .pointer("/mcpServers/bootty_terminal/command")
                .unwrap(),
            "/literal path/bootty"
        );
    }
}

#[rstest]
#[case(AgentKind::Claude, "--safe-mode")]
#[case(AgentKind::Claude, "--mcp-config")]
#[case(AgentKind::Claude, "--tools=")]
#[case(AgentKind::Codex, "mcp_servers.bootty_terminal.enabled=false")]
#[case(AgentKind::Codex, "--remote")]
#[case(AgentKind::Pi, "")]
fn explicit_provider_tool_configuration_is_preserved(
    #[case] provider: AgentKind,
    #[case] option: &str,
) {
    let launch = AgentLaunch {
        program: provider.default_program().to_owned(),
        cwd: None,
        arguments: vec![option.to_owned()],
        ephemeral: false,
    };
    assert!(!terminal_tools_supported(provider, &launch));
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
fn failed_attachment_preserves_a_valid_provider_launch_at_the_argv_limit(
    #[case] provider: AgentKind,
) {
    let arguments = ["--model", "chosen-model"]
        .into_iter()
        .cycle()
        .take(64)
        .map(str::to_owned)
        .collect();
    let mut launch = AgentLaunch {
        program: provider.default_program().to_owned(),
        cwd: None,
        arguments,
        ephemeral: false,
    };
    launch.validate().unwrap();
    let original = launch.clone();
    assert!(
        attach_terminal_tool_arguments(
            &mut launch,
            provider,
            Path::new("/bootty"),
            "workspace-a",
            "id-a",
            "bootty-dev-fixture"
        )
        .is_err()
    );
    assert_eq!(launch, original);
    launch.validate().unwrap();
}
