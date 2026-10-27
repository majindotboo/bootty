use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use time::OffsetDateTime;
use time::macros::format_description;

pub fn utc_timestamp() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("failed to format UTC timestamp")
}

pub fn utc_datetime() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .context("failed to format UTC time")
}

pub struct Timer {
    started: Instant,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl Timer {
    pub fn start() -> Self {
        Self::with_clock(Arc::new(Instant::now))
    }

    pub fn with_clock(now: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            started: now(),
            now,
        }
    }

    pub fn elapsed(&self) -> Duration {
        (self.now)().saturating_duration_since(self.started)
    }
}
