#![cfg(test)]

use std::time::Duration;

use bootty_ui::gpui::chrome::{
    ChromeLayout, SidebarPosition, StatusIntent, TabDragGesture, TabInsertionTarget,
    WindowDragGesture, tab_insertion_target,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case(Duration::ZERO, "0s")]
#[case(Duration::from_secs(7), "7s")]
#[case(Duration::from_millis(59_999), "59s")]
#[case(Duration::from_secs(60), "1m")]
#[case(Duration::from_millis(299_999), "4m 59s")]
#[case(Duration::from_secs(300), "5m")]
#[case(Duration::from_secs(3_599), "59m 59s")]
#[case(Duration::from_secs(3_600), "1h")]
#[case(Duration::from_mins(90), "1h 30m")]
#[case(Duration::from_secs(3_661), "1h 1m 1s")]
#[case(Duration::from_mins(1_501), "25h 1m")]
fn agent_working_duration_uses_compact_seconds_minutes_and_hours(
    #[case] elapsed: Duration,
    #[case] expected: &str,
) {
    assert_eq!(bootty_ui::clock::format_working_duration(elapsed), expected);
}

proptest! {
    #[test]
    fn fractional_seconds_do_not_advance_working_labels(
        seconds in 0_u64..60,
        nanos in 0_u32..1_000_000_000,
    ) {
        prop_assert_eq!(
            bootty_ui::clock::format_working_duration(Duration::new(seconds, nanos)),
            format!("{seconds}s"),
        );
    }
}

#[rstest]
fn a_tab_press_cancels_the_pending_window_drag() {
    let mut gesture = WindowDragGesture::default();
    gesture.arm();
    gesture.cancel();

    assert!(!gesture.take_on_motion());
}

#[rstest]
fn empty_titlebar_motion_starts_at_most_one_window_drag() {
    let mut gesture = WindowDragGesture::default();
    gesture.arm();

    assert!(gesture.take_on_motion());
    assert!(!gesture.take_on_motion());
}

#[rstest]
#[case(Some("window-1"), TabInsertionTarget::Before("window-1".to_owned()))]
#[case(None, TabInsertionTarget::End)]
fn tab_drag_preview_and_release_share_the_insertion_target(
    #[case] before: Option<&str>,
    #[case] target: TabInsertionTarget,
) {
    let mut gesture = TabDragGesture::default();
    assert_eq!(gesture.insertion_target(), None);
    gesture.begin("window-2");
    gesture.hover_before(before);

    assert_eq!(gesture.insertion_target(), Some(&target));
    assert_eq!(
        gesture.release(),
        Some(StatusIntent::Reorder {
            source: "window-2".to_owned(),
            before: before.map(str::to_owned),
        })
    );
    assert_eq!(gesture.insertion_target(), None);
}

#[rstest]
fn releasing_a_tab_on_itself_is_not_a_reorder() {
    let mut gesture = TabDragGesture::default();
    gesture.begin("window-2");

    assert_eq!(gesture.release_before(Some("window-2")), None);
    assert_eq!(gesture.release_before(None), None);
}

#[rstest]
#[case("a", 1, false, TabInsertionTarget::Before("a".to_owned()))]
#[case("a", 1, true, TabInsertionTarget::Before("c".to_owned()))]
#[case("c", 0, false, TabInsertionTarget::Before("a".to_owned()))]
#[case("c", 0, true, TabInsertionTarget::Before("b".to_owned()))]
#[case("a", 2, true, TabInsertionTarget::End)]
fn tab_halves_resolve_to_stable_insertion_boundaries(
    #[case] source: &str,
    #[case] target: usize,
    #[case] right_half: bool,
    #[case] expected: TabInsertionTarget,
) {
    let anchors = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
    assert_eq!(
        tab_insertion_target(&anchors, source, target, right_half),
        Some(expected)
    );
}

#[rstest]
#[case(1200.0, 286.0, 286.0)]
#[case(1200.0, 900.0, 800.0)]
#[case(1200.0, 100.0, 200.0)]
#[case(360.0, 286.0, 159.0)]
#[case(100.0, 286.0, 99.0)]
fn sidebar_width_matches_zed_bounds_without_starving_the_center(
    #[case] window_width: f32,
    #[case] configured_width: f32,
    #[case] expected_width: f32,
) {
    let layout = ChromeLayout {
        left_dock_toggle: true,
        right_dock_toggle: true,
        panel_tab_style: bootty_config::config::PanelTabStyle::default(),
        panel_tabs: bootty_config::config::PanelTabs::default(),
        tabs: bootty_config::config::ChromeConfig::default().tabs,
        width: window_width,
        height: 800.0,
        sidebar_position: SidebarPosition::Left,
        sidebar_width: configured_width,
        gap: 1.0,
        top_inset: 0.0,
        notch_span: None,
        wrap_tabs_at_notch: true,
        titlebar_height: 0.0,
        status_height: 30.0,
        sidebar_visible: true,
        titlebar_visible: false,
        fullscreen: false,
    };

    assert_eq!(
        gpui_kit::px(layout.effective_sidebar_width()),
        gpui_kit::px(expected_width)
    );
}
