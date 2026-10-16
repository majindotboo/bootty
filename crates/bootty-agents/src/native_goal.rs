//! A Codex thread's goal: what it works toward, whether it is working on it, and what it has
//! spent.

use crate::NativeSessionSnapshot;
use crate::native_protocol::field;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeGoal {
    pub objective: String,
    pub status: NativeGoalStatus,
    /// The tokens it may spend, when it has a budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub seconds_used: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeGoalStatus {
    Active,
    Paused,
    Blocked,
    /// The account's usage limit stopped it.
    UsageLimited,
    /// It spent its token budget.
    BudgetLimited,
    Complete,
}

impl NativeGoal {
    /// Codex's `ThreadGoal`, its objective cut to the 8 KiB a goal may have.
    pub(crate) fn codex(goal: &Value) -> Option<Self> {
        let objective = field(goal, "objective").as_str()?.trim();
        if objective.is_empty() {
            return None;
        }
        Some(Self {
            objective: objective
                .get(..objective.floor_char_boundary(8192))
                .unwrap_or_default()
                .to_owned(),
            status: match field(goal, "status").as_str() {
                Some("paused") => NativeGoalStatus::Paused,
                Some("blocked") => NativeGoalStatus::Blocked,
                Some("usageLimited") => NativeGoalStatus::UsageLimited,
                Some("budgetLimited") => NativeGoalStatus::BudgetLimited,
                Some("complete") => NativeGoalStatus::Complete,
                _ => NativeGoalStatus::Active,
            },
            token_budget: field(goal, "tokenBudget").as_u64(),
            tokens_used: field(goal, "tokensUsed").as_u64().unwrap_or(0),
            seconds_used: field(goal, "timeUsedSeconds").as_u64().unwrap_or(0),
        })
    }
}

impl NativeSessionSnapshot {
    /// Codex's goal notifications for this thread, which come outside any turn. Returns whether
    /// `value` was one.
    pub(crate) fn codex_goal(&mut self, value: &Value) -> bool {
        let params = field(value, "params");
        if self.session_id.is_none()
            || self.session_id.as_deref() != field(params, "threadId").as_str()
        {
            return false;
        }
        match field(value, "method").as_str() {
            Some("thread/goal/updated") => self.goal = NativeGoal::codex(field(params, "goal")),
            Some("thread/goal/cleared") => self.goal = None,
            _ => return false,
        }
        self.changed();
        true
    }
}
