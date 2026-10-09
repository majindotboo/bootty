use bootty_agents::{
    AgentKind, AgentLaunch, AgentObservation, PreparedTerminalRestore, TerminalAgentRecord,
    TerminalAgentService, TerminalAgentStatus,
};
use bootty_control::{CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn record(provider: AgentKind, prompt: &str) -> TerminalAgentRecord {
    TerminalAgentRecord {
        provider,
        target: target("old", 1),
        binding_id: "binding".to_owned(),
        location: None,
        launch: AgentLaunch {
            program: provider.default_program().to_owned(),
            cwd: Some("/project".to_owned()),
            account_directory: Some("/captured-account".to_owned()),
            arguments: vec![
                "--model".to_owned(),
                "captured-model".to_owned(),
                prompt.to_owned(),
            ],
            ephemeral: false,
        },
        observation: AgentObservation {
            session_id: Some("66812ac6-48bc-4e7f-a982-7997a82d28bc".to_owned()),
            session_file: (provider == AgentKind::Pi)
                .then(|| "/captured-account/session.jsonl".to_owned()),
            status: TerminalAgentStatus::Unavailable,
            detail: None,
        },
    }
}
fn target(handle: &str, generation: u64) -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Terminal,
        handle: handle.to_owned(),
        generation,
    }
}

proptest! {
    #[test]
    fn native_resume_preserves_exact_account_model_and_omits_prompts(prompt in "[a-z]{1,80}", provider in prop::sample::select(AgentKind::ALL.to_vec())) {
        let source = record(provider, &prompt);
        let launch = source.recovery_launch().unwrap();
        prop_assert_eq!(&launch.program, &source.launch.program);
        prop_assert_eq!(&launch.cwd, &source.launch.cwd);
        prop_assert_eq!(&launch.account_directory, &source.launch.account_directory);
        let selector = if provider == AgentKind::Pi { source.observation.session_file.as_ref() } else { source.observation.session_id.as_ref() }.unwrap();
        prop_assert!(launch.arguments.contains(selector));
        prop_assert!(launch.arguments.contains(&"captured-model".to_owned()));
        prop_assert!(!launch.arguments.contains(&prompt));
        prop_assert!(!launch.arguments.contains(&"--fork".to_owned()));
        prop_assert!(!launch.arguments.contains(&"--fork-session".to_owned()));
    }
}

enum RecoveryBoundary {
    Stopped,
    Ephemeral,
    MissingAccount,
    MissingSession,
}

#[rstest]
#[case(RecoveryBoundary::Stopped)]
#[case(RecoveryBoundary::Ephemeral)]
#[case(RecoveryBoundary::MissingAccount)]
#[case(RecoveryBoundary::MissingSession)]
fn unsupported_recovery_never_starts_a_new_conversation(#[case] boundary: RecoveryBoundary) {
    let mut source = record(AgentKind::Claude, "old prompt");
    match boundary {
        RecoveryBoundary::Stopped => source.observation.status = TerminalAgentStatus::Stopped,
        RecoveryBoundary::Ephemeral => source.launch.ephemeral = true,
        RecoveryBoundary::MissingAccount => source.launch.account_directory = None,
        RecoveryBoundary::MissingSession => source.observation.session_id = None,
    }
    assert!(source.recovery_launch().is_err());
}

#[rstest]
fn private_preparation_is_consumed_once_and_durable_registration_replaces_source() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("agents.json");
    let source = record(AgentKind::Claude, "old prompt");
    std::fs::write(&path, serde_json::to_vec(&vec![source.clone()]).unwrap()).unwrap();
    let service = TerminalAgentService::open(&path).unwrap();
    let source = service
        .associate_location(
            &source,
            bootty_agents::TerminalAgentLocation {
                task_identity: "task".to_owned(),
                window_id: "saved-window".to_owned(),
                pane_id: "saved-pane".to_owned(),
            },
        )
        .unwrap();
    let prepared = TerminalAgentService::prepare_unobserved(
        source.provider,
        source.recovery_launch().unwrap(),
        "fixture".to_owned(),
    )
    .unwrap();
    let fresh = target("new", 2);
    let token = service
        .stage_restore(PreparedTerminalRestore {
            prepared,
            source: source.clone(),
            target: fresh.clone(),
        })
        .unwrap();
    let restore = service.take_restore(&token).unwrap();
    assert!(service.take_restore(&token).is_none());
    let restored = service
        .register_restored(restore.prepared, restore.target, &restore.source)
        .unwrap();
    assert_eq!(restored.location, source.location);
    assert_eq!(
        restored.observation.session_id,
        source.observation.session_id
    );
    assert_eq!(restored.launch, source.launch.retained(source.provider));
    assert_eq!(service.records().len(), 1);
    assert_eq!(service.records()[0].target, fresh);
    assert!(service.record(&source.target).is_none());
    service.shutdown_and_wait().unwrap();
    let reopened = TerminalAgentService::open(path).unwrap();
    assert_eq!(reopened.records().len(), 1);
    assert_eq!(reopened.records()[0].target, fresh);
    assert_eq!(reopened.records()[0].location, source.location);
}

#[rstest]
#[case::account("account")]
#[case::model("model")]
#[case::stopped("stopped")]
fn changed_source_is_not_replaced_by_stale_preparation(#[case] change: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("agents.json");
    let source = record(AgentKind::Claude, "old prompt");
    std::fs::write(&path, serde_json::to_vec(&vec![source.clone()]).unwrap()).unwrap();
    let service = TerminalAgentService::open(&path).unwrap();
    let prepared = TerminalAgentService::prepare_unobserved(
        source.provider,
        source.recovery_launch().unwrap(),
        "fixture".to_owned(),
    )
    .unwrap();
    if change == "stopped" {
        service.retire(&source.target).unwrap();
    } else {
        let mut launch = source.launch.clone();
        if change == "account" {
            launch.account_directory = Some("/other-account".to_owned());
        } else {
            launch.arguments = vec!["--model".to_owned(), "other-model".to_owned()];
        }
        let changed =
            TerminalAgentService::prepare_unobserved(source.provider, launch, "changed".to_owned())
                .unwrap();
        service
            .register(changed, source.target.clone(), source.binding_id.clone())
            .unwrap();
    }
    assert!(
        service
            .register_restored(prepared, target("new", 2), &source)
            .is_err()
    );
    assert_eq!(service.records().len(), 1);
    assert_eq!(service.records()[0].target, source.target);
}
