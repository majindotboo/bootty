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
