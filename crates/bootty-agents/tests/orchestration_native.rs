use bootty_agents::{
    AgentKind, AgentLaunch, AgentPrompt, NativeTurnOutcome, NativeTurnReceipt,
    OrchestrationContext, OrchestrationDispatch, OrchestrationLaunch, OrchestrationNodeSpec,
    OrchestrationNodeState, OrchestrationOutcome, OrchestrationPlan, OrchestrationService,
};
use bootty_control::{Caller, CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    directory: assert_fs::TempDir,
    service: OrchestrationService,
    dispatch: OrchestrationDispatch,
    target: CommandTarget,
}

fn receipt(outcome: NativeTurnOutcome) -> NativeTurnReceipt {
    NativeTurnReceipt {
        id: "first-provider-turn".into(),
        outcome,
    }
}

#[fixture]
fn fixture() -> TestResult<Fixture> {
    let directory = assert_fs::TempDir::new()?;
    let service = OrchestrationService::open(directory.path().join("runs.json"))?;
    let context = OrchestrationContext::capture_destination(
        CommandTarget {
            kind: ResourceKind::Binding,
            handle: "exact-binding".into(),
            generation: 3,
        },
        Caller::Socket,
        "exact-window".into(),
        "durable-binding".into(),
    )?;
    let launch = OrchestrationLaunch::capture_for_task(
        AgentKind::Codex,
        Some("captured".into()),
        AgentLaunch {
            program: "codex".into(),
            cwd: Some("/project".into()),
            arguments: Vec::new(),
            ephemeral: false,
            account_directory: Some("/account".into()),
        },
        "existing-saved-task".into(),
    )?;
    let node = |id: &str, dependencies| {
        Ok::<_, String>(OrchestrationNodeSpec {
            id: id.into(),
            title: id.into(),
            dependencies,
            prompt: AgentPrompt::new("Inspect this task".into())?,
            launch: launch.clone(),
        })
    };
    let plan = OrchestrationPlan::new(vec![node("a", Vec::new())?, node("b", vec!["a".into()])?])?;
    service.create("run".into(), context, &plan)?;
    let dispatch = service
        .claim_ready("run", 32)?
        .pop()
        .ok_or("First node was not ready")?;
    let target = CommandTarget {
        kind: ResourceKind::Session,
        handle: "accepted-native-id".into(),
        generation: 7,
    };
    service.accept_native(
        &dispatch.token,
        dispatch.context.binding(),
        target.clone(),
        &receipt(NativeTurnOutcome::Running),
    )?;
    Ok(Fixture {
        directory,
        service,
        dispatch,
        target,
    })
}

proptest! {
    #[test]
    fn another_turn_or_process_generation_cannot_release_dependencies(
        changed_turn in any::<bool>(),
        generation_delta in 1_u64..u64::MAX - 7,
    ) {
        let fixture = fixture().map_err(|error| TestCaseError::fail(error.to_string()))?;
        let mut target = fixture.target.clone();
        let mut observed = receipt(NativeTurnOutcome::Succeeded);
        if changed_turn { observed.id = "later-manual-turn".into(); }
        else { target.generation = target.generation.checked_add(generation_delta).ok_or_else(|| TestCaseError::fail("Generation overflow"))?; }
        let before = fixture.service.snapshot();
        prop_assert!(fixture.service.finish_native(&fixture.dispatch.token, &target, &observed).is_err());
        prop_assert_eq!(fixture.service.snapshot(), before);
        prop_assert!(fixture.service.claim_ready("run", 32).map_err(TestCaseError::fail)?.is_empty());
    }
}

#[rstest]
#[case(NativeTurnOutcome::Running, false)]
#[case(NativeTurnOutcome::Succeeded, true)]
#[case(NativeTurnOutcome::Failed, false)]
#[case(NativeTurnOutcome::Interrupted, false)]
fn only_the_accepted_successful_first_turn_releases_a_dependent(
    fixture: TestResult<Fixture>,
    #[case] outcome: NativeTurnOutcome,
    #[case] advances: bool,
) {
    let fixture = fixture.unwrap();
    // Generic terminal completion cannot fabricate success for a native conversation.
    assert!(
        fixture
            .service
            .finish(
                &fixture.dispatch.token,
                &fixture.target,
                OrchestrationOutcome::Succeeded
            )
            .is_err()
    );
    let before = fixture.service.snapshot();
    fixture
        .service
        .finish_native(&fixture.dispatch.token, &fixture.target, &receipt(outcome))
        .unwrap();
    if outcome == NativeTurnOutcome::Running {
        assert_eq!(fixture.service.snapshot(), before);
    }
    let ready = fixture.service.claim_ready("run", 32).unwrap();
    assert_eq!(ready.len(), usize::from(advances));
    if advances {
        assert_eq!(ready[0].token.node_id(), "b");
    }
    assert_eq!(
        fixture.service.snapshot().runs[0].nodes[0]
            .native_turn_id
            .as_deref(),
        Some("first-provider-turn")
    );
}

#[rstest]
fn recovery_cancel_and_retry_never_reuse_the_old_native_turn(fixture: TestResult<Fixture>) {
    let fixture = fixture.unwrap();
    let recovered = OrchestrationService::open(fixture.directory.path().join("runs.json")).unwrap();
    assert!(matches!(
        recovered.snapshot().runs[0].nodes[0].state,
        OrchestrationNodeState::Interrupted { .. }
    ));
    assert_eq!(recovered.claim_ready("run", 32).unwrap(), Vec::new());
    recovered.retry("run", "a").unwrap();
    let retried = recovered.claim_ready("run", 32).unwrap().pop().unwrap();
    assert_eq!(recovered.snapshot().runs[0].nodes[0].native_turn_id, None);
    assert!(
        recovered
            .finish_native(
                &fixture.dispatch.token,
                &fixture.target,
                &receipt(NativeTurnOutcome::Succeeded)
            )
            .is_err()
    );
    let target = CommandTarget {
        generation: 8,
        ..fixture.target
    };
    recovered
        .accept_native(
            &retried.token,
            retried.context.binding(),
            target.clone(),
            &receipt(NativeTurnOutcome::Running),
        )
        .unwrap();
    assert_eq!(recovered.cancel("run").unwrap(), vec![target.clone()]);
    let cancelled = recovered.snapshot();
    assert!(
        recovered
            .finish_native(
                &retried.token,
                &target,
                &receipt(NativeTurnOutcome::Succeeded)
            )
            .is_err()
    );
    assert_eq!(recovered.snapshot(), cancelled);
}

#[rstest]
fn a_failed_publication_keeps_the_prior_running_attempt(fixture: TestResult<Fixture>) {
    let fixture = fixture.unwrap();
    let before = fixture.service.snapshot();
    std::fs::remove_file(fixture.directory.path().join("runs.json")).unwrap();
    std::fs::create_dir(fixture.directory.path().join("runs.json")).unwrap();
    assert!(
        fixture
            .service
            .finish_native(
                &fixture.dispatch.token,
                &fixture.target,
                &receipt(NativeTurnOutcome::Succeeded)
            )
            .is_err()
    );
    assert_eq!(fixture.service.snapshot(), before);
    assert_eq!(fixture.service.claim_ready("run", 32).unwrap(), Vec::new());
}

#[rstest]
fn first_prompt_failure_retains_the_created_tab_and_does_not_start_dependents(
    fixture: TestResult<Fixture>,
) {
    let fixture = fixture.unwrap();
    fixture
        .service
        .finish_native(
            &fixture.dispatch.token,
            &fixture.target,
            &receipt(NativeTurnOutcome::Failed),
        )
        .unwrap();
    fixture.service.retry("run", "a").unwrap();
    let dispatch = fixture
        .service
        .claim_ready("run", 32)
        .unwrap()
        .pop()
        .unwrap();
    let target = CommandTarget {
        generation: 9,
        ..fixture.target
    };
    fixture
        .service
        .reject_created_native(
            &dispatch.token,
            dispatch.context.binding(),
            target.clone(),
            "Provider rejected the first prompt".into(),
        )
        .unwrap();
    assert_eq!(
        fixture.service.snapshot().runs[0].nodes[0].state,
        OrchestrationNodeState::Failed {
            target: Some(target),
            message: "Provider rejected the first prompt".into()
        }
    );
    assert_eq!(fixture.service.claim_ready("run", 32).unwrap(), Vec::new());
}
