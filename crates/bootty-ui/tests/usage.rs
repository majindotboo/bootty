#![cfg(test)]

use bootty_ui::usage::{QuotaTone, UsageWindow, parse_usage};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case(r#"{"primary":{"usedPercent":25,"resetsAt":"2026-09-02T12:00:00Z"}}"#)]
#[case(r#"[{"usage":{"primary":{"usedPercent":25,"resetsAt":"2026-09-02T12:00:00Z"}}}]"#)]
fn accepts_provider_envelopes(#[case] json: &str) {
    let usage = parse_usage(json);
    assert_eq!(usage.error, None);
    assert_eq!(usage.windows.len(), 1);
    let meter = usage.windows[0].meter(
        usage.windows[0]
            .resets_at
            .unwrap()
            .checked_sub(9000)
            .expect("window start fits"),
    );
    assert_eq!(meter.remaining_percent.to_bits(), 75.0_f64.to_bits());
    assert_eq!(meter.expected_remaining_percent, Some(50.0));
    assert_eq!(meter.pace, "+25%");
    assert_eq!(meter.reset, "2h30");
    assert_eq!(meter.marker_tone, QuotaTone::Success);
}

#[rstest]
#[case(
    r#"{"error":{"message":"unavailable\nprivate detail"}}"#,
    "unavailable"
)]
#[case(r#"[{"error":{"message":"not signed in"}}]"#, "not signed in")]
#[case("not json\nmore details", "not json")]
fn reports_first_line_of_provider_errors(#[case] json: &str, #[case] expected: &str) {
    let usage = parse_usage(json);
    assert_eq!(usage.error.as_deref(), Some(expected));
    assert_eq!(usage.windows, Vec::<bootty_ui::usage::UsageWindow>::new());
}

#[rstest]
#[case(95.0, QuotaTone::Critical)]
#[case(85.0, QuotaTone::Warning)]
#[case(50.0, QuotaTone::Provider)]
fn quota_warning_thresholds(#[case] used_percent: f64, #[case] expected: QuotaTone) {
    let meter = UsageWindow {
        label: "5h",
        used_percent,
        duration_secs: 18_000.0,
        resets_at: None,
    }
    .meter(0);
    assert_eq!(meter.tone, expected);
    assert_eq!(meter.expected_remaining_percent, None);
    assert_eq!(meter.pace, "");
    assert_eq!(meter.reset, "");
}

proptest! {
    #[test]
    fn provider_percentages_stay_within_meter_bounds(used in -1000_f64..1000.0) {
        let json = serde_json::json!({"primary": {"usedPercent": used}}).to_string();
        let window = parse_usage(&json).windows[0];
        prop_assert!((0.0..=100.0).contains(&window.used_percent));
        prop_assert!((0.0..=100.0).contains(&window.meter(0).remaining_percent));
    }
}
