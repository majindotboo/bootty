#![cfg(test)]

use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsControl, SettingsIntent, SettingsPage, SettingsPageItem,
    SettingsRow, UiPalette, init_theme,
};
use gpui_kit::{Context, Modifiers, TestAppContext, point};
use settings_support::GpuiSettingsSnapshot;

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as TextProbe;

impl TextProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_snapshot(snapshot(), cx)
    }
}

#[gpui_kit::test]
fn scalar_text_setting_uses_retained_component_input_and_emits_typed_writeback(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| TextProbe::new(cx));
    let input = cx
        .debug_bounds("settings-input-terminal.shell.program")
        .expect("scalar text input");
    cx.simulate_click(bounds_center(input), Modifiers::none());
    let command = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.simulate_keystrokes(&format!("{command}-a"));
    cx.simulate_input("/opt/homebrew/bin/fish");

    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().last(),
            Some(SettingsIntent::SetText { id, value })
                if id == "terminal.shell.program" && value == "/opt/homebrew/bin/fish"
        ));
    });
}

fn snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Terminal,
        pages: vec![SettingsPage {
            category: SettingsCategory::Terminal,
            title: "Terminal".to_owned(),
            search_terms: "terminal shell".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "shell".to_owned(),
                    title: "Shell".to_owned(),
                    search_terms: "shell program".to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::Value {
                    id: "terminal.shell.program".to_owned(),
                    label: "Shell program".to_owned(),
                    help: "Program used to start a terminal session.".to_owned(),
                    value: ScalarValue::Text("/bin/zsh".to_owned()),
                    control: SettingsControl::Text {
                        placeholder: "/bin/zsh".to_owned(),
                        optional: false,
                    },
                    enabled: true,
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}

fn bounds_center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(
        gpui_kit::px(width.mul_add(0.5, left)),
        gpui_kit::px(height.mul_add(0.5, top)),
    )
}
