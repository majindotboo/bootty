use bootty_agents::{AgentAttention, AgentInvocation, AgentKind, AgentService};
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct Events;
impl bootty_agents::AgentEventPublisher for Events {
    fn publish(
        &self,
        _: &str,
        _: u64,
        _: &str,
        _: serde_json::Value,
        _: Instant,
        _: &CommandCancellation,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[rstest]
#[case(AgentKind::Pi, "agent_start", "agent_settled", "type")]
#[case(AgentKind::Codex, "UserPromptSubmit", "Stop", "hook_event_name")]
#[case(AgentKind::Claude, "UserPromptSubmit", "Stop", "hook_event_name")]
fn acknowledgements_do_not_hide_newer_attention(
    #[case] provider: AgentKind,
    #[case] start: &str,
    #[case] stop: &str,
    #[case] field: &str,
) {
    let service = AgentService::new(
        Arc::new(|_, _, _| CommandOutcome::success()),
        Arc::new(Events),
    );
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    let cancellation = CommandCancellation::new();
    let ingest = |event| {
        service.ingest_scoped(
            provider,
            Some("host-a"),
            Some("pane"),
            json!({field: event}),
            deadline,
            &cancellation,
        )
    };
    ingest(start);
    assert!(
        !service
            .snapshot_scoped(provider, Some("host-a"), Some("pane"))
            .unread()
    );
    ingest(stop);
    let first = service.snapshot_scoped(provider, Some("host-a"), Some("pane"));
    assert_eq!(first.attention, Some(AgentAttention::Complete));
    assert!(first.unread());
    ingest(stop);
    assert_eq!(
        service
            .snapshot_scoped(provider, Some("host-a"), Some("pane"))
            .attention_sequence,
        first.attention_sequence
    );
    ingest(start);
    ingest(stop);
    let second = service.snapshot_scoped(provider, Some("host-a"), Some("pane"));
    assert!(second.attention_sequence > first.attention_sequence);
    let acknowledge = |sequence: u64| {
        let mut command = CommandInvocation::from_action(
            &format!("agents.{provider}.acknowledge"),
            Caller::Internal,
        );
        command.arguments = vec![sequence.to_string()];
        let mut request = AgentInvocation::new(
            command,
            true,
            Some("host-a".to_owned()),
            deadline,
            cancellation.clone(),
        );
        request.launch_context.pane = Some("pane".to_owned());
        service.invoke(&request)
    };
    assert!(matches!(
        acknowledge(first.attention_sequence),
        CommandOutcome::Success { .. }
    ));
    assert!(
        service
            .snapshot_scoped(provider, Some("host-a"), Some("pane"))
            .unread()
    );
    acknowledge(second.attention_sequence);
    assert!(
        !service
            .snapshot_scoped(provider, Some("host-a"), Some("pane"))
            .unread()
    );
    assert!(
        !service
            .snapshot_scoped(provider, Some("host-b"), Some("pane"))
            .unread()
    );
}
