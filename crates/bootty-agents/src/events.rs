use std::time::Instant;

use bootty_control::{CommandCancellation, ControlEventSender};
use serde_json::{Value, json};

use crate::provider::{AgentEventKind, AgentKind, AgentState};

/// The one event publication seam used by the native agent service.
pub trait AgentEventPublisher: Send + Sync {
    /// # Errors
    /// Returns an error if publication is cancelled, exceeds its deadline, or the event receiver rejects it.
    fn publish(
        &self,
        identity: &str,
        generation: u64,
        topic: &str,
        payload: Value,
        deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> Result<(), String>;
}

impl AgentEventPublisher for ControlEventSender {
    fn publish(
        &self,
        identity: &str,
        generation: u64,
        topic: &str,
        payload: Value,
        deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> Result<(), String> {
        Self::publish(
            self,
            identity.to_owned(),
            generation,
            topic.to_owned(),
            payload,
            deadline,
            cancellation,
        )
    }
}

/// A raw provider event plus the typed state produced by applying its transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEvent {
    pub provider: AgentKind,
    pub scope: String,
    pub pane: String,
    pub kind: AgentEventKind,
    pub state: AgentState,
    pub payload: Value,
}

impl AgentEvent {
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "scope": self.scope.clone(),
            "pane": self.pane.clone(),
            "state": self.state.to_value(),
            "payload": self.payload.clone(),
        })
    }
}
