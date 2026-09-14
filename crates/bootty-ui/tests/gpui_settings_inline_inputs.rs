#![cfg(test)]

use settings_support::GpuiSettingsSnapshot;
use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    SettingsCategory, SettingsChoice, SettingsIntent, SettingsPage, SettingsPageItem, SettingsRow,
    StatusSegmentAlignment, StatusSegmentEditorRow, StatusSegmentIntent, StatusSegmentsSnapshot,
    UiPalette, init_theme,
};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, Modifiers, Pixels, TestAppContext, point, px,
};

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as InlineInputProbe;

impl InlineInputProbe {
    fn new(snapshot: GpuiSettingsSnapshot, cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(snapshot, cx)
    }
}

#[gpui_kit::test]
fn dynamic_list_and_status_inputs_emit_typed_changes_and_restore_content_focus(
    cx: &TestAppContext,
) {
    init(cx);
    let (probe, mut cx) = rooted_probe(cx, dynamic_snapshot());

    click(&mut cx, "settings-string-list-session.args-0");
    replace_focused_text(&mut cx, "--login");
    assert!(focused_input(&mut cx));
    cx.simulate_keystrokes("enter");
    assert!(
        !focused_input(&mut cx),
        "Enter returns focus to settings content"
    );

    click(&mut cx, "settings-status-segment-chrome.top-segment-0-icon");
    replace_focused_text(&mut cx, "terminal");
    assert!(focused_input(&mut cx));
    cx.simulate_keystrokes("escape");
    assert!(
        !focused_input(&mut cx),
        "Escape returns focus to settings content"
    );

    probe.update(&mut cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.iter().any(|intent| matches!(
            intent,
            SettingsIntent::SetStringListItem { id, index: 0, value }
                if id == "session.args" && value == "--login"
        )));
        assert!(intents.iter().any(|intent| matches!(
            intent,
            SettingsIntent::EditStatusSegments {
                id,
                edit: StatusSegmentIntent::SetIcon {
                    index: 0,
                    icon: Some(icon),
                },
            } if id == "chrome.top-segment" && icon == "terminal"
        )));
        assert!(
            !intents
                .iter()
                .any(|intent| matches!(intent, SettingsIntent::Close))
        );
    });
}

fn dynamic_snapshot() -> GpuiSettingsSnapshot {
    snapshot(vec![
        SettingsPageItem::Setting(SettingsRow::StringList {
            id: "session.args".to_owned(),
            label: "Shell arguments".to_owned(),
            help: "Arguments passed to the shell.".to_owned(),
            items: vec!["--interactive".to_owned()],
            options: Vec::new(),
            add_label: "Add argument".to_owned(),
            enabled: true,
        }),
        SettingsPageItem::Setting(SettingsRow::StatusSegments(StatusSegmentsSnapshot {
            id: "chrome.top-segment".to_owned(),
            label: "Top bar modules".to_owned(),
            help: "Modules rendered in the top bar.".to_owned(),
            modules: vec![SettingsChoice {
                token: "session".to_owned(),
                label: "session".to_owned(),
                description: None,
            }],
            segments: vec![StatusSegmentEditorRow {
                module: "session".to_owned(),
                alignment: StatusSegmentAlignment::Left,
                foreground: None,
                background: None,
                icon: Some("S".to_owned()),
            }],
            add_label: "Add top module".to_owned(),
        })),
    ])
}

fn snapshot(items: Vec<SettingsPageItem>) -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Advanced,
        pages: vec![SettingsPage {
            category: SettingsCategory::Advanced,
            title: "Advanced".to_owned(),
            search_terms: "settings inputs".to_owned(),
            items,
        }],
        search: String::new(),
        write_error: None,
    }
}

fn init(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}

fn rooted_probe(
    cx: &TestAppContext,
    snapshot: GpuiSettingsSnapshot,
) -> (Entity<InlineInputProbe>, gpui_kit::VisualTestContext) {
    let probe_slot = Rc::new(RefCell::new(None));
    let opened_probe = Rc::clone(&probe_slot);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), move |window, cx| {
            let probe = cx.new(|cx| InlineInputProbe::new(snapshot, cx));
            opened_probe.replace(Some(probe.clone()));
            cx.new(|cx| Root::new(probe, window, cx).bordered(false))
        })
        .expect("open rooted settings window")
    });
    let probe = probe_slot
        .borrow_mut()
        .take()
        .expect("capture settings probe");
    let cx = gpui_kit::VisualTestContext::from_window(window.into(), cx);
    (probe, cx)
}

fn click(cx: &mut gpui_kit::VisualTestContext, selector: &'static str) {
    let bounds = cx.debug_bounds(selector).expect("control exists");
    cx.simulate_click(center(bounds), Modifiers::none());
    cx.run_until_parked();
}

fn replace_focused_text(cx: &mut gpui_kit::VisualTestContext, text: &str) {
    let command = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.simulate_keystrokes(&format!("{command}-a"));
    cx.simulate_input(text);
}

fn focused_input(cx: &mut gpui_kit::VisualTestContext) -> bool {
    cx.update(|window, cx| window.focused_input(cx).is_some())
}

fn center(bounds: Bounds<Pixels>) -> gpui_kit::Point<Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width / 2.0), px(top + height / 2.0))
}
