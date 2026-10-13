//! Local calendar/clock values for the native status bar.

use std::time::Duration;

/// Compact elapsed label shared by native and terminal agent activity.
#[must_use]
pub fn format_working_duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    let mut parts = Vec::with_capacity(3);
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

/// Coarse age of past activity: now, 5m, 3h, 2d, 3w, 4mo, 1y.
#[must_use]
pub fn format_age(elapsed: Duration) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let seconds = elapsed.as_secs();
    match seconds {
        0..MINUTE => "now".to_owned(),
        MINUTE..HOUR => format!("{}m", seconds / MINUTE),
        HOUR..DAY => format!("{}h", seconds / HOUR),
        _ if seconds < 7 * DAY => format!("{}d", seconds / DAY),
        _ if seconds < 30 * DAY => format!("{}w", seconds / (7 * DAY)),
        _ if seconds < 365 * DAY => format!("{}mo", seconds / (30 * DAY)),
        _ => format!("{}y", seconds / (365 * DAY)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockSnapshot {
    pub date: String,
    pub time: String,
    pub epoch: i64,
}

impl ClockSnapshot {
    #[must_use]
    pub fn now() -> Self {
        let now = chrono::Local::now();
        Self {
            date: now.format("%a %b %d").to_string(),
            time: now.format("%H:%M:%S").to_string(),
            epoch: now.timestamp(),
        }
    }
}
