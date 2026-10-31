#![cfg(test)]

use bootty_ui::usage::{QuotaTone, UsageProvider, UsageService, UsageWindow};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(95.0, QuotaTone::Critical)]
#[case(85.0, QuotaTone::Warning)]
#[case(50.0, QuotaTone::Provider)]
fn quota_warning_thresholds(#[case] used_percent: f64, #[case] expected: QuotaTone) {
    let meter = UsageWindow {
        label: "5h".into(),
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
        label: "7d".into(),
        used_percent,
        duration_secs: 604_800.0,
        resets_at: Some(302_400),
    }
    .meter(0);
    assert_eq!(meter.pace, pace);
    assert_eq!(meter.pace_tone, tone);
    assert_eq!(meter.reset, "3d 12h");
}

#[test]
fn native_provider_reports_supply_usage() {
    let mut usage = UsageService::default();
    let now = std::time::Instant::now();
    let window = |label: &str, used_percent| UsageWindow {
        label: label.into(),
        used_percent,
        duration_secs: 18_000.0,
        resets_at: Some(1_760_000_000),
    };
    usage.report(UsageProvider::Codex, vec![window("5h", 72.0)], now);
    usage.report(
        UsageProvider::Claude,
        vec![window("5h", 10.0), window("7d Opus", 40.0)],
        now,
    );
    assert_eq!(
        usage
            .current()
            .iter()
            .map(|provider| provider
                .windows
                .iter()
                .map(|window| (window.label.as_str(), window.used_percent))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [vec![("5h", 72.0)], vec![("5h", 10.0), ("7d Opus", 40.0)]]
    );
}
