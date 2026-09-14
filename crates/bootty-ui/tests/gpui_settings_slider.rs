#![cfg(test)]

use settings_support::GpuiSettingsSnapshot;
use std::ops::RangeInclusive;

use bootty_ui::gpui::{
    NumberControl, ScalarValue, SettingsCategory, SettingsControl, SettingsIntent, SettingsPage,
    SettingsPageItem, SettingsRow, UiPalette, init_theme, update_ui_font,
};

fn init_zed_ui(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}
use gpui_kit::{
    Bounds, Context, Modifiers, MouseButton, MouseUpEvent, Pixels, TestAppContext, point, px,
};

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as NumberProbe;

impl NumberProbe {
    fn new(
        enabled: bool,
        value: f32,
        range: RangeInclusive<f32>,
        precision: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_snapshot(number_snapshot(enabled, value, range, precision), cx)
    }
}

fn number_snapshot(
    enabled: bool,
    value: f32,
    range: RangeInclusive<f32>,
    precision: usize,
) -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance|colors|text|window|sidebar|status".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "text".to_owned(),
                    title: "Text".to_owned(),
                    search_terms:
                        "font|family|fallback|size|cell width|cell height|baseline|underline|glyph|features"
                            .to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: "text.font_size".to_owned(),
                    label: "Font size".to_owned(),
                    help: "The terminal font size.".to_owned(),
                    value: ScalarValue::Number(value),
                    control: SettingsControl::Number {
                        range,
                        control: NumberControl::Slider,
                        precision,
                        suffix: " px".to_owned(),
                        display_scale: 1.0,
                        optional: true,
                    },
                    enabled,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

#[gpui_kit::test]
fn dragging_slider_commits_only_the_released_value(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| NumberProbe::new(true, 12.0, 8.0..=24.0, 0, cx));
    let bounds = cx
        .debug_bounds("settings-slider-text.font_size")
        .expect("slider");
    let start = point(
        px(f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 4.0),
        bounds.center().y,
    );
    let end = point(
        px(f32::from(bounds.size.width).mul_add(0.75, f32::from(bounds.origin.x))),
        start.y,
    );
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
    cx.simulate_event(MouseUpEvent {
        position: end,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });
    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(matches!(intents.as_slice(), [SettingsIntent::SetValue { id, value: ScalarValue::Number(value) }] if id == "text.font_size" && *value > 12.0), "{intents:?}");
    });
}

#[gpui_kit::test]
fn slider_setting_uses_the_component_slider_and_number_input(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| NumberProbe::new(true, 3.185_855_2, 0.0..=8.0, 7, cx));
    assert!(cx.debug_bounds("settings-slider-text.font_size").is_some());
    let input = cx
        .debug_bounds("settings-number-input-text.font_size")
        .expect("slider setting has an editable component number input");
    cx.simulate_click(right_control(input), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SettingsIntent::SetValue {
                id,
                value: ScalarValue::Number(value),
            } if id == "text.font_size" && *value > 3.185_855_2
        )));
    });
}

#[gpui_kit::test]
fn number_input_width_tracks_live_ui_font_scaling(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| NumberProbe::new(true, 12.0, 8.0..=24.0, 0, cx));
    let initial = cx
        .debug_bounds("settings-number-input-text.font_size")
        .expect("number input is rendered");
    let initial_width: f32 = initial.size.width.into();

    cx.update(|_, cx| update_ui_font(&[], 20.0, cx));
    cx.refresh().expect("render the live UI font size");

    let scaled = cx
        .debug_bounds("settings-number-input-text.font_size")
        .expect("number input remains rendered after scaling");
    let scaled_width: f32 = scaled.size.width.into();
    assert_close(initial_width, 160.0, 1.0);
    assert_close(scaled_width, 200.0, 1.0);
}

#[gpui_kit::test]
fn number_input_keeps_room_for_value_and_suffix_at_large_ui_font(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| NumberProbe::new(true, 25.0, 0.0..=100.0, 0, cx));

    cx.update(|_, cx| update_ui_font(&[], 32.0, cx));
    cx.refresh().expect("render the large live UI font size");

    let bounds = cx
        .debug_bounds("settings-number-input-text.font_size")
        .expect("number input remains rendered at large UI font sizes");
    assert_close(bounds.size.width.into(), 320.0, 1.0);
}

#[gpui_kit::test]
fn slider_accepts_ranges_above_the_component_default_maximum(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (_, cx) = cx.add_window_view(|_, cx| NumberProbe::new(true, 274.0, 120.0..=600.0, 0, cx));

    assert!(cx.debug_bounds("settings-slider-text.font_size").is_some());
    assert!(
        cx.debug_bounds("settings-number-input-text.font_size")
            .is_some()
    );
}

#[gpui_kit::test]
fn number_stepper_reaches_the_exact_lower_bound(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| NumberProbe::new(true, 0.01, 0.0..=8.0, 2, cx));
    let input = cx
        .debug_bounds("settings-number-input-text.font_size")
        .expect("number setting has a component number input");
    cx.simulate_click(left_control(input), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SettingsIntent::SetValue {
                value: ScalarValue::Number(value),
                ..
            } if *value == 0.0
        )));
    });
}

#[gpui_kit::test]
fn disabled_number_input_does_not_emit_changes(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| NumberProbe::new(false, 12.0, 8.0..=24.0, 0, cx));
    let input = cx
        .debug_bounds("settings-number-input-text.font_size")
        .expect("disabled number control remains visible");
    cx.simulate_click(right_control(input), Modifiers::none());
    probe.update(cx, |probe, _| assert!(probe.intents.borrow().is_empty()));
}

fn left_control(bounds: Bounds<Pixels>) -> gpui_kit::Point<Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + 8.0), px(top + height / 2.0))
}

fn right_control(bounds: Bounds<Pixels>) -> gpui_kit::Point<Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width - 8.0), px(top + height / 2.0))
}

fn assert_close(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected}px ± {tolerance}px, got {actual}px"
    );
}
