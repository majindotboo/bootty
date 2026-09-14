#![cfg(test)]

use bootty_ui::gpui::{
    SettingsCategory, SettingsChoice, SettingsIntent, SettingsPage, SettingsPageItem, SettingsRow,
    StatusSegmentAlignment, StatusSegmentColor, StatusSegmentEditorRow, StatusSegmentIntent,
    StatusSegmentsSnapshot, UiPalette, init_theme,
};
use gpui_kit::{Context, Modifiers, TestAppContext, point, px};
use settings_support::GpuiSettingsSnapshot;

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as StatusProbe;

impl StatusProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(snapshot(), cx)
    }
}

#[gpui_kit::test]
fn status_segments_render_structured_fields_preview_and_lifecycle_controls(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| StatusProbe::new(cx));

    for selector in [
        "settings-status-segments-chrome.top-segment-preview",
        "settings-status-segment-chrome.top-segment-0-module",
        "settings-status-segment-chrome.top-segment-0-alignment",
        "settings-status-segment-chrome.top-segment-0-Foreground",
        "settings-status-segment-chrome.top-segment-0-Background",
        "settings-status-segment-chrome.top-segment-0-icon",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "missing {selector}");
    }

    let add = cx
        .debug_bounds("settings-status-segments-chrome.top-segment-add")
        .expect("add control");
    cx.simulate_click(center(add), Modifiers::none());
    let remove = cx
        .debug_bounds("status-segment-chrome.top-segment-0-remove")
        .expect("remove control");
    cx.simulate_click(center(remove), Modifiers::none());

    probe.update(cx, |probe, _| {
        let intents = probe.intents.borrow();
        assert!(intents.iter().any(|intent| matches!(
            intent,
            SettingsIntent::EditStatusSegments {
                id,
                edit: StatusSegmentIntent::Add { module },
            } if id == "chrome.top-segment" && module == "session"
        )));
        assert!(intents.iter().any(|intent| matches!(
            intent,
            SettingsIntent::EditStatusSegments {
                id,
                edit: StatusSegmentIntent::Remove { index: 0 },
            } if id == "chrome.top-segment"
        )));
    });
}

#[gpui_kit::test]
fn status_segment_color_picker_resets_through_a_typed_field_edit(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| StatusProbe::new(cx));
    let reset = cx
        .debug_bounds("settings-status-segment-chrome.top-segment-0-Foreground-reset")
        .expect("configured foreground color has a reset control");
    cx.simulate_click(center(reset), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert!(probe.intents.borrow().iter().any(|intent| matches!(
            intent,
            SettingsIntent::EditStatusSegments {
                id,
                edit: StatusSegmentIntent::SetColor {
                    index: 0,
                    field: StatusSegmentColor::Foreground,
                    value: None,
                },
            } if id == "chrome.top-segment"
        )));
    });
}

fn snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Panels,
        pages: vec![SettingsPage {
            category: SettingsCategory::Panels,
            title: "Panels".to_owned(),
            search_terms: "panels status".to_owned(),
            items: vec![SettingsPageItem::Setting(SettingsRow::StatusSegments(
                StatusSegmentsSnapshot {
                    id: "chrome.top-segment".to_owned(),
                    label: "Top bar modules".to_owned(),
                    help: "Modules rendered in the top bar.".to_owned(),
                    modules: ["session", "clock", "sysinfo"]
                        .into_iter()
                        .map(|module| SettingsChoice {
                            token: module.to_owned(),
                            label: module.to_owned(),
                            description: None,
                        })
                        .collect(),
                    segments: vec![StatusSegmentEditorRow {
                        module: "session".to_owned(),
                        alignment: StatusSegmentAlignment::Left,
                        foreground: Some("#abcdef".to_owned()),
                        background: None,
                        icon: Some("S".to_owned()),
                    }],
                    add_label: "Add top module".to_owned(),
                },
            ))],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(left + width / 2.0), px(top + height / 2.0))
}
