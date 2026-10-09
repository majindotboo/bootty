//! Selected conversation recovery emits resume targets, never prompts or new identities.
use bootty_agents::{NativeSessionRecord, NativeSessionStatus};
use bootty_control::CommandTarget;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct NativeReconnect {
    session: Option<String>,
    retry_at: Option<Instant>,
    failures: u32,
}

pub enum NativeReconnectStep {
    Idle,
    Wait(Duration),
    Resume(CommandTarget),
}

impl NativeReconnect {
    pub fn poll(&mut self, record: &NativeSessionRecord, now: Instant) -> NativeReconnectStep {
        if self.session.as_deref() != Some(record.id.as_str()) || !record.snapshot.transport_lost {
            self.session = Some(record.id.clone());
            self.retry_at = None;
            self.failures = 0;
        }
        if !record.snapshot.transport_lost || record.snapshot.status != NativeSessionStatus::Error {
            return NativeReconnectStep::Idle;
        }
        if let Some(at) = self.retry_at.filter(|at| *at > now) {
            return NativeReconnectStep::Wait(at.saturating_duration_since(now));
        }
        let delay = Duration::from_millis(500_u64 << self.failures.min(4));
        self.failures = self.failures.saturating_add(1);
        self.retry_at = now.checked_add(delay);
        NativeReconnectStep::Resume(record.target())
    }
}
