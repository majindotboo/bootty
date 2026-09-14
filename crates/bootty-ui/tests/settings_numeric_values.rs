#![cfg(test)]

use bootty_ui::settings_session::{normalize_number, parse_display_number};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("37.5", 100.0, Some(0.375))]
#[case("150", 100.0, Some(1.0))]
#[case("-5", 100.0, Some(0.0))]
#[case(" 0.125 ", 1.0, Some(0.125))]
fn displayed_numbers_are_unscaled_and_clamped(
    #[case] text: &str,
    #[case] display_scale: f32,
    #[case] expected: Option<f32>,
) {
    assert_eq!(
        parse_display_number(text, &(0.0..=1.0), display_scale),
        expected
    );
}

#[rstest]
#[case("")]
#[case("not a number")]
#[case("NaN")]
#[case("inf")]
#[case("-inf")]
fn invalid_displayed_numbers_are_rejected(#[case] text: &str) {
    assert_eq!(parse_display_number(text, &(0.0..=1.0), 100.0), None);
}

#[rstest]
#[case(0.0)]
#[case(-1.0)]
#[case(f32::INFINITY)]
#[case(f32::NAN)]
fn invalid_display_scales_are_rejected(#[case] scale: f32) {
    assert_eq!(parse_display_number("50", &(0.0..=1.0), scale), None);
}

#[rstest]
#[case(-1.0, Some(0.0))]
#[case(0.375, Some(0.375))]
#[case(2.0, Some(1.0))]
#[case(f32::NAN, None)]
#[case(f32::INFINITY, None)]
fn stored_numbers_are_clamped_without_display_scaling(
    #[case] value: f32,
    #[case] expected: Option<f32>,
) {
    assert_eq!(normalize_number(value, &(0.0..=1.0)), expected);
}

proptest! {
    #[test]
    fn displayed_number_round_trip_is_symmetric(value in 0.0_f32..=1.0) {
        let displayed = (value * 100.0).to_string();
        let parsed = parse_display_number(&displayed, &(0.0..=1.0), 100.0)
            .expect("finite displayed value parses");
        prop_assert!((parsed - value).abs() <= f32::EPSILON * 4.0);
    }
}
