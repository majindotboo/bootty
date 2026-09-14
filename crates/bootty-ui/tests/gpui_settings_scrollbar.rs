#![cfg(test)]

use settings_support::{GpuiSettingsSnapshot, SettingsProbe as ScrollbarProbe};
#[path = "support/settings.rs"]
mod settings_support;
use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsControl, SettingsPage, SettingsPageItem, SettingsRow,
    UiPalette, init_theme,
};

fn init_zed_ui(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}
use gpui_kit::{
    Entity, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point,
};

fn scrollable_snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance|colors|text|window|sidebar|status".to_owned(),
            items: std::iter::once(SettingsPageItem::SectionHeader {
                id: "appearance".to_owned(),
                title: "Appearance".to_owned(),
                search_terms:
                    "cursor|blink|inactive pane|mouse pointer|hide while typing|fullscreen notch"
                        .to_owned(),
            })
            .chain((0..48).map(|index| {
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: format!("appearance.option-{index}"),
                    label: format!("Appearance option {index}"),
                    help: "A setting used to exercise the scrolling surface.".to_owned(),
                    value: ScalarValue::Bool(false),
                    control: SettingsControl::Toggle,
                    enabled: true,
                })
            }))
            .collect(),
        }],
        search: String::new(),
        write_error: None,
    }
}

fn scroll_first_row_out_of_view(cx: &mut VisualTestContext) {
    let content = cx
        .debug_bounds("settings-scrollbar-track")
        .expect("settings content surface");
    cx.simulate_event(ScrollWheelEvent {
        position: content.center(),
        delta: ScrollDelta::Lines(point(0.0, -18.0)),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-toggle-appearance.option-0")
            .is_none(),
        "the first row scrolled out of the virtual viewport"
    );
}

fn publish_snapshot(
    probe: &Entity<ScrollbarProbe>,
    snapshot: GpuiSettingsSnapshot,
    cx: &mut VisualTestContext,
) {
    probe.update(cx, |probe, cx| {
        probe.settings.update(cx, |settings, cx| {
            settings.set_content(snapshot.into_content(), cx);
        });
    });
    cx.run_until_parked();
}

#[gpui_kit::test]
fn scrolling_selects_the_visible_subsection_instead_of_the_category(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = scrollable_snapshot();
    let original = std::mem::take(&mut snapshot.pages[0].items);
    snapshot.pages[0].items = original
        .into_iter()
        .enumerate()
        .flat_map(|(index, item)| {
            if index == 0 {
                return vec![item];
            }
            if index == 33 {
                return vec![
                    SettingsPageItem::SectionHeader {
                        id: "last-section".to_owned(),
                        title: "Last section".to_owned(),
                        search_terms: "last".to_owned(),
                    },
                    item,
                ];
            }
            vec![item]
        })
        .collect();
    let (_, cx) = cx.add_window_view(|_, cx| ScrollbarProbe::with_snapshot(snapshot, cx));
    assert!(
        cx.debug_bounds("settings-active-section-appearance")
            .is_some()
    );

    for (delta, selected, unselected) in [
        (
            -500.0,
            "settings-active-section-last-section",
            "settings-active-section-appearance",
        ),
        (
            500.0,
            "settings-active-section-appearance",
            "settings-active-section-last-section",
        ),
    ] {
        let content = cx
            .debug_bounds("settings-scrollbar-track")
            .expect("content scroll surface");
        cx.simulate_event(ScrollWheelEvent {
            position: content.center(),
            delta: ScrollDelta::Lines(point(0.0, delta)),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds(selected).is_some());
        assert!(cx.debug_bounds(unselected).is_none());
    }
}

#[gpui_kit::test]
fn settings_show_a_tracked_scrollbar_for_overflowing_content(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) =
        cx.add_window_view(|_, cx| ScrollbarProbe::with_snapshot(scrollable_snapshot(), cx));
    cx.refresh().expect("redraw the tracked settings scrollbar");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("settings-scrollbar-track").is_some(),
        "overflowing settings content has a visible scrollbar track"
    );
}

#[gpui_kit::test]
fn value_only_snapshot_publications_preserve_the_content_scroll_position(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| ScrollbarProbe::with_snapshot(scrollable_snapshot(), cx));
    scroll_first_row_out_of_view(cx);

    let mut snapshot = scrollable_snapshot();
    let SettingsPageItem::Setting(SettingsRow::Value { value, .. }) =
        &mut snapshot.pages[0].items[1]
    else {
        panic!("first settings row is a value")
    };
    *value = ScalarValue::Bool(true);
    publish_snapshot(&probe, snapshot, cx);

    assert!(
        cx.debug_bounds("settings-toggle-appearance.option-0")
            .is_none(),
        "a value-only publication preserves the current list offset"
    );
}

#[gpui_kit::test]
fn notice_text_publications_preserve_the_content_scroll_position(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut snapshot = scrollable_snapshot();
    snapshot.pages[0].items.insert(
        1,
        SettingsPageItem::Setting(SettingsRow::Notice {
            text: "The active profile changed.".to_owned(),
            destructive: false,
        }),
    );
    let (probe, cx) =
        cx.add_window_view(|_, cx| ScrollbarProbe::with_snapshot(snapshot.clone(), cx));
    scroll_first_row_out_of_view(cx);

    let SettingsPageItem::Setting(SettingsRow::Notice { text, .. }) =
        &mut snapshot.pages[0].items[1]
    else {
        panic!("first settings row is a notice")
    };
    *text = "The active profile was reloaded.".to_owned();
    publish_snapshot(&probe, snapshot, cx);

    assert!(
        cx.debug_bounds("settings-toggle-appearance.option-0")
            .is_none(),
        "a notice value publication preserves the current list offset"
    );
}

#[gpui_kit::test]
fn query_changes_reset_scroll_even_when_the_same_rows_remain_visible(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| ScrollbarProbe::with_snapshot(scrollable_snapshot(), cx));
    scroll_first_row_out_of_view(cx);

    probe.update(cx, |probe, cx| {
        probe
            .settings
            .update(cx, |settings, cx| settings.apply_search("appearance", cx));
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("settings-toggle-appearance.option-0")
            .is_some(),
        "a different query starts its result view at the top"
    );
}

#[gpui_kit::test]
fn category_changes_reset_scroll_even_when_row_identities_match(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let mut initial = scrollable_snapshot();
    let mut general = initial.pages[0].clone();
    general.category = SettingsCategory::General;
    general.title = "General".to_owned();
    initial.pages.push(general);
    let (probe, cx) =
        cx.add_window_view(|_, cx| ScrollbarProbe::with_snapshot(initial.clone(), cx));
    scroll_first_row_out_of_view(cx);

    probe.update(cx, |probe, cx| {
        probe.settings.update(cx, |settings, cx| {
            settings.select_category(SettingsCategory::General, cx);
        });
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("settings-toggle-appearance.option-0")
            .is_some(),
        "a different category starts at the top even when its row identities match"
    );
}

#[gpui_kit::test]
fn visible_row_structure_changes_reset_scroll(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| ScrollbarProbe::with_snapshot(scrollable_snapshot(), cx));
    scroll_first_row_out_of_view(cx);

    let mut snapshot = scrollable_snapshot();
    snapshot.pages[0].items.swap(1, 2);
    publish_snapshot(&probe, snapshot, cx);

    assert!(
        cx.debug_bounds("settings-toggle-appearance.option-1")
            .is_some(),
        "a reordered visible row list starts at the top"
    );
}
