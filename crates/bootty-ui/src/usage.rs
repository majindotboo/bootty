//! Quota presentation from native conversation reports.
use num_traits::ToPrimitive as _;
use std::time::Instant;

#[derive(Default)]
pub struct UsageService {
    current: [ProviderUsage; 2],
}
impl UsageService {
    #[must_use]
    pub const fn current(&self) -> &[ProviderUsage; 2] {
        &self.current
    }
    pub fn report(&mut self, provider: UsageProvider, windows: Vec<UsageWindow>, _now: Instant) {
        let [codex, claude] = &mut self.current;
        let current = match provider {
            UsageProvider::Codex => codex,
            UsageProvider::Claude => claude,
        };
        *current = ProviderUsage {
            windows,
            error: None,
        };
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

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    pub used_percent: f64,
    pub duration_secs: f64,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderUsage {
    pub windows: Vec<UsageWindow>,
    pub error: Option<String>,
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
    pub fn meter(&self, now: i64) -> QuotaMeter {
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
            pace: expected.map_or_else(String::new, |_| match rounded_deficit {
                1.. => format!("{rounded_deficit}% behind pace"),
                ..=-1 => format!("{}% ahead of pace", rounded_deficit.unsigned_abs()),
                _ => "On pace".to_owned(),
            }),
            pace_tone: match rounded_deficit.cmp(&0) {
                std::cmp::Ordering::Greater => deficit_tone(f64::from(rounded_deficit)),
                std::cmp::Ordering::Less => QuotaTone::Success,
                std::cmp::Ordering::Equal => QuotaTone::Muted,
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
        format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3600)
    } else if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else {
        format!("{}m", seconds / 60)
    }
}
