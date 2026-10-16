//! The account's usage limits as the provider last reported them: how much of each window is
//! used, when it resets, and whether a limit stopped work.

use crate::NativeSessionSnapshot;
use crate::native_protocol::field;
use num_traits::ToPrimitive as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeRateLimits {
    pub windows: Vec<NativeRateLimit>,
    /// A limit stopped work until it resets.
    #[serde(default)]
    pub reached: bool,
}

/// One usage window, such as the five hours or seven days a plan allows.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeRateLimit {
    /// What the window covers, such as "5h" or "7d Opus".
    pub label: String,
    pub used_percent: u8,
    /// When it resets, in UTC seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// How long the window lasts, when it is a span of time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_seconds: Option<u64>,
}

impl NativeRateLimits {
    /// The soonest a reached limit resets, when one stopped work.
    #[must_use]
    pub fn reached_until(&self) -> Option<i64> {
        self.reached
            .then(|| {
                self.windows
                    .iter()
                    .filter(|window| window.used_percent >= 100)
                    .filter_map(|window| window.resets_at)
                    .max()
            })
            .flatten()
    }
}

impl NativeSessionSnapshot {
    /// Codex's account limits, which come outside any thread. Limits for other metered models
    /// leave them alone. Returns whether `value` was its report.
    pub(crate) fn codex_rate_limits(&mut self, value: &Value) -> bool {
        if field(value, "method") != "account/rateLimits/updated" {
            return false;
        }
        let limits = field(field(value, "params"), "rateLimits");
        if field(limits, "limitId")
            .as_str()
            .is_some_and(|id| id != "codex")
        {
            return true;
        }
        let windows = ["primary", "secondary"]
            .into_iter()
            .filter_map(|key| {
                let window = field(limits, key);
                let minutes = field(window, "windowDurationMins").as_u64()?;
                Some(NativeRateLimit {
                    label: duration_label(minutes),
                    used_percent: percent(field(window, "usedPercent").as_f64()?),
                    resets_at: field(window, "resetsAt").as_i64(),
                    window_seconds: minutes.checked_mul(60),
                })
            })
            .collect::<Vec<_>>();
        self.rate_limits = Some(NativeRateLimits {
            reached: !field(limits, "rateLimitReachedType").is_null(),
            windows,
        });
        self.changed();
        true
    }

    /// One of Claude's limit reports, for one window; the others keep what they last said.
    pub(crate) fn claude_rate_limit(&mut self, value: &Value) {
        const WEEK: Option<u64> = Some(604_800);
        let info = field(value, "rate_limit_info");
        let Some((label, window_seconds)) = (match field(info, "rateLimitType").as_str() {
            Some("five_hour") => Some(("5h", Some(18_000))),
            Some("seven_day") => Some(("7d", WEEK)),
            Some("seven_day_opus") => Some(("7d Opus", WEEK)),
            Some("seven_day_sonnet") => Some(("7d Sonnet", WEEK)),
            Some("overage") => Some(("Extra usage", None)),
            _ => None,
        }) else {
            return;
        };
        let rejected = field(info, "status") == "rejected";
        let used = field(info, "utilization")
            .as_f64()
            .map_or(if rejected { 100 } else { 0 }, |used| percent(used * 100.));
        let window = NativeRateLimit {
            label: label.to_owned(),
            used_percent: if rejected { 100 } else { used },
            resets_at: field(info, "resetsAt").as_i64(),
            window_seconds,
        };
        let limits = self
            .rate_limits
            .get_or_insert_with(NativeRateLimits::default);
        match limits
            .windows
            .iter_mut()
            .find(|known| known.label == window.label)
        {
            Some(known) => *known = window,
            None => limits.windows.push(window),
        }
        limits.reached = rejected;
    }
}

fn percent(used: f64) -> u8 {
    used.clamp(0., 100.).round().to_u8().unwrap_or(0)
}

/// "5h" for 300 minutes, "7d" for 10080.
fn duration_label(minutes: u64) -> String {
    match minutes {
        minutes if minutes % 1440 == 0 => format!("{}d", minutes / 1440),
        minutes if minutes % 60 == 0 => format!("{}h", minutes / 60),
        minutes => format!("{minutes}m"),
    }
}
