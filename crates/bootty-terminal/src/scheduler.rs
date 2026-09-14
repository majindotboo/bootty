use std::time::Duration;

const INPUT_REFRESH_INTERVAL: Duration = Duration::ZERO;
// A busy terminal needs another frame soon, but a zero-delay recommendation turns a persistent
// backend backlog into an uncapped GPUI render loop. Backend publication already wakes the host;
// this interval only drains work that remains after that frame.
const BUSY_REFRESH_INTERVAL: Duration = Duration::from_millis(16);
/// Cadence for animations that need intermediate frames, such as indeterminate progress.
pub const CURSOR_BLINK_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const CHROME_REFRESH_INTERVAL: Duration = Duration::from_millis(900);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepaintSignal {
    pub drained_bytes: usize,
    pub drain_elapsed_us: u64,
    pub pending_bytes: usize,
    pub dirty_rows: usize,
    pub cursor_blinking: bool,
    pub input_commands: usize,
}

impl RepaintSignal {
    const fn has_input(self) -> bool {
        self.input_commands > 0
    }

    const fn has_backlog_or_expensive_drain(self) -> bool {
        self.pending_bytes > 0 || self.drain_elapsed_us >= 1_000
    }

    const fn has_blinking_cursor(self) -> bool {
        self.cursor_blinking
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepaintScheduler {
    input: Duration,
    busy: Duration,
    chrome: Duration,
}

impl Default for RepaintScheduler {
    fn default() -> Self {
        Self {
            input: INPUT_REFRESH_INTERVAL,
            busy: BUSY_REFRESH_INTERVAL,
            // Terminal output publishes a wake directly. Periodic repainting
            // is only a chrome/session-refresh safety net while idle.
            chrome: CHROME_REFRESH_INTERVAL,
        }
    }
}

impl RepaintScheduler {
    #[must_use]
    pub const fn recommend(self, signal: RepaintSignal) -> Duration {
        if signal.has_input() {
            self.input
        } else if signal.has_backlog_or_expensive_drain() {
            self.busy
        } else if signal.has_blinking_cursor() {
            CURSOR_BLINK_REFRESH_INTERVAL
        } else {
            self.chrome
        }
    }
}
