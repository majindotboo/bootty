//! Restart recovery remembers interrupted work without replaying the original input.
use crate::{NativeSessionSnapshot, NativeSessionStatus};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NativeRestart {
    source_turn: Option<String>,
    attempts: u8,
    completed_turns: u64,
}

impl NativeRestart {
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.source_turn.is_some()
    }

    /// Three attempts without a completed turn stop automatic restart loops.
    #[must_use]
    pub const fn needs_resume(&self) -> bool {
        self.has_pending() && self.attempts < 3
    }

    pub(crate) fn observe(&mut self, snapshot: &NativeSessionSnapshot, established: bool) {
        if established
            && snapshot.status == NativeSessionStatus::Working
            && !snapshot.stopping()
            && snapshot.requests.is_empty()
            && snapshot.pending_async_questions().next().is_none()
            && let Some(turn) = &snapshot.turn_id
        {
            if self.completed_turns != snapshot.completed_turns {
                self.completed_turns = snapshot.completed_turns;
                self.attempts = 0;
            }
            self.source_turn = Some(turn.clone());
        }
    }

    pub(crate) fn prepare(&mut self) -> Option<String> {
        let turn = self.source_turn.clone()?;
        self.attempts = self.attempts.saturating_add(1).min(3);
        Some(turn)
    }

    pub(crate) fn sending(&mut self, turn: &str) {
        if self.source_turn.as_deref() == Some(turn) {
            self.cancel();
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.source_turn = None;
    }

    pub(crate) fn completed(&mut self, turns: u64) {
        if turns > self.completed_turns {
            self.cancel();
        }
    }
}
