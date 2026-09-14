//! `CodexBar` quota presentation. `CodexBar` owns the provider protocols; Bootty reads its JSON.

use bootty_host::{CancellableCommandRunner, CommandCancellation, CommandRunner};
use chrono::DateTime;
use num_traits::ToPrimitive as _;
use serde_json::Value;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const USAGE_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// One bounded background request per supported `CodexBar` provider.
/// Pending processes are cancelled when this owner is retired; painting only reads snapshots.
#[derive(Default)]
pub struct UsageService {
    current: [ProviderUsage; 2],
    pending: [Option<mpsc::Receiver<ProviderUsage>>; 2],
    cancellation: CommandCancellation,
    last_poll: Option<Instant>,
}

impl UsageService {
    #[must_use]
    pub const fn current(&self) -> &[ProviderUsage; 2] {
        &self.current
    }

    pub fn refresh(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for (pending, current) in self.pending.iter_mut().zip(&mut self.current) {
            let result = pending.as_ref().map(std::sync::mpsc::Receiver::try_recv);
            match result {
                Some(Ok(updated)) => {
                    changed |= *current != updated;
                    *current = updated;
                    *pending = None;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => *pending = None,
                Some(Err(mpsc::TryRecvError::Empty)) | None => {}
            }
        }

        let due = self
            .last_poll
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_mins(1));
        if due {
            let mut started = false;
            for (pending, provider) in self.pending.iter_mut().zip(UsageProvider::ALL) {
                if pending.is_some() {
                    continue;
                }
                *pending = Self::spawn_provider(provider, self.cancellation.clone());
                started |= pending.is_some();
            }
            if started {
                self.last_poll = Some(now);
            }
        }
        changed
    }

    fn spawn_provider(
        provider: UsageProvider,
        cancellation: CommandCancellation,
    ) -> Option<mpsc::Receiver<ProviderUsage>> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let runner = CancellableCommandRunner::with_deadline(
            cancellation,
            Instant::now().checked_add(USAGE_COMMAND_TIMEOUT)?,
        );
        std::thread::spawn(move || {
            let usage = query_provider(&runner, provider);
            let _ = sender.send(usage);
        });
        Some(receiver)
    }
}

impl Drop for UsageService {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageProvider {
    Codex,
    Claude,
}

impl UsageProvider {
    pub const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

fn query_provider(runner: &CancellableCommandRunner, provider: UsageProvider) -> ProviderUsage {
    let args = ["usage", "--json", "--provider", provider.id()].map(str::to_owned);
    match runner.run("codexbar", &args) {
        Ok(output) if output.success || !output.stdout.is_empty() => parse_usage(&output.stdout),
        Ok(output) => ProviderUsage {
            error: Some(short_status(&output.stderr)),
            ..ProviderUsage::default()
        },
        Err(error) => ProviderUsage {
            error: Some(short_status(&error.to_string())),
            ..ProviderUsage::default()
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: &'static str,
    pub used_percent: f64,
    pub duration_secs: f64,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderUsage {
    pub windows: Vec<UsageWindow>,
    pub error: Option<String>,
}

/// Accept both `CodexBar`'s object and single-provider array responses.
#[must_use]
pub fn parse_usage(raw: &str) -> ProviderUsage {
    if raw.is_empty() {
        return ProviderUsage::default();
    }
    let Ok(data) = serde_json::from_str::<Value>(raw) else {
        return ProviderUsage {
            error: Some(short_status(raw)),
            ..ProviderUsage::default()
        };
    };
    let provider = data.get(0).unwrap_or(&data);
    if let Some(error) = data
        .get("error")
        .or_else(|| provider.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return ProviderUsage {
            error: Some(short_status(error)),
            ..ProviderUsage::default()
        };
    }
    let usage = provider.get("usage").unwrap_or(provider);
    let windows = [("primary", "5h", 18_000.0), ("secondary", "7d", 604_800.0)]
        .into_iter()
        .filter_map(|(key, label, fallback)| {
            let value = usage.get(key)?;
            Some(UsageWindow {
                label,
                used_percent: value.get("usedPercent")?.as_f64()?.clamp(0.0, 100.0),
                duration_secs: value
                    .get("windowMinutes")
                    .and_then(Value::as_f64)
                    .filter(|minutes| *minutes > 0.0)
                    .map_or(fallback, |minutes| minutes * 60.0),
                resets_at: value
                    .get("resetsAt")
                    .and_then(Value::as_str)
                    .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
                    .map(|date| date.timestamp()),
            })
        })
        .collect();
    ProviderUsage {
        windows,
        error: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaTone {
    Provider,
    Muted,
    Success,
    Warning,
    Critical,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaMeter {
    pub remaining_percent: f64,
    pub expected_remaining_percent: Option<f64>,
    pub tone: QuotaTone,
    pub marker_tone: QuotaTone,
    pub pace: String,
    pub pace_tone: QuotaTone,
    pub reset: String,
}

impl UsageWindow {
    #[must_use]
    pub fn meter(self, now: i64) -> QuotaMeter {
        let remaining = (100.0 - self.used_percent).clamp(0.0, 100.0);
        let reset = self.resets_at.map(|reset| reset.saturating_sub(now));
        let expected = reset
            .filter(|reset| *reset > 0 && self.duration_secs > 0.0)
            .and_then(|reset| reset.to_f64())
            .map(|reset| (reset / self.duration_secs * 100.0).clamp(0.0, 100.0));
        let deficit = expected.map_or(0.0, |expected| expected - remaining);
        let rounded_deficit = deficit.round().to_i32().unwrap_or_default();
        QuotaMeter {
            remaining_percent: remaining,
            expected_remaining_percent: expected,
            tone: if remaining <= 5.0 {
                QuotaTone::Critical
            } else if remaining <= 15.0 {
                QuotaTone::Warning
            } else {
                QuotaTone::Provider
            },
            marker_tone: if deficit <= 0.0 {
                QuotaTone::Success
            } else {
                deficit_tone(deficit)
            },
            pace: match rounded_deficit {
                1.. => format!("{rounded_deficit}% def"),
                ..=-1 => format!("+{}%", rounded_deficit.unsigned_abs()),
                _ => String::new(),
            },
            pace_tone: if rounded_deficit > 0 {
                deficit_tone(f64::from(rounded_deficit))
            } else {
                QuotaTone::Muted
            },
            reset: reset
                .filter(|reset| *reset > 0)
                .map_or_else(String::new, format_duration),
        }
    }
}

fn deficit_tone(deficit: f64) -> QuotaTone {
    if deficit >= 25.0 {
        QuotaTone::Critical
    } else {
        QuotaTone::Warning
    }
}

fn format_duration(seconds: i64) -> String {
    if seconds >= 86_400 {
        format!(
            "{}d{:02}:{:02}",
            seconds / 86_400,
            seconds % 86_400 / 3600,
            seconds % 3600 / 60
        )
    } else if seconds >= 3600 {
        format!("{}h{:02}", seconds / 3600, seconds % 3600 / 60)
    } else {
        format!("{}m", seconds / 60)
    }
}

fn short_status(value: &str) -> String {
    let first = value.lines().next().unwrap_or_default();
    if first.chars().count() > 42 {
        format!("{}...", first.chars().take(39).collect::<String>())
    } else {
        first.to_owned()
    }
}
