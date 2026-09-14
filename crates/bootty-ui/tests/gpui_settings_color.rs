#![cfg(test)]

use settings_support::GpuiSettingsSnapshot;
use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsControl, SettingsIntent, SettingsPage, SettingsPageItem,
    SettingsRow, UiPalette, init_theme,
};
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::{
    AppContext, Context, Entity, Focusable as _, Hsla, IntoElement, Modifiers, Render,
    Subscription, TestAppContext, Window, point, px,
};

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as ColorProbe;

impl ColorProbe {
    fn new(enabled: bool, cx: &mut Context<Self>) -> Self {
        let mut draft = settings_support::draft();
        assert!(draft.set_custom_value(
            "appearance.dark.colors.background",
            &ScalarValue::Text("#336699".to_owned())
        ));
        Self::with_draft(color_snapshot(enabled), draft, cx)
    }
}

#[gpui_kit::test]
fn actual_color_picker_accepts_every_hex_width_and_preserves_alpha(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(ActualPickerProbe::new);

    for (input, expected) in [
        ("#abc", "#AABBCC"),
        ("#abcd", "#AABBCCDD"),
        ("#102030", "#102030"),
        ("#10203040", "#10203040"),
    ] {
        cx.update(|window, cx| {
            let picker = probe.read(cx).picker.clone();
            picker.update(cx, |picker, cx| {
                assert!(picker.commit_hex(input, window, cx).is_some());
            });
        });
        probe.update(cx, |probe, _| {
            assert_eq!(
                rounded_hex(*probe.changes.borrow().last().unwrap()),
                expected
            );
        });
    }

    let change_count = probe.update(cx, |probe, _| probe.changes.borrow().len());
    cx.update(|window, cx| {
        let picker = probe.read(cx).picker.clone();
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.commit_hex("#12", window, cx), None);
        });
    });
    probe.update(cx, |probe, _| {
        assert_eq!(probe.changes.borrow().len(), change_count);
    });
}

#[gpui_kit::test]
fn color_picker_resets_through_the_typed_settings_intent(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ColorProbe::new(true, cx));
    assert!(
        cx.debug_bounds("settings-color-control-appearance.dark.colors.background-reset")
            .is_some(),
        "modified color exposes the reset affordance"
    );
    let reset = cx
        .debug_bounds("settings-color-control-appearance.dark.colors.background-reset")
        .expect("reset affordance is an interactive color control");
    cx.simulate_click(center(reset), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SettingsIntent::RemoveValue(id) if id == "appearance.dark.colors.background"
        )));
    });
}

#[gpui_kit::test]
fn disabled_color_picker_is_not_an_interactive_trigger(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| ColorProbe::new(false, cx));
    let disabled = cx
        .debug_bounds("settings-color-control-appearance.dark.colors.background-component")
        .expect("disabled color has a truthful disabled representation");

    cx.simulate_click(center(disabled), Modifiers::none());
    cx.simulate_keystrokes("enter");
    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().is_empty());
    });
}

#[gpui_kit::test]
fn color_picker_escape_and_click_away_dismiss_and_restore_the_trigger(cx: &mut TestAppContext) {
    init_zed_ui(cx);
    let (probe, cx) = cx.add_window_view(ActualPickerProbe::new);
    focus_picker(&probe, cx);

    cx.simulate_keystrokes("enter");
    assert!(picker_is_open(&probe, cx));

    cx.simulate_keystrokes("escape");
    assert!(!picker_is_open(&probe, cx), "Escape dismisses the popover");

    cx.simulate_keystrokes("enter");
    assert!(
        picker_is_open(&probe, cx),
        "focus returns to the trigger after Escape"
    );

    cx.simulate_click(point(px(500.0), px(500.0)), Modifiers::none());
    cx.run_until_parked();
    assert!(
        !picker_is_open(&probe, cx),
        "clicking outside dismisses the popover"
    );
    probe.update(cx, |probe, _| assert!(probe.changes.borrow().is_empty()));
}

fn init_zed_ui(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}

fn focus_picker(probe: &Entity<ActualPickerProbe>, cx: &mut gpui_kit::VisualTestContext) {
    cx.update(|window, cx| {
        let focus_handle = probe.read(cx).picker.focus_handle(cx);
        focus_handle.focus(window, cx);
        window.draw(cx).clear(cx);
    });
}

fn picker_is_open(probe: &Entity<ActualPickerProbe>, cx: &mut gpui_kit::VisualTestContext) -> bool {
    probe.update(cx, |probe, cx| probe.picker.read(cx).is_open())
}

struct ActualPickerProbe {
    picker: Entity<ColorPickerState>,
    changes: Rc<RefCell<Vec<Hsla>>>,
    _subscription: Subscription,
}

impl ActualPickerProbe {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker = cx.new(|cx| ColorPickerState::new(window, cx));
        let changes = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&changes);
        let subscription = cx.subscribe(&picker, move |_, _, event, _| {
            let ColorPickerEvent::Change(Some(color)) = event else {
                return;
            };
            received.borrow_mut().push(*color);
        });
        Self {
            picker,
            changes,
            _subscription: subscription,
        }
    }
}

impl Render for ActualPickerProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        ColorPicker::new(&self.picker).label("Color")
    }
}

fn rounded_hex(color: Hsla) -> String {
    let color = gpui_kit::Rgba::from(color);
    let channel = |value: f32| {
        format!("{:.0}", value.clamp(0.0, 1.0).mul_add(255.0, 0.0))
            .parse::<u8>()
            .expect("clamped color channel fits u8")
    };
    let red = channel(color.r);
    let green = channel(color.g);
    let blue = channel(color.b);
    let alpha = channel(color.a);
    if alpha < u8::MAX {
        format!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}")
    } else {
        format!("#{red:02X}{green:02X}{blue:02X}")
    }
}

fn center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(width.mul_add(0.5, left)), px(height.mul_add(0.5, top)))
}

fn color_snapshot(enabled: bool) -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance colors".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "colors".to_owned(),
                    title: "Colors".to_owned(),
                    search_terms: "colors background".to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: "appearance.dark.colors.background".to_owned(),
                    label: "Background".to_owned(),
                    help: "Terminal background color.".to_owned(),
                    value: ScalarValue::Text("#336699".to_owned()),
                    control: SettingsControl::Color,
                    enabled,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}
