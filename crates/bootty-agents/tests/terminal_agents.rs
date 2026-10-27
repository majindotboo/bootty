#![cfg(test)]

use std::fs;

use bootty_agents::{
    AgentKind, AgentLaunch, TerminalAgentService, TerminalAgentStatus, claude_terminal_observation,
};
use bootty_control::{CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use serde_json::json;

#[rstest]
#[case("busy", TerminalAgentStatus::Working)]
#[case("waiting", TerminalAgentStatus::Waiting)]
#[case("idle", TerminalAgentStatus::Idle)]
#[case("unrecognized", TerminalAgentStatus::Unavailable)]
fn claude_query_selects_the_exact_interactive_identity(
    #[case] status: &str,
    #[case] expected: TerminalAgentStatus,
) {
    let bytes = serde_json::to_vec(&json!([
        {"sessionId":"other", "cwd":"same", "status":"waiting"},
        {"sessionId":"requested", "kind":"interactive", "cwd":"same", "status":status, "waitingFor":"permission prompt"}
    ])).unwrap();
    let observation = claude_terminal_observation(&bytes, "requested")
        .unwrap()
        .unwrap();
    assert_eq!(observation.status, expected);
    assert_eq!(observation.session_id.as_deref(), Some("requested"));
    assert_eq!(observation.detail.as_deref(), Some("permission prompt"));
    assert_eq!(claude_terminal_observation(&bytes, "absent").unwrap(), None);
}

#[rstest]
#[case("done", TerminalAgentStatus::Finished)]
#[case("failed", TerminalAgentStatus::Error)]
#[case("stopped", TerminalAgentStatus::Stopped)]
#[case("unknown", TerminalAgentStatus::Working)]
fn claude_completion_uses_the_reported_session_state(
    #[case] state: &str,
    #[case] expected: TerminalAgentStatus,
) {
    let bytes = serde_json::to_vec(&json!([
        {"sessionId":"exact", "status":"busy", "state":state}
    ]))
    .unwrap();
    let observation = claude_terminal_observation(&bytes, "exact")
        .unwrap()
        .unwrap();
    assert_eq!(observation.status, expected);
}

#[rstest]
fn duplicate_identity_is_unavailable_instead_of_choosing_a_list_entry() {
    let bytes = serde_json::to_vec(&json!([
        {"sessionId":"same", "status":"busy"},
        {"sessionId":"same", "status":"idle"}
    ]))
    .unwrap();
    assert!(claude_terminal_observation(&bytes, "same").is_err());
}

proptest! {
    #[test]
    fn another_session_cannot_supply_requested_activity(other in "[a-z]{1,64}") {
        prop_assume!(other != "requested");
        let bytes = serde_json::to_vec(&json!([{ "sessionId":other, "status":"busy" }])).unwrap();
        prop_assert_eq!(claude_terminal_observation(&bytes, "requested").unwrap(), None);
    }
}

fn target(generation: u64) -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "host-issued-terminal".to_owned(),
        generation,
    }
}

fn launch() -> AgentLaunch {
    AgentLaunch {
        program: std::env::temp_dir()
            .join("bootty-unobserved-provider-fixture")
            .to_string_lossy()
            .into_owned(),
        cwd: None,
        arguments: vec![
            "--model".to_owned(),
            "selected-model".to_owned(),
            "--config".to_owned(),
            "credential=private".to_owned(),
            "prompt".to_owned(),
        ],
        ephemeral: false,
        account_directory: None,
    }
}

#[rstest]
fn terminal_registry_replaces_generation_and_retains_only_reusable_configuration() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("terminals.json");
    let service = TerminalAgentService::open(&path).unwrap();
    for generation in [1, 2] {
        let prepared = TerminalAgentService::prepare_unobserved(
            AgentKind::Codex,
            launch(),
            "Observation unsupported for this host".to_owned(),
        )
        .unwrap();
        service
            .register(prepared, target(generation), "binding".to_owned())
            .unwrap();
    }
    assert!(service.record(&target(1)).is_none());
    assert_eq!(service.records().len(), 1);
    assert_eq!(
        service.record(&target(2)).unwrap().launch.arguments,
        vec!["--model", "selected-model"]
    );
    drop(service);
    let restored = TerminalAgentService::open(path).unwrap();
    assert_eq!(
        restored.activity(&target(2)).unwrap().status,
        TerminalAgentStatus::Unavailable
    );
    assert_eq!(
        restored.record(&target(2)).unwrap().launch.arguments,
        vec!["--model", "selected-model"]
    );
}

#[rstest]
fn unobserved_launch_preserves_literal_tui_arguments_without_starting_a_provider_process() {
    let directory = assert_fs::TempDir::new().unwrap();
    let service = TerminalAgentService::open(directory.path().join("terminals.json")).unwrap();
    let launch = AgentLaunch {
        program: directory
            .path()
            .join("missing-provider")
            .to_string_lossy()
            .into_owned(),
        cwd: None,
        arguments: vec![
            "--profile".to_owned(),
            "configured-profile".to_owned(),
            "resume".to_owned(),
            "actual-provider-id".to_owned(),
            "literal\nargument\t$HOME".to_owned(),
        ],
        ephemeral: false,
        account_directory: None,
    };
    let original = std::iter::once(launch.program.clone())
        .chain(launch.arguments.clone())
        .collect::<Vec<_>>();
    let prepared = TerminalAgentService::prepare_unobserved(
        AgentKind::Codex,
        launch.clone(),
        "App-owned observation is unavailable for persistent terminals".to_owned(),
    )
    .unwrap();
    assert_eq!(prepared.launch, launch);
    assert_eq!(
        prepared
            .argv_for_shell(bootty_agents::LaunchShell::Posix)
            .unwrap(),
        original
    );
    let record = service
        .register(prepared, target(1), "persistent-binding".to_owned())
        .unwrap();
    assert_eq!(record.observation.status, TerminalAgentStatus::Unavailable);
    assert_eq!(record.observation.session_id, None);
    service.shutdown();
    assert_eq!(
        service.activity(&target(1)).unwrap().status,
        TerminalAgentStatus::Unavailable
    );
}

#[rstest]
fn failed_catalog_commit_does_not_publish_a_terminal_record() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("terminals.json");
    let service = TerminalAgentService::open(&path).unwrap();
    fs::create_dir(path).unwrap();
    let prepared = TerminalAgentService::prepare_unobserved(
        AgentKind::Codex,
        launch(),
        "Unsupported".to_owned(),
    )
    .unwrap();
    assert!(
        service
            .register(prepared, target(1), "binding".to_owned())
            .is_err()
    );
    assert!(service.records().is_empty());
}

#[cfg(unix)]
#[rstest]
#[case(
    AgentKind::Claude,
    "{\"loggedIn\":true,\"accessToken\":\"credential\"}",
    true
)]
#[case(
    AgentKind::Pi,
    "{\"status\":\"ready\",\"provider\":\"provider\",\"credentials\":\"credential\"}",
    true
)]
#[case(
    AgentKind::Pi,
    "{\"status\":\"not_ready\",\"provider\":\"provider\",\"reason\":\"credential_not_available\"}",
    false
)]
fn account_queries_publish_only_native_readiness(
    #[case] provider: AgentKind,
    #[case] output: &str,
    #[case] authenticated: bool,
) {
    let root = assert_fs::TempDir::new().unwrap();
    let account = root.path().to_string_lossy().into_owned();
    let captured_account = account.clone();
    let response = output.as_bytes().to_vec();
    let transport = bootty_agents::TerminalProviderTransport {
        output: std::sync::Arc::new(move |command, _cancelled| {
            assert_eq!(command.get_program(), "fixture-account-provider");
            let account = command
                .get_envs()
                .find(|(name, _)| *name == provider.account_directory_variable())
                .unwrap()
                .1
                .unwrap();
            assert_eq!(account, std::ffi::OsStr::new(&captured_account));
            let args = command
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                args,
                if provider == AgentKind::Claude {
                    vec!["auth", "status", "--json"]
                } else {
                    vec![
                        "auth",
                        "check",
                        "--provider",
                        "provider",
                        "--json",
                        "--no-refresh",
                    ]
                }
            );
            Ok(bootty_agents::TerminalQueryOutput {
                successful: authenticated,
                exit_code: Some(i32::from(!authenticated)),
                stdout: response.clone(),
                stderr: Vec::new(),
            })
        }),
        observer_launch: std::sync::Arc::new(|_| {
            panic!("account inspection must not start an observer")
        }),
        launch: std::sync::Arc::new(|_| {
            panic!("account inspection must not open a provider stream")
        }),
    };
    let result = bootty_agents::terminal_account_status_using(
        provider,
        "fixture-account-provider",
        Some(&bootty_agents::PiAccountSelector::provider("provider")),
        Some(&account),
        &transport,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({
            "provider":provider,
            "authenticated":authenticated,
            "detail":if provider == AgentKind::Pi && authenticated { Some("Pi model provider: provider. Pi does not report subscription information.") } else { None },
            "account":null,
            "auth_method":null,
            "subscription":null
        })
    );
}
