//! Settings-facing units for the native terminal scrollback byte budget.

use bootty_terminal::terminal_engine::NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;

/// Present the byte budget as its estimated number of terminal lines.
#[must_use]
pub const fn lines_from_bytes(bytes: usize) -> usize {
    bytes.div_ceil(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE)
}

/// Convert an entered line count to the persisted byte budget.
#[must_use]
pub fn bytes_from_lines(lines: usize) -> i64 {
    i64::try_from(lines.saturating_mul(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE))
        .unwrap_or(i64::MAX)
}
