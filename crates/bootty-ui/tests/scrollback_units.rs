#![cfg(test)]

use bootty_config::config::BoottyConfig;
use bootty_terminal::terminal_engine::NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;
use bootty_ui::presentation::scrollback::{bytes_from_lines, lines_from_bytes};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case(0, 0)]
#[case(1, 1)]
#[case(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE, 1)]
#[case(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE + 1, 2)]
fn byte_budgets_are_presented_as_estimated_lines(#[case] bytes: usize, #[case] lines: usize) {
    assert_eq!(lines_from_bytes(bytes), lines);
}

#[rstest]
#[case(0, 0)]
#[case(1, i64::try_from(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE).expect("row estimate fits"))]
#[case(1_000_000, 320_000_000)]
fn entered_lines_are_persisted_as_byte_budgets(#[case] lines: usize, #[case] bytes: i64) {
    assert_eq!(bytes_from_lines(lines), bytes);
}

#[rstest]
fn default_scrollback_is_presented_as_one_million_lines() {
    assert_eq!(
        lines_from_bytes(BoottyConfig::default().session.max_scrollback),
        1_000_000
    );
}

#[rstest]
fn line_to_byte_conversion_saturates_at_the_persisted_integer_limit() {
    let expected = i64::try_from(usize::MAX).unwrap_or(i64::MAX);
    assert_eq!(bytes_from_lines(usize::MAX), expected);
}

proptest! {
    #[test]
    fn representable_line_counts_round_trip(
        lines in 0_usize..=max_round_trip_lines()
    ) {
        let bytes = usize::try_from(bytes_from_lines(lines)).expect("byte budget is representable");
        prop_assert_eq!(lines_from_bytes(bytes), lines);
    }
}

fn max_round_trip_lines() -> usize {
    usize::try_from(i64::MAX).unwrap_or(usize::MAX) / NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE
}
