//! Local calendar/clock values for the native status bar.

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
