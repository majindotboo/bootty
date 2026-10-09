use std::{fs, path::PathBuf};

use bootty_agents::{
    AgentKind, AgentLaunch, AgentPrompt, MAX_RUN_FILE_BYTES, OrchestrationContext,
    OrchestrationDispatch, OrchestrationLaunch, OrchestrationNodeSpec, OrchestrationNodeState,
    OrchestrationOutcome, OrchestrationPlan, OrchestrationService,
};
use bootty_control::{Caller, CommandTarget, ResourceKind};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};
use serde_json::{Value, json};

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

struct RunFixture {
    directory: assert_fs::TempDir,
    path: PathBuf,
    service: OrchestrationService,
}

fn target(kind: ResourceKind, handle: &str, generation: u64) -> CommandTarget {
    CommandTarget {
        kind,
        handle: handle.to_owned(),
        generation,
    }
}

fn context() -> FixtureResult<OrchestrationContext> {
    Ok(OrchestrationContext::capture(
        target(ResourceKind::Binding, "captured-binding", 12),
        Caller::Socket,
    )?)
}

fn node(id: &str, dependencies: Vec<String>) -> FixtureResult<OrchestrationNodeSpec> {
    Ok(OrchestrationNodeSpec {
        id: id.to_owned(),
        title: format!("Task {id}"),
        dependencies,
        prompt: AgentPrompt::new("Literal prompt\n$(never execute as shell)".to_owned())?,
        launch: OrchestrationLaunch::capture(
            AgentKind::Pi,
            Some("captured profile".to_owned()),
            AgentLaunch {
                program: "/captured/provider".to_owned(),
                cwd: Some("/captured/project".to_owned()),
                arguments: vec![
                    "--provider".to_owned(),
                    "captured-model-provider".to_owned(),
                ],
                ephemeral: false,
                account_directory: Some("/captured/account".to_owned()),
            },
        )?,
    })
}

fn plan() -> FixtureResult<OrchestrationPlan> {
    Ok(OrchestrationPlan::new(vec![
        node("root", Vec::new())?,
        node("dependent", vec!["root".to_owned()])?,
    ])?)
}

#[fixture]
fn run_fixture() -> FixtureResult<RunFixture> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.path().join("state/runs.json");
    let service = OrchestrationService::open(&path)?;
    service.create("run".to_owned(), context()?, &plan()?)?;
    Ok(RunFixture {
        directory,
        path,
        service,
    })
}

fn first_dispatch(service: &OrchestrationService) -> FixtureResult<OrchestrationDispatch> {
    service
        .claim_ready("run", 32)?
        .into_iter()
        .next()
        .ok_or_else(|| "Fixture node is not ready".into())
}

fn complete_root(service: &OrchestrationService) -> FixtureResult<OrchestrationDispatch> {
    let dispatch = first_dispatch(service)?;
    let terminal = target(ResourceKind::Terminal, "root-terminal", 2);
    service.accept_terminal(
        &dispatch.token,
        dispatch.context.binding(),
        terminal.clone(),
    )?;
    service.finish(&dispatch.token, &terminal, OrchestrationOutcome::Succeeded)?;
    Ok(dispatch)
}

proptest! {
    #[test]
    fn bounded_dependency_chains_validate_in_either_storage_order(
        count in 1usize..33,
        reverse in any::<bool>(),
    ) {
        let mut nodes: Vec<_> = (0..count).map(|index| {
            let dependencies = index.checked_sub(1).map(|previous| format!("node-{previous}")).into_iter().collect();
            node(&format!("node-{index}"), dependencies).unwrap()
        }).collect();
        if reverse { nodes.reverse(); }
        let accepted = OrchestrationPlan::new(nodes.clone());
        prop_assert_eq!(accepted.unwrap().nodes().len(), count);
        if count > 1 {
            let last = count.checked_sub(1).unwrap();
            nodes.iter_mut().find(|node| node.id == "node-0").unwrap().dependencies.push(format!("node-{last}"));
            prop_assert!(OrchestrationPlan::new(nodes).is_err());
        }
    }
}

#[rstest]
#[case("empty")]
#[case("oversized")]
#[case("duplicate")]
#[case("missing")]
#[case("self")]
#[case("repeated_dependency")]
#[case("bad_id")]
fn plans_reject_invalid_nodes_and_dependency_boundaries(#[case] boundary: &str) {
    let mut nodes = plan().unwrap().nodes().to_vec();
    match boundary {
        "empty" => nodes.clear(),
        "oversized" => {
            nodes = (0..33)
                .map(|index| node(&format!("node-{index}"), Vec::new()).unwrap())
                .collect();
        }
        "duplicate" => nodes[1].id = "root".to_owned(),
        "missing" => nodes[1].dependencies = vec!["missing".to_owned()],
        "self" => nodes[1].dependencies = vec!["dependent".to_owned()],
        "repeated_dependency" => nodes[1].dependencies.push("root".to_owned()),
        _ => nodes[0].id = "../../other".to_owned(),
    }
    assert!(OrchestrationPlan::new(nodes).is_err());
}

#[rstest]
#[case(String::new())]
#[case(" \n\t".to_owned())]
#[case("invalid\0prompt".to_owned())]
#[case("x".repeat(8193))]
fn prompts_preserve_literal_text_and_enforce_the_provider_bound(#[case] invalid: String) {
    assert!(AgentPrompt::new(invalid).is_err());
    let literal = "--help\n$(`literal`)";
    assert_eq!(
        AgentPrompt::new(literal.to_owned()).unwrap().text(),
        literal
    );
    assert!(AgentPrompt::new("x".repeat(8192)).is_ok());
}

#[rstest]
fn dependencies_advance_only_after_trusted_success_for_the_exact_terminal(
    run_fixture: FixtureResult<RunFixture>,
) {
    let fixture = run_fixture.unwrap();
    let dispatch = first_dispatch(&fixture.service).unwrap();
    assert_eq!(dispatch.token.node_id(), "root");
    assert_eq!(dispatch.context, context().unwrap());
    assert_eq!(dispatch.launch, node("root", Vec::new()).unwrap().launch);
    assert_eq!(
        dispatch.prompt.text(),
        "Literal prompt\n$(never execute as shell)"
    );
    assert_eq!(dispatch.context.caller(), Caller::Socket);
    let claimed = fixture.service.snapshot();
    assert_eq!(fixture.service.claim_ready("run", 32).unwrap(), Vec::new());
    assert_eq!(fixture.service.snapshot(), claimed);
    let terminal = target(ResourceKind::Terminal, "accepted-terminal", 3);
    let other_binding = target(ResourceKind::Binding, "other-binding", 12);
    assert!(
        fixture
            .service
            .accept_terminal(&dispatch.token, &other_binding, terminal.clone())
            .is_err()
    );
    fixture
        .service
        .accept_terminal(
            &dispatch.token,
            dispatch.context.binding(),
            terminal.clone(),
        )
        .unwrap();
    assert!(
        fixture
            .service
            .accept_terminal(
                &dispatch.token,
                dispatch.context.binding(),
                terminal.clone()
            )
            .is_err()
    );
    let other_terminal = target(ResourceKind::Terminal, "accepted-terminal", 4);
    assert!(
        fixture
            .service
            .finish(
                &dispatch.token,
                &other_terminal,
                OrchestrationOutcome::Succeeded
            )
            .is_err()
    );
    assert_eq!(fixture.service.claim_ready("run", 1).unwrap(), Vec::new());
    fixture
        .service
        .finish(&dispatch.token, &terminal, OrchestrationOutcome::Succeeded)
        .unwrap();
    assert!(
        fixture
            .service
            .finish(&dispatch.token, &terminal, OrchestrationOutcome::Succeeded)
            .is_err()
    );
    let next = first_dispatch(&fixture.service).unwrap();
    assert_eq!(next.token.node_id(), "dependent");
    assert_eq!(next.token.generation(), dispatch.token.generation());
    assert!(
        fixture
            .service
            .accept_terminal(&next.token, next.context.binding(), terminal)
            .is_err()
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn observed_failure_blocks_dependencies_and_retry_rejects_old_attempts(
    run_fixture: FixtureResult<RunFixture>,
    #[case] launched: bool,
) {
    let fixture = run_fixture.unwrap();
    let first = first_dispatch(&fixture.service).unwrap();
    let terminal = target(ResourceKind::Terminal, "failed-terminal", 7);
    if launched {
        fixture
            .service
            .accept_terminal(&first.token, first.context.binding(), terminal.clone())
            .unwrap();
        fixture
            .service
            .finish(
                &first.token,
                &terminal,
                OrchestrationOutcome::Failed {
                    message: "Provider reported failure".to_owned(),
                },
            )
            .unwrap();
    } else {
        fixture
            .service
            .reject_dispatch(&first.token, "Host launch failed".to_owned())
            .unwrap();
    }
    assert_eq!(fixture.service.claim_ready("run", 32).unwrap(), Vec::new());
    assert!(
        fixture
            .service
            .reject_dispatch(&first.token, "Duplicate failure".to_owned())
            .is_err()
    );
    fixture.service.retry("run", "root").unwrap();
    let second = first_dispatch(&fixture.service).unwrap();
    assert_eq!(
        second.token.attempt(),
        first.token.attempt().saturating_add(1)
    );
    assert!(
        fixture
            .service
            .accept_terminal(&first.token, first.context.binding(), terminal.clone())
            .is_err()
    );
    assert!(
        fixture
            .service
            .finish(&first.token, &terminal, OrchestrationOutcome::Succeeded)
            .is_err()
    );
    fixture
        .service
        .accept_terminal(&second.token, second.context.binding(), terminal.clone())
        .unwrap();
    fixture
        .service
        .finish(&second.token, &terminal, OrchestrationOutcome::Succeeded)
        .unwrap();
    assert_eq!(
        first_dispatch(&fixture.service).unwrap().token.node_id(),
        "dependent"
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn reopening_persists_interruption_and_requires_explicit_retry(
    run_fixture: FixtureResult<RunFixture>,
    #[case] launched: bool,
) {
    let fixture = run_fixture.unwrap();
    let first = first_dispatch(&fixture.service).unwrap();
    let terminal = target(ResourceKind::Terminal, "interrupted-terminal", 5);
    if launched {
        fixture
            .service
            .accept_terminal(&first.token, first.context.binding(), terminal.clone())
            .unwrap();
    }
    let previous = fixture.service.snapshot();
    drop(fixture.service);
    let recovered = OrchestrationService::open(&fixture.path).unwrap();
    let snapshot = recovered.snapshot();
    assert_eq!(snapshot.revision, previous.revision.saturating_add(1));
    assert_eq!(
        snapshot.runs[0].generation,
        first.token.generation().saturating_add(1)
    );
    assert_eq!(snapshot.runs[0].context, first.context);
    assert_eq!(snapshot.runs[0].nodes[0].spec.launch, first.launch);
    assert_eq!(
        snapshot.runs[0].nodes[0].state,
        OrchestrationNodeState::Interrupted {
            target: launched.then_some(terminal.clone())
        }
    );
    assert_eq!(recovered.claim_ready("run", 32).unwrap(), Vec::new());
    assert!(
        recovered
            .finish(&first.token, &terminal, OrchestrationOutcome::Succeeded)
            .is_err()
    );
    assert_eq!(
        OrchestrationService::open(&fixture.path)
            .unwrap()
            .snapshot(),
        snapshot
    );
    recovered.retry("run", "root").unwrap();
    let retried = first_dispatch(&recovered).unwrap();
    assert_eq!(
        retried.token.attempt(),
        first.token.attempt().saturating_add(1)
    );
    assert!(
        recovered
            .accept_terminal(&first.token, first.context.binding(), terminal)
            .is_err()
    );
}

#[rstest]
fn cancellation_and_restart_preserve_success_and_invalidate_live_dispatches(
    run_fixture: FixtureResult<RunFixture>,
) {
    let fixture = run_fixture.unwrap();
    complete_root(&fixture.service).unwrap();
    let second = first_dispatch(&fixture.service).unwrap();
    let terminal = target(ResourceKind::Terminal, "dependent-terminal", 8);
    fixture
        .service
        .accept_terminal(&second.token, second.context.binding(), terminal.clone())
        .unwrap();
    assert!(fixture.service.restart("run").is_err());
    assert_eq!(
        fixture.service.cancel("run").unwrap(),
        vec![terminal.clone()]
    );
    assert!(fixture.service.claim_ready("run", 32).is_err());
    assert!(
        fixture
            .service
            .finish(&second.token, &terminal, OrchestrationOutcome::Succeeded)
            .is_err()
    );
    assert!(fixture.service.cancel("run").is_err());
    drop(fixture.service);
    let recovered = OrchestrationService::open(&fixture.path).unwrap();
    assert!(recovered.snapshot().runs[0].cancelled);
    recovered.restart("run").unwrap();
    let next = first_dispatch(&recovered).unwrap();
    assert_eq!(next.token.node_id(), "dependent");
    assert_eq!(
        next.token.attempt(),
        second.token.attempt().saturating_add(1)
    );
    assert!(next.token.generation() > second.token.generation());
    assert!(
        recovered
            .accept_terminal(&second.token, second.context.binding(), terminal)
            .is_err()
    );
    assert!(matches!(
        recovered.snapshot().runs[0].nodes[0].state,
        OrchestrationNodeState::Succeeded { .. }
    ));
}

#[rstest]
fn cancelling_pending_nodes_roundtrips_and_retry_is_explicit(
    run_fixture: FixtureResult<RunFixture>,
) {
    let fixture = run_fixture.unwrap();
    assert_eq!(fixture.service.cancel("run").unwrap(), Vec::new());
    let cancelled = fixture.service.snapshot();
    drop(fixture.service);
    let recovered = OrchestrationService::open(&fixture.path).unwrap();
    assert_eq!(recovered.snapshot(), cancelled);
    recovered.retry("run", "root").unwrap();
    assert_eq!(first_dispatch(&recovered).unwrap().token.node_id(), "root");
}

#[rstest]
#[case("create")]
#[case("claim")]
#[case("accept")]
#[case("finish")]
#[case("cancel")]
#[case("retry")]
#[case("restart")]
fn failed_commits_never_publish_state_or_dispatch_tokens(
    run_fixture: FixtureResult<RunFixture>,
    #[case] mutation: &str,
) {
    let fixture = run_fixture.unwrap();
    let dispatch = if matches!(mutation, "accept" | "finish" | "retry") {
        Some(first_dispatch(&fixture.service).unwrap())
    } else {
        None
    };
    let terminal = target(ResourceKind::Terminal, "persisted-terminal", 10);
    if mutation == "finish" {
        let dispatch = dispatch.as_ref().unwrap();
        fixture
            .service
            .accept_terminal(
                &dispatch.token,
                dispatch.context.binding(),
                terminal.clone(),
            )
            .unwrap();
    }
    if mutation == "retry" {
        fixture
            .service
            .reject_dispatch(
                &dispatch.as_ref().unwrap().token,
                "Observed launch failure".to_owned(),
            )
            .unwrap();
    }
    let before = fixture.service.snapshot();
    let bytes = fs::read(&fixture.path).unwrap();
    let parent = fixture.path.parent().unwrap();
    let held = fixture.directory.path().join("held-state");
    fs::rename(parent, &held).unwrap();
    fs::write(parent, "Injected unavailable parent").unwrap();
    let failed = match mutation {
        "create" => fixture
            .service
            .create("other-run".to_owned(), context().unwrap(), &plan().unwrap())
            .map(|_| ()),
        "claim" => fixture.service.claim_ready("run", 1).map(|_| ()),
        "accept" => {
            let dispatch = dispatch.as_ref().unwrap();
            fixture
                .service
                .accept_terminal(&dispatch.token, dispatch.context.binding(), terminal)
        }
        "finish" => fixture.service.finish(
            &dispatch.as_ref().unwrap().token,
            &terminal,
            OrchestrationOutcome::Succeeded,
        ),
        "cancel" => fixture.service.cancel("run").map(|_| ()),
        "retry" => fixture.service.retry("run", "root"),
        _ => fixture.service.restart("run"),
    };
    fs::remove_file(parent).unwrap();
    fs::rename(held, parent).unwrap();
    assert!(failed.is_err());
    assert_eq!(fixture.service.snapshot(), before);
    assert_eq!(fs::read(&fixture.path).unwrap(), bytes);
}

#[rstest]
#[case("binding")]
#[case("binding_generation")]
#[case("account")]
#[case("prompt")]
#[case("cycle")]
#[case("attempt")]
#[case("terminal")]
#[case("dependency_lifecycle")]
#[case("version")]
#[case("unknown_field")]
#[case("too_many_runs")]
fn restore_revalidates_captured_authority_bounds_and_lifecycle(
    run_fixture: FixtureResult<RunFixture>,
    #[case] invalid: &str,
) {
    let fixture = run_fixture.unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&fixture.path).unwrap()).unwrap();
    match invalid {
        "binding" => saved["runs"][0]["context"]["binding"]["kind"] = json!("terminal"),
        "binding_generation" => saved["runs"][0]["context"]["binding"]["generation"] = json!("0"),
        "account" => {
            saved["runs"][0]["nodes"][0]["spec"]["launch"]["launch"]["account_directory"] =
                json!("relative/account");
        }
        "prompt" => saved["runs"][0]["nodes"][0]["spec"]["prompt"] = json!("x".repeat(8193)),
        "cycle" => saved["runs"][0]["nodes"][0]["spec"]["dependencies"] = json!(["dependent"]),
        "attempt" => saved["runs"][0]["nodes"][0]["state"] = json!({"state":"claimed"}),
        "terminal" => {
            saved["runs"][0]["nodes"][0]["attempt"] = json!(1);
            saved["runs"][0]["nodes"][0]["state"] =
                json!({"state":"running","target":target(ResourceKind::Binding, "wrong-kind", 1)});
        }
        "dependency_lifecycle" => {
            saved["runs"][0]["nodes"][1]["attempt"] = json!(1);
            saved["runs"][0]["nodes"][1]["state"] = json!({"state":"claimed"});
        }
        "version" => saved["version"] = json!(2),
        "unknown_field" => saved["runs"][0]["context"]["widen_identity"] = json!("other"),
        _ => saved["runs"] = json!(vec![saved["runs"][0].clone(); 65]),
    }
    let bytes = serde_json::to_vec(&saved).unwrap();
    fs::write(&fixture.path, &bytes).unwrap();
    drop(fixture.service);
    assert!(OrchestrationService::open(&fixture.path).is_err());
    assert_eq!(fs::read(&fixture.path).unwrap(), bytes);
}

#[rstest]
fn exhausted_recovery_generation_fails_without_publishing_or_rewriting(
    run_fixture: FixtureResult<RunFixture>,
) {
    let fixture = run_fixture.unwrap();
    first_dispatch(&fixture.service).unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&fixture.path).unwrap()).unwrap();
    saved["runs"][0]["generation"] = json!(u64::MAX);
    let bytes = serde_json::to_vec(&saved).unwrap();
    fs::write(&fixture.path, &bytes).unwrap();
    drop(fixture.service);
    assert!(OrchestrationService::open(&fixture.path).is_err());
    assert_eq!(fs::read(&fixture.path).unwrap(), bytes);
}

#[rstest]
fn aggregate_file_limit_rejects_commit_without_publishing_the_candidate() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("runs.json");
    let service = OrchestrationService::open(&path).unwrap();
    let nodes = (0..32)
        .map(|index| {
            let mut node = node(&format!("large-{index}"), Vec::new()).unwrap();
            let mut launch = node.launch.launch().clone();
            launch.arguments = vec!["a".repeat(8192); 7];
            node.launch = OrchestrationLaunch::capture(AgentKind::Pi, None, launch).unwrap();
            node.prompt = AgentPrompt::new("p".repeat(8192)).unwrap();
            node
        })
        .collect();
    let plan = OrchestrationPlan::new(nodes).unwrap();
    service
        .create("first".to_owned(), context().unwrap(), &plan)
        .unwrap();
    let before = service.snapshot();
    let bytes = fs::read(&path).unwrap();
    assert!(
        service
            .create("second".to_owned(), context().unwrap(), &plan)
            .is_err()
    );
    assert_eq!(service.snapshot(), before);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let oversized = vec![
        b' ';
        usize::try_from(MAX_RUN_FILE_BYTES)
            .unwrap()
            .saturating_add(1)
    ];
    fs::write(&path, oversized).unwrap();
    assert!(OrchestrationService::open(&path).is_err());
}

#[rstest]
fn invalid_host_targets_and_launch_preferences_cannot_enter_a_plan() {
    assert!(
        OrchestrationContext::capture(
            target(ResourceKind::Terminal, "terminal", 1),
            Caller::Internal
        )
        .is_err()
    );
    assert!(
        OrchestrationContext::capture(
            target(ResourceKind::Binding, "binding", 0),
            Caller::Internal
        )
        .is_err()
    );
    let captured = node("root", Vec::new()).unwrap().launch;
    let mut launch = captured.launch().clone();
    launch.account_directory = Some("relative/account".to_owned());
    assert!(OrchestrationLaunch::capture(AgentKind::Pi, None, launch).is_err());
    assert!(
        OrchestrationLaunch::capture(
            AgentKind::Pi,
            Some("x".repeat(257)),
            captured.launch().clone()
        )
        .is_err()
    );
}

#[rstest]
fn concurrent_dispatch_claims_publish_each_ready_attempt_once(
    run_fixture: FixtureResult<RunFixture>,
) {
    let fixture = run_fixture.unwrap();
    let service = std::sync::Arc::new(fixture.service);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let previous_revision = service.snapshot().revision;
    let workers: [_; 2] = std::array::from_fn(|_| {
        let service = std::sync::Arc::clone(&service);
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            service.claim_ready("run", 32)
        })
    });
    barrier.wait();
    let dispatches: Vec<_> = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap().unwrap())
        .collect();
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0].token.node_id(), "root");
    assert_eq!(dispatches[0].token.attempt(), 1);
    assert_eq!(
        service.snapshot().revision,
        previous_revision.saturating_add(1)
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn cancellation_reaches_the_final_gate_and_preserves_already_accepted_ids(
    run_fixture: FixtureResult<RunFixture>,
    #[case] accepted_before_cancel: bool,
) {
    let fixture = run_fixture.unwrap();
    let service = std::sync::Arc::new(fixture.service);
    let dispatch = first_dispatch(&service).unwrap();
    let guard = service.begin_dispatch(&dispatch.token).unwrap();
    assert!(service.begin_dispatch(&dispatch.token).is_err());
    let pending = guard.cancellation();
    let terminal = target(ResourceKind::Terminal, "accepted-created-id", 24);
    let created = terminal.clone();
    let (start, start_requested) = std::sync::mpsc::channel();
    let (admitted, admission) = std::sync::mpsc::channel();
    let (finish, finish_requested) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        start_requested.recv().unwrap();
        let accepted = pending.try_start();
        admitted.send(accepted).unwrap();
        finish_requested.recv().unwrap();
        accepted.then_some(created)
    });
    if accepted_before_cancel {
        start.send(()).unwrap();
        assert!(admission.recv().unwrap());
    }
    assert_eq!(service.cancel("run").unwrap(), Vec::new());
    if !accepted_before_cancel {
        start.send(()).unwrap();
        assert!(!admission.recv().unwrap());
    }
    finish.send(()).unwrap();
    let observed_ids = worker.join().unwrap();
    assert_eq!(
        observed_ids,
        accepted_before_cancel.then_some(terminal.clone())
    );
    assert!(
        service
            .accept_terminal(&dispatch.token, dispatch.context.binding(), terminal)
            .is_err()
    );
    drop(guard);
}

#[rstest]
fn failed_cancellation_commit_leaves_the_registered_owner_token_active(
    run_fixture: FixtureResult<RunFixture>,
) {
    let fixture = run_fixture.unwrap();
    let service = std::sync::Arc::new(fixture.service);
    let dispatch = first_dispatch(&service).unwrap();
    let guard = service.begin_dispatch(&dispatch.token).unwrap();
    let before = service.snapshot();
    let parent = fixture.path.parent().unwrap();
    let held = fixture.directory.path().join("held-state");
    fs::rename(parent, &held).unwrap();
    fs::write(parent, "Injected unavailable parent").unwrap();
    let cancelled = service.cancel("run");
    fs::remove_file(parent).unwrap();
    fs::rename(held, parent).unwrap();
    assert!(cancelled.is_err());
    assert_eq!(service.snapshot(), before);
    assert!(!guard.cancellation().is_cancelled());
    assert!(guard.cancellation().try_start());
}

#[rstest]
fn abandoned_pending_guards_and_owner_shutdown_cancel_without_replaying(
    run_fixture: FixtureResult<RunFixture>,
) {
    let fixture = run_fixture.unwrap();
    let service = std::sync::Arc::new(fixture.service);
    let dispatch = first_dispatch(&service).unwrap();
    let guard = service.begin_dispatch(&dispatch.token).unwrap();
    let abandoned = guard.cancellation();
    drop(guard);
    assert!(!abandoned.try_start());
    let guard = service.begin_dispatch(&dispatch.token).unwrap();
    let pending = guard.cancellation();
    drop(service);
    assert!(!pending.try_start());
    let recovered = OrchestrationService::open(&fixture.path).unwrap();
    assert_eq!(recovered.claim_ready("run", 32).unwrap(), Vec::new());
    assert_eq!(recovered.running_dispatches(), Vec::new());
}

#[rstest]
#[case("same", true)]
#[case("window", false)]
#[case("binding", false)]
#[case("caller", false)]
#[case("generation", false)]
#[case("live", false)]
#[case("absent", false)]
fn explicit_recovery_rebinds_only_original_durable_destination(
    #[case] boundary: &str,
    #[case] accepted: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("runs.json");
    let captured = if boundary == "absent" {
        context().unwrap()
    } else {
        OrchestrationContext::capture_destination(
            target(ResourceKind::Binding, "old-process-window", 12),
            Caller::Socket,
            "main".to_owned(),
            "space-a".to_owned(),
        )
        .unwrap()
    };
    let service = OrchestrationService::open(&path).unwrap();
    service
        .create("run".to_owned(), captured, &plan().unwrap())
        .unwrap();
    first_dispatch(&service).unwrap();
    drop(service);
    let recovered = OrchestrationService::open(&path).unwrap();
    if boundary == "live" {
        recovered.retry("run", "root").unwrap();
        first_dispatch(&recovered).unwrap();
    }
    let before = recovered.snapshot();
    let bytes = fs::read(&path).unwrap();
    let run = &before.runs[0];
    let context = OrchestrationContext::capture_destination(
        target(ResourceKind::Binding, "new-process-window", 19),
        if boundary == "caller" {
            Caller::Internal
        } else {
            Caller::Socket
        },
        if boundary == "window" {
            "other"
        } else {
            "main"
        }
        .to_owned(),
        if boundary == "binding" {
            "space-b"
        } else {
            "space-a"
        }
        .to_owned(),
    )
    .unwrap();
    let generation = if boundary == "generation" {
        run.generation.saturating_sub(1)
    } else {
        run.generation
    };
    let result = recovered.restart_on_binding("run", generation, context);
    assert_eq!(result.is_ok(), accepted);
    if accepted {
        let published = recovered.snapshot();
        assert_eq!(
            published.runs[0].context.binding().handle,
            "new-process-window"
        );
        assert_eq!(published.runs[0].context.caller(), Caller::Socket);
        assert_eq!(
            published.runs[0].nodes[0].spec.launch,
            run.nodes[0].spec.launch
        );
        assert_eq!(
            published.runs[0].nodes[0].state,
            OrchestrationNodeState::Pending
        );
        assert!(published.runs[0].generation > run.generation);
        drop(recovered);
        assert_eq!(
            OrchestrationService::open(&path).unwrap().snapshot(),
            published
        );
    } else {
        assert_eq!(recovered.snapshot(), before);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}
