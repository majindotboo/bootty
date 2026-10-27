//! Local calendar/clock values for the native status bar, and moments written the way the
//! person's system writes them.

use std::time::Duration;

/// Which parts of a moment to write. The system picks their order, language and clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Moment {
    /// 15:40 or 3:40 PM.
    Time,
    /// 15:40:05 or 3:40:05 PM.
    TimeWithSeconds,
    /// Oct 17.
    Day,
    /// Oct 17, 2026.
    DayAndYear,
    /// Oct 17, 15:40.
    DayAndTime,
    /// Fri 15:40.
    WeekdayAndTime,
    /// Fri, Oct 17.
    WeekdayAndDay,
}

impl Moment {
    /// The Unicode date skeleton the system localizes.
    #[cfg(target_os = "macos")]
    const fn skeleton(self) -> &'static str {
        match self {
            Self::Time => "jmm",
            Self::TimeWithSeconds => "jmmss",
            Self::Day => "MMMd",
            Self::DayAndYear => "yMMMd",
            Self::DayAndTime => "MMMdjmm",
            Self::WeekdayAndTime => "EEEjmm",
            Self::WeekdayAndDay => "EEEMMMd",
        }
    }
}

/// `at` as the person's system writes `moment`: its language, date order and 12- or 24-hour
/// clock.
#[must_use]
pub fn local_moment(at: chrono::DateTime<chrono::Local>, moment: Moment) -> String {
    platform_moment(at, moment)
}

#[cfg(target_os = "macos")]
fn platform_moment(at: chrono::DateTime<chrono::Local>, moment: Moment) -> String {
    use objc2::rc::Retained;
    use objc2_foundation::{NSDate, NSDateFormatter, NSString};
    use std::{cell::RefCell, collections::HashMap};
    thread_local! {
        // A formatter reads the locale when made; Bootty picks up a changed locale on restart.
        static FORMATTERS: RefCell<HashMap<Moment, Retained<NSDateFormatter>>> =
            RefCell::default();
    }
    let seconds =
        num_traits::ToPrimitive::to_f64(&at.timestamp_millis()).unwrap_or_default() / 1_000.;
    FORMATTERS.with_borrow_mut(|formatters| {
        let formatter = formatters.entry(moment).or_insert_with(|| {
            let formatter = NSDateFormatter::new();
            formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str(moment.skeleton()));
            formatter
        });
        formatter
            .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(seconds))
            .to_string()
    })
}

/// Other platforms' locale APIs need FFI, which Bootty denies, so they write English with a
/// 24-hour clock until a safe binding exists.
#[cfg(not(target_os = "macos"))]
fn platform_moment(at: chrono::DateTime<chrono::Local>, moment: Moment) -> String {
    let format = match moment {
        Moment::Time => "%H:%M",
        Moment::TimeWithSeconds => "%H:%M:%S",
        Moment::Day => "%b %-d",
        Moment::DayAndYear => "%b %-d, %Y",
        Moment::DayAndTime => "%b %-d, %H:%M",
        Moment::WeekdayAndTime => "%a %H:%M",
        Moment::WeekdayAndDay => "%a, %b %-d",
    };
    at.format(format).to_string()
}

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
            date: local_moment(now, Moment::WeekdayAndDay),
            time: local_moment(now, Moment::TimeWithSeconds),
            epoch: now.timestamp(),
        }
    }
}
