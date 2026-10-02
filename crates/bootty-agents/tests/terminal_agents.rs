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
        program: "codex".to_owned(),
        cwd: None,
        arguments: vec![
            "--model".to_owned(),
            "selected-model".to_owned(),
            "--config".to_owned(),
            "credential=private".to_owned(),
            "prompt".to_owned(),
        ],
        ephemeral: false,
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
    };
    let original = std::iter::once(launch.program.clone())
        .chain(launch.arguments.clone())
        .collect::<Vec<_>>();
    let prepared = TerminalAgentService::prepare_unobserved(
        AgentKind::Codex,
        launch,
        "App-owned observation is unavailable for persistent terminals".to_owned(),
    )
    .unwrap();
    assert_eq!(prepared.argv(), original);
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
#[case(false)]
#[case(true)]
fn exact_retirement_preserves_observed_identity_and_commits_before_publication(
    #[case] initially_failed_commit: bool,
) {
    use std::{
        os::unix::fs::PermissionsExt as _,
        sync::{Arc, mpsc},
        time::Duration,
    };

    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("terminals.json");
    let service = Arc::new(TerminalAgentService::open(&path).unwrap());
    let program = directory.path().join("claude-query");
    let session = "8ea5a4d1-9c09-4e2a-93e2-c4d2d9658b60";
    fs::write(
        &program,
        format!("#!/bin/sh\nprintf '%s' '[{{\"sessionId\":\"{session}\",\"status\":\"busy\"}}]'\n"),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let (changed, changes) = mpsc::channel();
    service.set_change_handler(Arc::new(move || {
        let _ = changed.send(());
    }));
    let prepared = service
        .prepare(
            AgentKind::Claude,
            AgentLaunch {
                program: program.to_string_lossy().into_owned(),
                cwd: None,
                arguments: vec!["--session-id".to_owned(), session.to_owned()],
                ephemeral: false,
            },
        )
        .unwrap();
    service
        .register(prepared, target(1), "binding".to_owned())
        .unwrap();
    loop {
        if service.activity(&target(1)).unwrap().session_id.as_deref() == Some(session) {
            break;
        }
        changes
            .recv_timeout(Duration::from_secs(2))
            .expect("native query publishes identity");
    }
    let before = serde_json::to_value(service.records()).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(service.retire(&target(2)).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(serde_json::to_value(service.records()).unwrap(), before);
    if initially_failed_commit {
        let preserved = directory.path().join("preserved.json");
        fs::rename(&path, &preserved).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(service.retire(&target(1)).is_err());
        assert_eq!(serde_json::to_value(service.records()).unwrap(), before);
        fs::remove_dir(&path).unwrap();
        fs::rename(preserved, &path).unwrap();
    }
    let retired = service.retire(&target(1)).unwrap();
    assert_eq!(retired.observation.status, TerminalAgentStatus::Stopped);
    assert_eq!(retired.observation.session_id.as_deref(), Some(session));
    let persisted: Vec<bootty_agents::TerminalAgentRecord> =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(persisted).unwrap(),
        serde_json::to_value(service.records()).unwrap()
    );
    drop(service);
    let restored = TerminalAgentService::open(path).unwrap();
    let activity = restored.activity(&target(1)).unwrap();
    assert_eq!(activity.status, TerminalAgentStatus::Stopped);
    assert_eq!(activity.session_id.as_deref(), Some(session));
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
    "{\"status\":\"ready\",\"credentials\":\"credential\"}",
    true
)]
#[case(
    AgentKind::Pi,
    "{\"status\":\"not_ready\",\"reason\":\"credential_not_available\"}",
    false
)]
fn account_queries_publish_only_native_readiness(
    #[case] provider: AgentKind,
    #[case] output: &str,
    #[case] authenticated: bool,
) {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = assert_fs::TempDir::new().unwrap();
    let program = directory.path().join("provider");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s' '{}'\n",
            output.replace('\'', "'\\''")
        ),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let result = bootty_agents::terminal_account_status(
        provider,
        program.to_str().unwrap(),
        Some("provider"),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({"provider":provider,"authenticated":authenticated,"detail":null})
    );
}
