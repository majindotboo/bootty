use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bootty_agents::{AgentCommandExecutor, AgentInvocation, OrchestrationService};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};
use serde_json::Value;

#[fixture]
fn directory() -> Result<assert_fs::TempDir, String> {
    assert_fs::TempDir::new().map_err(|error| error.to_string())
}

fn request(command: &str, args: &[&str], target: Option<CommandTarget>) -> AgentInvocation {
    let mut invocation = CommandInvocation::new(
        format!("orchestration.{command}"),
        args.iter().map(|arg| (*arg).to_owned()).collect(),
        Caller::Cli,
    );
    invocation.target = target;
    let target_supplied = invocation.target.is_some();
    let now = Instant::now();
    AgentInvocation::new(
        invocation,
        target_supplied,
        None,
        now.checked_add(Duration::from_secs(30)).unwrap_or(now),
        CommandCancellation::new(),
    )
}

fn target(kind: ResourceKind, generation: u64) -> CommandTarget {
    CommandTarget {
        kind,
        handle: "native:codex:session-1".to_owned(),
        generation,
    }
}

fn value(outcome: CommandOutcome) -> Result<Value, String> {
    match outcome {
        CommandOutcome::Success { value, .. } => Ok(value),
        outcome => Err(format!("expected success: {outcome:?}")),
    }
}

fn id(outcome: CommandOutcome, entity: &str) -> Result<String, String> {
    value(outcome)?
        .get(entity)
        .and_then(|entity| entity.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing {entity} id"))
}

fn setup(
    service: &OrchestrationService,
    target: &CommandTarget,
) -> Result<(String, String, String), String> {
    let run = id(
        service.invoke(&request(
            "run.create",
            &["Project", "Finish the change"],
            None,
        )),
        "run",
    )?;
    let task = id(
        service.invoke(&request(
            "task.create",
            &[&run, "Implementation", "Implement the requested change"],
            None,
        )),
        "task",
    )?;
    let worker = id(
        service.invoke(&request(
            "worker.attach",
            &[&run, "Builder", "codex"],
            Some(target.clone()),
        )),
        "worker",
    )?;
    Ok((run, task, worker))
}

#[rstest]
#[case(ResourceKind::Session)]
#[case(ResourceKind::Terminal)]
fn task_delivery_commits_before_using_exact_session(
    directory: Result<assert_fs::TempDir, String>,
    #[case] kind: ResourceKind,
) {
    let directory = directory.unwrap();
    let path = directory.path().join("coordination.json");
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let invocations = Arc::clone(&recorded);
    let saved = path.clone();
    let executor: Arc<dyn AgentCommandExecutor> =
        Arc::new(move |invocation: CommandInvocation, _, _| {
            let state: Value = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
            assert_eq!(state["runs"][0]["tasks"][0]["state"], "dispatching");
            invocations.lock().unwrap().push(invocation);
            CommandOutcome::success()
        });
    let service = OrchestrationService::open(&path, executor)
        .unwrap()
        .with_cli("/tmp/Bootty Dev.app/Contents/MacOS/bootty-dev");
    let original = target(kind, 9);
    let (run, task, worker) = setup(&service, &original).unwrap();
    let dispatched =
        value(service.invoke(&request("task.dispatch", &[&run, &task, &worker], None))).unwrap();
    assert_eq!(dispatched["task"]["state"], "running");
    let invocation = recorded.lock().unwrap()[0].clone();
    assert_eq!(invocation.command, "agents.codex.prompt");
    assert_eq!(invocation.target, Some(original.clone()));
    assert!(invocation.arguments[0].contains("Implement the requested change"));
    assert!(invocation.arguments[0].contains("/tmp/Bootty Dev.app/Contents/MacOS/bootty-dev"));
    assert!(invocation.arguments[0].contains("native:codex:session-1@9"));
    assert!(matches!(
        service.invoke(&request("run.finish", &[&run], None)),
        CommandOutcome::Failed { .. }
    ));
    let dispatch = dispatched["task"]["dispatch"].as_str().unwrap();
    value(service.invoke(&request(
        "task.complete",
        &[&run, &task, dispatch, "Verified the change"],
        Some(original),
    )))
    .unwrap();
    assert_eq!(
        value(service.invoke(&request("run.finish", &[&run], None))).unwrap()["run"]["finished"],
        true
    );
}

#[rstest]
#[case(CommandOutcome::StaleTarget { message: "session generation changed".to_owned() })]
#[case(CommandOutcome::Unavailable { message: "provider disconnected".to_owned() })]
#[case(CommandOutcome::Unsupported { message: "prompt is unsupported".to_owned() })]
#[case(CommandOutcome::Failed { code: "transport_error".to_owned(), message: "connection closed".to_owned() })]
fn failures_are_retained_and_require_explicit_retry(
    directory: Result<assert_fs::TempDir, String>,
    #[case] outcome: CommandOutcome,
) {
    let directory = directory.unwrap();
    let expected = outcome.clone();
    let service = OrchestrationService::open(
        &directory.path().join("coordination.json"),
        Arc::new(move |_, _, _| outcome.clone()),
    )
    .unwrap();
    let (run, task, worker) = setup(&service, &target(ResourceKind::Session, 9)).unwrap();
    assert_eq!(
        service.invoke(&request("task.dispatch", &[&run, &task, &worker], None)),
        expected
    );
    let snapshot = value(service.invoke(&request("run.show", &[&run], None))).unwrap();
    assert_eq!(snapshot["run"]["tasks"][0]["state"], "failed");
    assert_eq!(
        snapshot["run"]["tasks"][0]["outcome"],
        serde_json::to_value(expected).unwrap()
    );
    assert!(matches!(
        service.invoke(&request("task.dispatch", &[&run, &task, &worker], None)),
        CommandOutcome::Failed { .. }
    ));
    assert_eq!(
        value(service.invoke(&request("task.retry", &[&run, &task], None))).unwrap()["task"]["state"],
        "pending"
    );
}

#[rstest]
fn restart_keeps_history_and_interrupts_unproven_work(
    directory: Result<assert_fs::TempDir, String>,
) {
    let directory = directory.unwrap();
    let path = directory.path().join("coordination.json");
    let executor: Arc<dyn AgentCommandExecutor> = Arc::new(|_, _, _| CommandOutcome::success());
    let service = OrchestrationService::open(&path, Arc::clone(&executor)).unwrap();
    let original = target(ResourceKind::Session, 9);
    let (run, task, worker) = setup(&service, &original).unwrap();
    let dispatched =
        value(service.invoke(&request("task.dispatch", &[&run, &task, &worker], None))).unwrap();
    let old_dispatch = dispatched["task"]["dispatch"].as_str().unwrap();
    drop(service);
    let service = OrchestrationService::open(&path, executor).unwrap();
    let snapshot = value(service.invoke(&request("run.show", &[&run], None))).unwrap();
    assert_eq!(snapshot["run"]["tasks"][0]["state"], "interrupted");
    assert_eq!(
        snapshot["run"]["workers"][0]["target"],
        serde_json::to_value(&original).unwrap()
    );
    value(service.invoke(&request("task.retry", &[&run, &task], None))).unwrap();
    let replaced = value(service.invoke(&request(
        "worker.attach",
        &[&run, "Builder", "codex"],
        Some(target(ResourceKind::Session, 10)),
    )))
    .unwrap();
    assert_eq!(replaced["worker"]["id"], worker);
    let current =
        value(service.invoke(&request("task.dispatch", &[&run, &task, &worker], None))).unwrap();
    assert_ne!(current["task"]["dispatch"], old_dispatch);
    assert!(matches!(
        service.invoke(&request(
            "task.complete",
            &[&run, &task, old_dispatch, "Old report"],
            Some(original)
        )),
        CommandOutcome::Failed { .. }
    ));
}

#[rstest]
fn failed_commit_leaves_live_state_unchanged_and_never_dispatches(
    directory: Result<assert_fs::TempDir, String>,
) {
    let directory = directory.unwrap();
    let parent = directory.path().join("state");
    let path = parent.join("coordination.json");
    let deliveries = Arc::new(Mutex::new(0_u32));
    let delivered = Arc::clone(&deliveries);
    let service = OrchestrationService::open(
        &path,
        Arc::new(move |_, _, _| {
            let mut count = delivered.lock().unwrap();
            *count = count.saturating_add(1);
            drop(count);
            CommandOutcome::success()
        }),
    )
    .unwrap();
    let (run, task, worker) = setup(&service, &target(ResourceKind::Session, 9)).unwrap();
    let before = serde_json::to_value(service.runs().unwrap()).unwrap();
    std::fs::remove_dir_all(&parent).unwrap();
    std::fs::write(&parent, "blocks the state directory").unwrap();
    assert!(
        matches!(service.invoke(&request("task.dispatch", &[&run, &task, &worker], None)), CommandOutcome::Failed { code, .. } if code == "persistence_failed")
    );
    assert_eq!(
        serde_json::to_value(service.runs().unwrap()).unwrap(),
        before
    );
    assert_eq!(*deliveries.lock().unwrap(), 0);
}

#[rstest]
fn accepted_delivery_with_failed_result_commit_remains_unproven(
    directory: Result<assert_fs::TempDir, String>,
) {
    let directory = directory.unwrap();
    let parent = directory.path().join("state");
    let path = parent.join("coordination.json");
    let deliveries = Arc::new(Mutex::new(0_u32));
    let delivered = Arc::clone(&deliveries);
    let obstruction = parent;
    let service = OrchestrationService::open(
        &path,
        Arc::new(move |_, _, _| {
            let mut count = delivered.lock().unwrap();
            *count = count.saturating_add(1);
            drop(count);
            std::fs::remove_dir_all(&obstruction).unwrap();
            std::fs::write(&obstruction, "blocks the result commit").unwrap();
            CommandOutcome::success()
        }),
    )
    .unwrap();
    let (run, task, worker) = setup(&service, &target(ResourceKind::Session, 9)).unwrap();
    assert!(
        matches!(service.invoke(&request("task.dispatch", &[&run, &task, &worker], None)), CommandOutcome::Failed { code, .. } if code == "delivery_unrecorded")
    );
    assert_eq!(*deliveries.lock().unwrap(), 1);
    assert_eq!(
        value(service.invoke(&request("run.show", &[&run], None))).unwrap()["run"]["tasks"][0]["state"],
        "dispatching"
    );
}

#[rstest]
fn messages_record_delivery_and_preserve_inbox_across_restart(
    directory: Result<assert_fs::TempDir, String>,
) {
    let directory = directory.unwrap();
    let path = directory.path().join("coordination.json");
    let executor: Arc<dyn AgentCommandExecutor> = Arc::new(|_, _, _| CommandOutcome::success());
    let service = OrchestrationService::open(&path, Arc::clone(&executor)).unwrap();
    let (run, _, worker) = setup(&service, &target(ResourceKind::Session, 9)).unwrap();
    let delivered = value(service.invoke(&request(
        "message.send",
        &[&run, &worker, "Review the diff before completing"],
        None,
    )))
    .unwrap();
    assert_eq!(delivered["message"]["state"], "delivered");
    drop(service);
    let service = OrchestrationService::open(&path, executor).unwrap();
    assert_eq!(
        value(service.invoke(&request("message.inbox", &[&run, &worker], None))).unwrap()["messages"]
            [0],
        delivered["message"]
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn changed_session_generation_cannot_complete_work(generation in 10_u64..u64::MAX, wrong_dispatch in "[a-z]{1,12}") {
        let directory = assert_fs::TempDir::new().unwrap();
        let service = OrchestrationService::open(&directory.path().join("coordination.json"), Arc::new(|_, _, _| CommandOutcome::success())).unwrap();
        let original = target(ResourceKind::Session, 9);
        let (run, task, worker) = setup(&service, &original).unwrap();
        let dispatch = value(service.invoke(&request("task.dispatch", &[&run, &task, &worker], None))).unwrap()["task"]["dispatch"].as_str().unwrap().to_owned();
        let before = serde_json::to_value(service.runs().unwrap()).unwrap();
        for (report_target, report_dispatch) in [(target(ResourceKind::Session, generation), dispatch), (original, wrong_dispatch)] {
            prop_assert!(matches!(service.invoke(&request("task.complete", &[&run, &task, &report_dispatch, "completed"], Some(report_target))), CommandOutcome::Failed { .. }), "report with a changed generation or attempt must fail");
            assert_eq!(serde_json::to_value(service.runs().unwrap()).unwrap(), before);
        }
    }
}

#[rstest]
#[case("{invalid JSON")]
#[case("{\"version\":2,\"next_id\":1,\"runs\":[]}")]
#[case("{\"version\":1,\"next_id\":0,\"runs\":[]}")]
#[case(
    "{\"version\":1,\"next_id\":1,\"runs\":[{\"id\":\"run-1\",\"title\":\"Saved\",\"goal\":\"Goal\",\"finished\":false,\"tasks\":[],\"workers\":[],\"messages\":[]}]}"
)]
fn invalid_state_is_reported_without_overwriting_it(
    directory: Result<assert_fs::TempDir, String>,
    #[case] saved: &str,
) {
    let directory = directory.unwrap();
    let path = directory.path().join("coordination.json");
    std::fs::write(&path, saved).unwrap();
    assert!(
        OrchestrationService::open(&path, Arc::new(|_, _, _| CommandOutcome::success())).is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
}

#[rstest]
fn cancelled_mutations_leave_no_published_state(directory: Result<assert_fs::TempDir, String>) {
    let directory = directory.unwrap();
    let path = directory.path().join("coordination.json");
    let service =
        OrchestrationService::open(&path, Arc::new(|_, _, _| CommandOutcome::success())).unwrap();
    let request = request("run.create", &["Title", "Goal"], None);
    assert!(request.cancellation.cancel());
    assert_eq!(service.invoke(&request), CommandOutcome::cancelled());
    assert!(service.runs().unwrap().is_empty());
    assert!(!Path::new(&path).exists());
}
