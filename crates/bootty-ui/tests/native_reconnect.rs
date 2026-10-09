#![cfg(test)]

//! Recovery retains the captured conversation and retries without replaying a prompt.
use bootty_agents::{AgentKind, NativeSessionConfig, NativeSessionRecord, NativeSessionStatus};
use bootty_ui::presentation::native_reconnect::{NativeReconnect, NativeReconnectStep};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};
use std::time::{Duration, Instant};

#[fixture]
fn record() -> NativeSessionRecord {
    let mut record = NativeSessionRecord {
        id: "captured-conversation".into(),
        binding_id: "captured-binding".into(),
        task_identity: Some("captured-task".into()),
        title: "Review".into(),
        pending_initial_message: None,
        permissions_pending: false,
        generation: 7,
        config: NativeSessionConfig::new(AgentKind::Codex, "/remote/project"),
        snapshot: serde_json::from_value(serde_json::json!({
            "provider": "codex", "session_id": "captured-provider-uuid",
            "turn_id": null, "status": "error", "transcript": [], "requests": [],
            "usage": null, "error": "remote output ended", "revision": 1
        }))
        .unwrap_or_else(|error| panic!("public saved conversation: {error}")),
        side_chat: None,
        spawn_parent: None,
        attachments: Vec::new(),
    };
    record.snapshot.transport_lost = true;
    record
}

#[rstest]
fn recovery_retries_exact_current_targets_with_bounded_backoff(mut record: NativeSessionRecord) {
    let mut recovery = NativeReconnect::default();
    let mut now = Instant::now();
    for millis in [500, 1000, 2000, 4000, 8000, 8000] {
        let NativeReconnectStep::Resume(target) = recovery.poll(&record, now) else {
            panic!("lost transport should resume its captured conversation");
        };
        assert_eq!(target, record.target());
        let NativeReconnectStep::Wait(remaining) = recovery.poll(&record, now) else {
            panic!("a retry must not be submitted twice");
        };
        assert_eq!(remaining, Duration::from_millis(millis));
        record.generation = record.generation.saturating_add(1);
        now = now
            .checked_add(remaining)
            .unwrap_or_else(|| panic!("test deadline overflow"));
    }
}

#[rstest]
#[case(NativeSessionStatus::Idle, false)]
#[case(NativeSessionStatus::Error, false)]
#[case(NativeSessionStatus::Stopped, false)]
#[case(NativeSessionStatus::Starting, true)]
#[case(NativeSessionStatus::Working, true)]
fn recovery_only_resumes_an_observed_transport_loss(
    mut record: NativeSessionRecord,
    #[case] status: NativeSessionStatus,
    #[case] lost: bool,
) {
    record.snapshot.status = status;
    record.snapshot.transport_lost = lost;
    assert!(matches!(
        NativeReconnect::default().poll(&record, Instant::now()),
        NativeReconnectStep::Idle
    ));
}

#[rstest]
fn healthy_or_different_conversations_clear_the_retry_delay(mut record: NativeSessionRecord) {
    let mut recovery = NativeReconnect::default();
    let now = Instant::now();
    assert!(matches!(
        recovery.poll(&record, now),
        NativeReconnectStep::Resume(_)
    ));
    record.snapshot.transport_lost = false;
    record.snapshot.status = NativeSessionStatus::Idle;
    assert!(matches!(
        recovery.poll(&record, now),
        NativeReconnectStep::Idle
    ));
    record.snapshot.transport_lost = true;
    record.snapshot.status = NativeSessionStatus::Error;
    assert!(matches!(
        recovery.poll(&record, now),
        NativeReconnectStep::Resume(_)
    ));
    record.id = "another-captured-conversation".into();
    let NativeReconnectStep::Resume(target) = recovery.poll(&record, now) else {
        panic!("another conversation must not inherit the prior retry delay");
    };
    assert_eq!(target, record.target());
}
