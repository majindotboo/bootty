#![cfg(test)]

use std::{cell::RefCell, rc::Rc};

use bootty_ui::{
    gpui::{
        FontFeatureEditorEvent, FontFeatureEditorSnapshot, FontFeaturePreset,
        GpuiFontFeatureEditor, UiPalette, init_theme, update_ui_font,
    },
    settings_session::FontFeatureDraft,
};
use gpui_kit::{
    AppContext as _, Bounds, ClipboardItem, Context, Entity, IntoElement, Modifiers, Pixels, Point,
    Render, Subscription, TestAppContext, VisualTestContext, Window, point, px,
};
use pretty_assertions::assert_eq;

struct FontFeatureProbe {
    editor: Entity<GpuiFontFeatureEditor>,
    events: Rc<RefCell<Vec<FontFeatureEditorEvent>>>,
    _subscription: Subscription,
}

impl FontFeatureProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(snapshot(), cx)
    }

    fn with_snapshot(snapshot: FontFeatureEditorSnapshot, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| GpuiFontFeatureEditor::new(snapshot, cx));
        let events = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&events);
        let subscription = cx.subscribe(&editor, move |_, _, event, _| {
            received.borrow_mut().push(event.clone());
        });
        Self {
            editor,
            events,
            _subscription: subscription,
        }
    }
}

#[gpui_kit::test]
fn selecting_the_other_state_of_a_font_feature_replaces_the_existing_state(
    cx: &mut TestAppContext,
) {
    init(cx);
    let (probe, cx) =
        cx.add_window_view(|_, cx| FontFeatureProbe::with_snapshot(opposing_states(), cx));

    assert!(cx.debug_bounds("font-feature-selected-+liga").is_some());
    click(cx, "font-feature-combobox");
    cx.run_until_parked();
    click(cx, "font-feature-option--liga");
    cx.run_until_parked();

    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.events.borrow().last(),
            Some(&FontFeatureEditorEvent::Replace(vec![
                FontFeatureDraft::new("liga", 0).expect("disabled ligature feature")
            ]))
        );
    });
    assert!(cx.debug_bounds("font-feature-selected-+liga").is_none());
    assert!(cx.debug_bounds("font-feature-selected--liga").is_some());
}

#[gpui_kit::test]
fn font_feature_combobox_keeps_selection_single_line_and_footer_within_menu(
    cx: &mut TestAppContext,
) {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, cx| FontFeatureProbe::with_snapshot(dense_snapshot(), cx));

    assert_selected_chips_fit_trigger(cx);

    click(cx, "font-feature-combobox");
    cx.run_until_parked();
    assert_footer_children_fit(cx);

    cx.update(|_, cx| update_ui_font(&[], 24.0, cx));
    cx.refresh().expect("render the scaled UI font");
    cx.run_until_parked();
    assert_selected_chips_fit_trigger(cx);
    assert_footer_children_fit(cx);
}

impl Render for FontFeatureProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.editor.clone()
    }
}

#[gpui_kit::test]
fn arbitrary_four_character_tag_and_value_emit_a_typed_replacement(cx: &mut TestAppContext) {
    init(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| FontFeatureProbe::new(cx));
    click(cx, "font-feature-combobox");
    cx.run_until_parked();
    click(cx, "font-feature-tag");
    cx.simulate_keystrokes("c v 0 1");
    click(cx, "font-feature-value");
    replace_focused_text(cx, "2");
    cx.simulate_keystrokes("up enter");

    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.events.borrow().last(),
            Some(&FontFeatureEditorEvent::Replace(vec![
                FontFeatureDraft::new("cv01", 3).expect("character variant")
            ]))
        );
    });
}

#[gpui_kit::test]
fn presets_deduplicate_by_tag_and_clear_removes_every_feature(cx: &mut TestAppContext) {
    init(cx);
    let (probe, cx) = cx.add_window_view(|_, cx| FontFeatureProbe::new(cx));

    assert!(
        cx.debug_bounds("font-feature-selected-+cv01").is_some(),
        "the current feature is shown as a compact selected badge"
    );
    let clear = cx
        .debug_bounds("font-feature-clear")
        .expect("clear features button");
    let control = cx
        .debug_bounds("font-feature-combobox-control")
        .expect("font feature combobox control");
    assert!(
        clear.size.width < control.size.width,
        "clear features stays compact instead of stretching across the editor"
    );
    click(cx, "font-feature-combobox");
    cx.run_until_parked();
    click(cx, "font-feature-option-cv01=2");
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.events.borrow().last(),
            Some(&FontFeatureEditorEvent::Replace(vec![
                FontFeatureDraft::new("cv01", 2).expect("updated character variant")
            ]))
        );
    });
    assert!(
        cx.debug_bounds("font-feature-selected-+cv01").is_none(),
        "selecting another value removes the old chip for the same tag"
    );
    assert!(
        cx.debug_bounds("font-feature-selected-cv01=2").is_some(),
        "the selected chip matches the value that will be persisted"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    click(cx, "font-feature-clear");
    probe.update(cx, |probe, _| {
        assert_eq!(
            probe.events.borrow().last(),
            Some(&FontFeatureEditorEvent::Replace(Vec::new()))
        );
    });
}

fn snapshot() -> FontFeatureEditorSnapshot {
    FontFeatureEditorSnapshot {
        features: vec![FontFeatureDraft::new("cv01", 1).expect("character variant")],
        presets: vec![FontFeaturePreset {
            label: "cv01=2".to_owned(),
            feature: FontFeatureDraft::new("cv01", 2).expect("updated character variant"),
        }],
        enabled: true,
    }
}

fn opposing_states() -> FontFeatureEditorSnapshot {
    FontFeatureEditorSnapshot {
        features: vec![FontFeatureDraft::new("liga", 1).expect("enabled ligature feature")],
        presets: vec![FontFeaturePreset {
            label: "-liga".to_owned(),
            feature: FontFeatureDraft::new("liga", 0).expect("disabled ligature feature"),
        }],
        enabled: true,
    }
}

fn dense_snapshot() -> FontFeatureEditorSnapshot {
    FontFeatureEditorSnapshot {
        features: [
            ("a001", u32::MAX),
            ("a002", u32::MAX),
            ("a003", u32::MAX),
            ("a004", u32::MAX),
        ]
        .into_iter()
        .map(|(tag, value)| FontFeatureDraft::new(tag, value).expect("valid OpenType feature"))
        .collect(),
        presets: Vec::new(),
        enabled: true,
    }
}

fn init(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let Some(bounds) = cx.debug_bounds(selector) else {
        panic!("{selector} is rendered");
    };
    cx.simulate_click(center(bounds), Modifiers::none());
}

fn replace_focused_text(cx: &mut VisualTestContext, text: &str) {
    let command = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.simulate_keystrokes(&format!("{command}-a"));
    cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned())));
    cx.simulate_keystrokes(&format!("{command}-v"));
}

fn center(bounds: Bounds<Pixels>) -> Point<Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(width.mul_add(0.5, left)), px(height.mul_add(0.5, top)))
}

fn assert_selected_chips_fit_trigger(cx: &mut VisualTestContext) {
    let trigger = cx
        .debug_bounds("font-feature-combobox-control")
        .expect("font feature combobox trigger");
    let trigger_top: f32 = trigger.origin.y.into();
    let trigger_top_value: f32 = trigger.origin.y.into();
    let trigger_height: f32 = trigger.size.height.into();
    let trigger_bottom: f32 = trigger_height.mul_add(1.0, trigger_top_value);

    for selector in [
        "font-feature-selected-a001=4294967295",
        "font-feature-selected-a002=4294967295",
        "font-feature-selected-a003=4294967295",
        "font-feature-selected-a004=4294967295",
    ] {
        let chip = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"));
        let chip_top: f32 = chip.origin.y.into();
        let chip_top_value: f32 = chip.origin.y.into();
        let chip_height: f32 = chip.size.height.into();
        let chip_bottom: f32 = chip_height.mul_add(1.0, chip_top_value);
        assert!(
            chip_top >= trigger_top,
            "selected chip starts above the fixed-height trigger: trigger={trigger:?}, chip={chip:?}"
        );
        assert!(
            chip_bottom <= trigger_bottom + 1.0,
            "selected chip must stay inside the fixed-height trigger: trigger={trigger:?}, chip={chip:?}"
        );
    }
}

fn assert_footer_children_fit(cx: &mut VisualTestContext) {
    let footer = cx
        .debug_bounds("font-feature-footer")
        .expect("font feature popup footer");
    let footer_left: f32 = footer.origin.x.into();
    let footer_width: f32 = footer.size.width.into();
    let footer_right: f32 = footer_width.mul_add(1.0, footer_left);
    for selector in ["font-feature-tag", "font-feature-value", "font-feature-add"] {
        let child = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"));
        let child_left: f32 = child.origin.x.into();
        let child_width: f32 = child.size.width.into();
        let child_right: f32 = child_width.mul_add(1.0, child_left);
        assert!(
            child_right <= footer_right + 1.0,
            "{selector} extends beyond the responsive footer: footer={footer:?}, child={child:?}"
        );
    }
}
