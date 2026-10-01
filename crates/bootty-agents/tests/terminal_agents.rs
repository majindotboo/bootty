use assert_fs::TempDir;
use bootty_agents::{AgentKind, AgentLaunch, TerminalAgentRecord, TerminalAgentService};
use bootty_control::{CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn record(provider: AgentKind) -> TerminalAgentRecord {
    TerminalAgentRecord {
        provider,
        target: CommandTarget {
            kind: ResourceKind::Terminal,
            handle: "host-issued-target".to_owned(),
            generation: 7,
        },
        binding_id: "binding".to_owned(),
        launch: AgentLaunch {
            program: provider.default_program().to_owned(),
            cwd: Some("/project".to_owned()),
            arguments: vec!["--api-key".to_owned(), "private-credential".to_owned()],
            ephemeral: false,
        },
        session_id: Some("provider-session".to_owned()),
    }
}

#[rstest]
#[case(AgentKind::Codex)]
#[case(AgentKind::Claude)]
#[case(AgentKind::Pi)]
fn terminal_launch_metadata_is_durable_without_a_provider_process(#[case] provider: AgentKind) {
    let root = TempDir::new().unwrap();
    let path = root.path().join("terminal.json");
    let registry = TerminalAgentService::open(&path).unwrap();
    let record = record(provider);
    let target = record.target.clone();
    registry.register(record).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("private-credential"));
    drop(registry);
    let reopened = TerminalAgentService::open(&path).unwrap();
    let restored = reopened.record(&target).unwrap();
    assert_eq!(restored.provider, provider);
    assert_eq!(restored.session_id.as_deref(), Some("provider-session"));
    let mut stale = target;
    stale.generation = stale.generation.saturating_add(1);
    assert!(reopened.record(&stale).is_none());
}

#[rstest]
fn failed_terminal_metadata_commit_preserves_the_prior_projection() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("terminal.json");
    let registry = TerminalAgentService::open(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(registry.register(record(AgentKind::Codex)).is_err());
    assert!(registry.records().is_empty());
}

#[cfg(unix)]
#[rstest]
#[case(None)]
#[case(Some("configured"))]
fn an_existing_backend_environment_cannot_suppress_agent_colors(#[case] configured: Option<&str>) {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = assert_fs::TempDir::new().unwrap();
    let program = directory.path().join("literal executable");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"${NO_COLOR-unset}\" \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let literal = "quoted ' value; $HOME `uname`";
    let launch = AgentLaunch {
        program: program.to_string_lossy().into_owned(),
        cwd: None,
        arguments: vec![literal.to_owned()],
        ephemeral: false,
    };
    let argv = launch.posix_terminal_argv(configured);
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env("NO_COLOR", "inherited")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n{literal}\n", configured.unwrap_or("unset"))
    );
}
