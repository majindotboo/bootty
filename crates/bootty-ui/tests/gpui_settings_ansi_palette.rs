#![cfg(test)]

use bootty_ui::gpui::{
    AnsiPalettePreset, SettingsCategory, SettingsIntent, SettingsPage, SettingsPageItem,
    SettingsRow, UiPalette, init_theme,
};
use gpui_kit::{Context, Modifiers, TestAppContext, point, px};
use settings_support::GpuiSettingsSnapshot;

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::SettingsProbe as PaletteProbe;

impl PaletteProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut draft = settings_support::draft();
        assert!(draft.set_ansi_palette(
            "appearance.dark.colors.palette",
            &["#102030".to_owned(), "#abcdef".to_owned()]
        ));
        Self::with_draft(snapshot(), draft, cx)
    }
}

#[gpui_kit::test]
fn ansi_palette_renders_indexed_controls_and_emits_typed_preset(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| PaletteProbe::new(cx));
    open_palette(cx);
    let preset = cx
        .debug_bounds("settings-ansi-palette-appearance.dark.colors.palette-preset-0")
        .expect("standard 16 preset button");
    let left: f32 = preset.origin.x.into();
    let top: f32 = preset.origin.y.into();
    let width: f32 = preset.size.width.into();
    let height: f32 = preset.size.height.into();
    cx.simulate_click(
        point(px(left + width / 2.0), px(top + height / 2.0)),
        Modifiers::none(),
    );

    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().as_slice(),
            [SettingsIntent::ReplaceAnsiPalette { id, colors }]
                if id == "appearance.dark.colors.palette"
                    && colors.as_slice() == ["#000000", "#ffffff"]
        ));
    });
}

#[gpui_kit::test]
fn ansi_palette_reset_is_explicitly_palette_wide(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| PaletteProbe::new(cx));
    open_palette(cx);

    assert!(
        cx.debug_bounds("settings-ansi-palette-appearance.dark.colors.palette-0-component")
            .is_some(),
        "ANSI slots render the gpui-component ColorPicker adapter"
    );
    assert!(
        cx.debug_bounds("settings-ansi-palette-appearance.dark.colors.palette-0-reset")
            .is_none()
    );
    let reset = cx
        .debug_bounds("settings-ansi-palette-appearance.dark.colors.palette-reset")
        .expect("the palette exposes a single whole-palette reset");
    cx.simulate_click(center(reset), Modifiers::none());

    probe.update(cx, |probe, _| {
        assert!(matches!(
            probe.intents.borrow().as_slice(),
            [SettingsIntent::ReplaceAnsiPalette { id, colors }]
                if id == "appearance.dark.colors.palette" && colors.is_empty()
        ));
    });
}

fn open_palette(cx: &mut gpui_kit::VisualTestContext) {
    let section = cx
        .debug_bounds("settings-section-ansi-palette")
        .expect("ANSI palette section navigation entry");
    cx.simulate_click(center(section), Modifiers::none());
}

fn center(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    let left: f32 = bounds.origin.x.into();
    let top: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    point(px(width.mul_add(0.5, left)), px(height.mul_add(0.5, top)))
}

fn snapshot() -> GpuiSettingsSnapshot {
    GpuiSettingsSnapshot {
        category: SettingsCategory::Appearance,
        pages: vec![SettingsPage {
            category: SettingsCategory::Appearance,
            title: "Appearance".to_owned(),
            search_terms: "appearance colors palette".to_owned(),
            items: vec![
                SettingsPageItem::SectionHeader {
                    id: "ansi-palette".to_owned(),
                    title: "ANSI Palette".to_owned(),
                    search_terms: "ansi palette colors".to_owned(),
                },
                SettingsPageItem::Setting(SettingsRow::AnsiPalette {
                    id: "appearance.dark.colors.palette".to_owned(),
                    label: "Dark ANSI palette".to_owned(),
                    help: "Override indexed terminal colors.".to_owned(),
                    colors: vec!["#102030".to_owned(), "#abcdef".to_owned()],
                    presets: vec![AnsiPalettePreset {
                        label: "Use standard 16".to_owned(),
                        colors: vec!["#000000".to_owned(), "#ffffff".to_owned()],
                    }],
                }),
            ],
        }],
        search: String::new(),
        write_error: None,
    }
}
