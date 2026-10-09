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
    assert_eq!(meter.pace, "25% ahead of pace");
    assert_eq!(meter.reset, "2h 30m");
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

#[rstest]
#[case(30.0, "20% ahead of pace", QuotaTone::Success)]
#[case(50.0, "On pace", QuotaTone::Muted)]
#[case(60.0, "10% behind pace", QuotaTone::Warning)]
#[case(100.0, "50% behind pace", QuotaTone::Critical)]
fn quota_pacing_names_the_estimated_direction(
    #[case] used_percent: f64,
    #[case] pace: &str,
    #[case] tone: QuotaTone,
) {
    let meter = UsageWindow {
        label: "7d",
        used_percent,
        duration_secs: 604_800.0,
        resets_at: Some(302_400),
    }
    .meter(0);
    assert_eq!(meter.pace, pace);
    assert_eq!(meter.pace_tone, tone);
    assert_eq!(meter.reset, "3d 12h");
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
