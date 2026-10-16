//! What each turn used: its tokens, and its price when the provider reports one, kept on the
//! turn's last reply.

use crate::NativeSessionSnapshot;
use crate::native_protocol::field;
use num_traits::ToPrimitive as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One turn's tokens across its model calls. `input` counts every prompt token, cached ones
/// included.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeTurnUsage {
    pub input: u64,
    pub output: u64,
    /// Of `input`, those read from the provider's cache.
    #[serde(default)]
    pub cache_read: u64,
    /// Of `input`, those written to the provider's cache.
    #[serde(default)]
    pub cache_write: u64,
    /// Of `output`, those spent reasoning, when the provider says.
    #[serde(default)]
    pub reasoning: u64,
    /// The price the provider reports, in millionths of a US dollar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
}

impl NativeTurnUsage {
    fn add(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
        self.cost_micros = match (self.cost_micros, other.cost_micros) {
            (Some(cost), Some(more)) => Some(cost.saturating_add(more)),
            (cost, more) => cost.or(more),
        };
    }

    /// Claude's turn result: counters that leave cached input out, and a price in dollars.
    pub(crate) fn claude(result: &Value) -> Option<Self> {
        let usage = field(result, "usage");
        let count = |key| field(usage, key).as_u64().unwrap_or(0);
        let input = field(usage, "input_tokens").as_u64()?;
        let (cache_read, cache_write) = (
            count("cache_read_input_tokens"),
            count("cache_creation_input_tokens"),
        );
        Some(Self {
            input: input.saturating_add(cache_read).saturating_add(cache_write),
            output: count("output_tokens"),
            cache_read,
            cache_write,
            reasoning: 0,
            cost_micros: dollars(field(result, "total_cost_usd")),
        })
    }

    /// One of Pi's assistant messages, whose counters leave cached input out too.
    pub(crate) fn pi(usage: &Value) -> Option<Self> {
        let count = |key| field(usage, key).as_u64().unwrap_or(0);
        let input = field(usage, "input").as_u64()?;
        let (cache_read, cache_write) = (count("cacheRead"), count("cacheWrite"));
        Some(Self {
            input: input.saturating_add(cache_read).saturating_add(cache_write),
            output: count("output"),
            cache_read,
            cache_write,
            reasoning: 0,
            cost_micros: dollars(field(field(usage, "cost"), "total")).filter(|cost| *cost > 0),
        })
    }

    /// Codex's running totals for the thread.
    fn codex(breakdown: &Value) -> Option<Self> {
        let count = |key| field(breakdown, key).as_u64().unwrap_or(0);
        Some(Self {
            input: field(breakdown, "inputTokens").as_u64()?,
            output: count("outputTokens"),
            cache_read: count("cachedInputTokens"),
            cache_write: 0,
            reasoning: count("reasoningOutputTokens"),
            cost_micros: None,
        })
    }

    const fn minus(self, earlier: Self) -> Self {
        Self {
            input: self.input.saturating_sub(earlier.input),
            output: self.output.saturating_sub(earlier.output),
            cache_read: self.cache_read.saturating_sub(earlier.cache_read),
            cache_write: self.cache_write.saturating_sub(earlier.cache_write),
            reasoning: self.reasoning.saturating_sub(earlier.reasoning),
            cost_micros: None,
        }
    }
}

fn dollars(value: &Value) -> Option<u64> {
    value
        .as_f64()
        .filter(|dollars| dollars.is_finite() && *dollars >= 0.)
        .and_then(|dollars| (dollars * 1_000_000.).round().to_u64())
}

impl NativeSessionSnapshot {
    /// Adds a model call's tokens to the running turn's.
    pub(crate) fn add_turn_usage(&mut self, usage: NativeTurnUsage) {
        self.turn_usage
            .get_or_insert_with(NativeTurnUsage::default)
            .add(usage);
    }

    /// Codex reports the thread's totals and the latest call's; the turn's are the totals less
    /// what came before its first report.
    pub(crate) fn codex_turn_usage(&mut self, turn: Option<&str>, usage: &Value) {
        let (Some(turn), Some(total), Some(last)) = (
            turn,
            NativeTurnUsage::codex(field(usage, "total")),
            NativeTurnUsage::codex(field(usage, "last")),
        ) else {
            return;
        };
        let before = match &self.codex_usage_before {
            Some((started, before)) if started == turn => *before,
            _ => total.minus(last),
        };
        self.codex_usage_before = Some((turn.to_owned(), before));
        self.turn_usage = Some(total.minus(before));
    }

    /// Keeps the finished turn's usage on its last reply.
    pub(crate) fn settle_turn_usage(&mut self) {
        self.turn_ended = true;
        let Some(usage) = self.turn_usage.take() else {
            return;
        };
        if let Some(reply) = self
            .transcript
            .iter_mut()
            .rev()
            .take_while(|item| item.role != "user")
            .find(|item| item.role == "assistant")
        {
            reply.usage = Some(usage);
        }
    }
}
