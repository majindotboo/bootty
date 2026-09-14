#![cfg(test)]

use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsControl, SettingsIntent, SettingsPage, SettingsPageItem,
    SettingsRow, UiPalette, init_theme,
};
use settings_support::GpuiSettingsSnapshot;

fn init_zed_ui(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}
use gpui_kit::{Context, Modifiers, TestAppContext, point, px};

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as ToggleProbe;

impl ToggleProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_enabled(true, cx)
    }

    fn with_enabled(enabled: bool, cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(toggle_snapshot(enabled), cx)
    }
}

fn toggle_snapshot(enabled: bool) -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance|colors|text|window|sidebar|status".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "appearance".to_owned(),
                    title: "Appearance".to_owned(),
                    search_terms:
                        "cursor|blink|inactive pane|mouse pointer|hide while typing|fullscreen notch"
                            .to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: "cursor.blink".to_owned(),
                    label: "Blink cursor".to_owned(),
                    help: "Make the default cursor blink.".to_owned(),
                    value: ScalarValue::Bool(true),
                    control: SettingsControl::Toggle,
                    enabled,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

#[gpui_kit::test]
fn boolean_setting_renders_as_a_switch_and_emits_a_boolean(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ToggleProbe::new(cx));
    let bounds = cx
        .debug_bounds("settings-toggle-cursor.blink")
        .expect("boolean setting is rendered as a switch");
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();

    cx.simulate_click(
        point(px(left + width / 2.0), px(top + height / 2.0)),
        Modifiers::none(),
    );

    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            matches!(
                intents.as_slice(),
                [SettingsIntent::SetValue {
                    id,
                    value: ScalarValue::Bool(false),
                }] if id == "cursor.blink"
            ),
            "unexpected toggle intents: {intents:#?}"
        );
    });
}

#[gpui_kit::test]
fn boolean_setting_enter_activation_emits_once(cx: &mut TestAppContext) {
    assert_keyboard_activation_emits_once(cx, "enter");
}

#[gpui_kit::test]
fn boolean_setting_space_activation_emits_once(cx: &mut TestAppContext) {
    assert_keyboard_activation_emits_once(cx, "space");
}

#[gpui_kit::test]
fn disabled_boolean_setting_ignores_pointer_and_keyboard_activation(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ToggleProbe::with_enabled(false, cx));
    let bounds = cx
        .debug_bounds("settings-toggle-cursor.blink")
        .expect("disabled boolean setting remains visible");

    cx.simulate_click(bounds_center(bounds), Modifiers::none());
    cx.simulate_keystrokes("enter space");

    probe.update(cx, |probe, _| {
        assert!(
            probe.intents.borrow().is_empty(),
            "disabled toggle emitted an intent"
        );
    });
}

fn assert_keyboard_activation_emits_once(cx: &mut TestAppContext, keystroke: &str) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ToggleProbe::new(cx));
    let bounds = cx
        .debug_bounds("settings-toggle-cursor.blink")
        .expect("boolean setting is rendered as a switch");

    // Clicking first gives the switch focus. Clear the click intent so the assertion covers only
    // the keyboard activation under test.
    cx.simulate_click(bounds_center(bounds), Modifiers::none());
    probe.update(cx, |probe, _| probe.intents.borrow_mut().clear());
    cx.simulate_keystrokes(keystroke);
    cx.simulate_event(gpui_kit::KeyUpEvent {
        keystroke: gpui_kit::Keystroke::parse(keystroke).unwrap(),
    });

    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(
            matches!(
                intents.as_slice(),
                [SettingsIntent::SetValue {
                    id,
                    value: ScalarValue::Bool(false),
                }] if id == "cursor.blink"
            ),
            "unexpected {keystroke:?} toggle intents: {intents:#?}"
        );
    });
}

fn bounds_center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width / 2.0), px(top + height / 2.0))
}
