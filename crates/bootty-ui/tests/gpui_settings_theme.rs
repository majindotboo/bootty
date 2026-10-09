#![cfg(test)]

use bootty_ui::gpui::{
    ScalarValue, SettingsCategory, SettingsChoice, SettingsControl, SettingsIntent, SettingsPage,
    SettingsPageItem, SettingsRow, ThemeColorGroup, UiPalette, init_theme,
    theme_colors_from_document, update_ui_font,
};
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, IntoElement, Modifiers, Render, ScrollDelta,
    ScrollWheelEvent, TestAppContext, TouchPhase, Window, point,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[path = "support/settings.rs"]
mod settings_support;
use settings_support::{GpuiSettingsSnapshot, SettingsProbe};

#[rstest]
#[case("appearance.light.colors.background", ThemeColorGroup::Terminal)]
#[case(
    "appearance.dark.colors.selection-background",
    ThemeColorGroup::Selection
)]
#[case("appearance.dark.colors.pointer-foreground", ThemeColorGroup::Cursor)]
#[case("appearance.light.colors.palette", ThemeColorGroup::Palette)]
#[case("chrome.pane-focus-border-color", ThemeColorGroup::Window)]
#[case("sidebar.hover", ThemeColorGroup::Sidebar)]
#[case("appearance.dark.colors.tektronix-cursor", ThemeColorGroup::Graphics)]
fn theme_group_retains_the_setting_path_and_appearance_scope(
    #[case] id: &str,
    #[case] group: ThemeColorGroup,
) {
    assert_eq!(ThemeColorGroup::for_setting(id), Some(group));
    assert_eq!(
        bootty_ui::settings_category_for(id, "colors"),
        SettingsCategory::Theme
    );
}

#[rstest]
fn preview_reads_uncommitted_colors_and_reset_restores_the_selected_theme() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "# Existing config stays unchanged during preview\n").unwrap();
    let mut document = bootty_config::config::load_or_create_config_document(&path).unwrap();
    let original = std::fs::read_to_string(&path).unwrap();
    let base = bootty_config::config::ColorConfig {
        background: Some(bootty_config::color::Color::from_hex("#123456").unwrap()),
        ..Default::default()
    };
    document
        .set_str(
            &["appearance", "light", "colors", "background"],
            "#abcdef80",
        )
        .unwrap();
    assert_eq!(
        theme_colors_from_document(&document, "appearance.light.colors", &base).background,
        Some(bootty_config::color::Color::from_hex("#abcdef80").unwrap())
    );
    assert_eq!(
        theme_colors_from_document(&document, "appearance.dark.colors", &base),
        base
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    document
        .remove(&["appearance", "light", "colors", "background"])
        .unwrap();
    assert_eq!(
        theme_colors_from_document(&document, "appearance.light.colors", &base),
        base
    );
}

fn theme_snapshot() -> GpuiSettingsSnapshot {
    let color = |id: &str, label: &str| {
        SettingsPageItem::Setting(SettingsRow::Value {
            id: id.to_owned(),
            label: label.to_owned(),
            help: "Selected theme default when unset".to_owned(),
            value: ScalarValue::Text("#336699".to_owned()),
            control: SettingsControl::Color,
            enabled: true,
        })
    };
    GpuiSettingsSnapshot {
        category: SettingsCategory::Theme,
        search: String::new(),
        write_error: None,
        pages: vec![SettingsPage {
            category: SettingsCategory::Theme,
            title: "Theme".to_owned(),
            search_terms: "theme colors light dark".to_owned(),
            items: vec![
                color("appearance.dark.colors.background", "Background"),
                color(
                    "appearance.dark.colors.selection-background",
                    "Selection background",
                ),
                color("appearance.light.colors.background", "Background"),
                color(
                    "appearance.light.colors.selection-background",
                    "Selection background",
                ),
            ],
        }],
    }
}

#[gpui_kit::test]
fn grouped_theme_colors_keep_exact_reset_scope_and_preview_visible(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, cx) = cx.add_window_view(|_, cx| {
        let mut draft = settings_support::draft();
        assert!(draft.set_custom_value(
            "appearance.dark.colors.selection-background",
            &ScalarValue::Text("#336699".to_owned())
        ));
        SettingsProbe::with_draft(theme_snapshot(), draft, cx)
    });
    assert!(cx.debug_bounds("theme-live-preview").is_some());
    assert!(
        cx.debug_bounds("settings-color-control-appearance.dark.colors.background-component")
            .is_some()
    );
    assert!(
        cx.debug_bounds(
            "settings-color-control-appearance.dark.colors.selection-background-component"
        )
        .is_none()
    );
    let group = cx
        .debug_bounds("theme-group-Selection")
        .expect("Selection group");
    cx.simulate_click(group.center(), Modifiers::none());
    cx.refresh().expect("show selected group");
    assert!(
        cx.debug_bounds("settings-color-control-appearance.dark.colors.background-component")
            .is_none()
    );
    let reset = cx
        .debug_bounds("settings-color-control-appearance.dark.colors.selection-background-reset")
        .expect("Exact scoped reset");
    cx.simulate_click(reset.center(), Modifiers::none());
    probe.update(cx,|probe,_| { assert!(probe.intents.borrow().iter().any(|intent|matches!(intent,SettingsIntent::RemoveValue(id) if id == "appearance.dark.colors.selection-background"))); });
}

#[gpui_kit::test]
fn theme_palette_and_preview_fit_minimum_width_at_each_ui_scale(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_, visual) = cx.add_window_view(|_, cx| {
        let mut snapshot = theme_snapshot();
        add_theme_choices(&mut snapshot);
        snapshot
            .pages
            .first_mut()
            .unwrap()
            .items
            .push(SettingsPageItem::Setting(SettingsRow::AnsiPalette {
                id: "appearance.dark.colors.palette".to_owned(),
                label: "ANSI palette".to_owned(),
                help: String::new(),
                colors: vec!["#336699".to_owned(); 16],
                presets: Vec::new(),
            }));
        SettingsProbe::with_snapshot(snapshot, cx)
    });
    let group = visual
        .debug_bounds("theme-group-Palette")
        .expect("Palette group");
    visual.simulate_click(group.center(), Modifiers::none());
    for (rem, width_in_rems, stacked) in [
        (16.0, 39.125, true),
        (24.0, 39.125, true),
        (16.0, 48.0, true),
        (16.0, 49.0, false),
        (16.0, 60.0, false),
    ] {
        visual.update(|window, cx| {
            update_ui_font(&[], rem, cx);
            window.set_rem_size(gpui_kit::px(rem));
        });
        visual.simulate_resize(gpui_kit::size(
            gpui_kit::px(rem * width_in_rems),
            gpui_kit::px(1600.0),
        ));
        visual.refresh().expect("Render responsive theme layout");
        let editor = visual
            .debug_bounds("theme-colors-editor")
            .expect("Theme editor");
        let preview = visual
            .debug_bounds("theme-live-preview")
            .expect("Theme preview");
        let viewport = visual.update(|window, _| window.viewport_size());
        assert!(editor.left() >= gpui_kit::px(0.0));
        assert!(editor.right() <= viewport.width);
        assert!(preview.left() >= gpui_kit::px(0.0));
        assert!(preview.right() <= viewport.width);
        for id in [
            "settings-choice-appearance.mode",
            "settings-choice-appearance.light.theme",
            "settings-choice-appearance.dark.theme",
        ] {
            let selector = visual
                .debug_bounds(id)
                .expect("Theme selector remains visible");
            assert!(selector.left() >= editor.left());
            assert!(selector.right() <= editor.right());
        }
        let mode = visual
            .debug_bounds("settings-choice-appearance.mode")
            .unwrap();
        let light = visual
            .debug_bounds("settings-choice-appearance.light.theme")
            .unwrap();
        let dark = visual
            .debug_bounds("settings-choice-appearance.dark.theme")
            .unwrap();
        assert_eq!(light.top(), dark.top(), "Light and dark themes stay paired");
        assert_eq!(
            light.size, dark.size,
            "Theme choices share the same geometry"
        );
        assert!(mode.bottom() < light.top(), "Mode has its own row");
        assert!(light.right() < dark.left());
        if stacked {
            assert!(
                preview.top() >= editor.bottom(),
                "The preview follows the editor at minimum width"
            );
        } else {
            assert!(
                preview.left() >= editor.right(),
                "Wide windows retain the side-by-side preview"
            );
        }
    }
}

struct NamedThemeKeyboardProbe {
    editor: Entity<bootty_ui::gpui::GpuiThemeEditor>,
    settings: Entity<SettingsProbe>,
}

impl NamedThemeKeyboardProbe {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = cx.new(|cx| SettingsProbe::with_snapshot(theme_snapshot(), cx));
        let shared = settings.read(cx).settings.clone();
        let (sender, receiver) = bootty_control::app_command_channel(1, std::sync::Arc::new(|| {}));
        // Loading failure must leave keyboard navigation and dismissal usable.
        drop(receiver);
        let editor = cx.new(|cx| {
            bootty_ui::gpui::GpuiThemeEditor::new(
                shared,
                sender.for_caller(bootty_control::Caller::Internal),
                "Draft".to_owned(),
                bootty_config::config::AppearanceVariant::Dark,
                bootty_config::config::ColorConfig::default(),
                window,
                cx,
            )
        });
        Self { editor, settings }
    }
}

impl Render for NamedThemeKeyboardProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.editor.clone()
    }
}

#[gpui_kit::test]
fn named_theme_editor_stacks_and_scrolls_with_the_preview_at_narrow_width(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (_, visual) = cx.add_window_view(NamedThemeKeyboardProbe::new);
    let named = visual
        .debug_bounds("theme-editor-mode-true")
        .expect("Named authoring mode");
    visual.simulate_click(named.center(), Modifiers::none());
    visual.run_until_parked();

    for (rem, width_in_rems, stacked, verify_scroll) in [
        (16.0, 39.125, true, true),
        (24.0, 39.125, true, false),
        (16.0, 60.0, false, false),
    ] {
        visual.update(|window, cx| {
            update_ui_font(&[], rem, cx);
            window.set_rem_size(gpui_kit::px(rem));
        });
        visual.simulate_resize(gpui_kit::size(
            gpui_kit::px(rem * width_in_rems),
            gpui_kit::px(360.0),
        ));
        visual
            .refresh()
            .expect("Render responsive named theme layout");

        let layout = visual
            .debug_bounds("named-theme-layout-scroll")
            .expect("Named theme layout");
        let editor = visual
            .debug_bounds("named-theme-scroll")
            .expect("Named theme editor");
        let preview = visual
            .debug_bounds("theme-live-preview")
            .expect("Theme preview");
        let viewport = visual.update(|window, _| window.viewport_size());
        assert!(layout.left() >= gpui_kit::px(0.0));
        assert!(layout.right() <= viewport.width);
        assert!(editor.left() >= layout.left());
        assert!(editor.right() <= layout.right());
        assert!(preview.left() >= layout.left());
        assert!(preview.right() <= layout.right());

        if stacked {
            assert!(
                preview.top() >= editor.bottom(),
                "The preview follows the editor at minimum width"
            );
        } else {
            assert!(
                preview.left() >= editor.right(),
                "Wide windows retain the side-by-side preview"
            );
        }

        if verify_scroll {
            let preview_top = preview.top();
            visual.simulate_event(ScrollWheelEvent {
                position: gpui_kit::Bounds::new(
                    point(gpui_kit::px(0.0), gpui_kit::px(0.0)),
                    viewport,
                )
                .center(),
                delta: ScrollDelta::Lines(point(0.0, -18.0)),
                modifiers: Modifiers::none(),
                touch_phase: TouchPhase::Moved,
            });
            visual.run_until_parked();
            assert!(
                visual
                    .debug_bounds("theme-live-preview")
                    .expect("Theme preview after scrolling")
                    .top()
                    < preview_top,
                "Scrolling the narrow layout also moves the preview: layout={layout:?}, editor={editor:?}, preview={preview:?}, viewport={viewport:?}"
            );
        }
    }
}

#[gpui_kit::test]
fn named_theme_first_entry_focuses_name_and_escape_uses_shared_close_intent(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, visual) = cx.add_window_view(NamedThemeKeyboardProbe::new);
    let named = visual
        .debug_bounds("theme-editor-mode-true")
        .expect("Named authoring mode");
    visual.simulate_click(named.center(), Modifiers::none());
    visual.run_until_parked();
    visual.update(|window, cx| {
        let editor = probe.read(cx).editor.clone();
        assert!(
            editor.focus_handle(cx).is_focused(window),
            "The first rendered named input owns keyboard focus"
        );
    });
    visual.simulate_keystrokes("escape");
    probe.update(visual, |probe, cx| {
        assert!(matches!(
            probe.settings.read(cx).intents.borrow().as_slice(),
            [SettingsIntent::Close]
        ));
    });
}

#[gpui_kit::test]
fn named_theme_color_popup_escape_precedes_editor_dismissal(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let (probe, visual) = cx.add_window_view(NamedThemeKeyboardProbe::new);
    let named = visual
        .debug_bounds("theme-editor-mode-true")
        .expect("Named authoring mode");
    visual.simulate_click(named.center(), Modifiers::none());
    visual.run_until_parked();
    let color = visual
        .debug_bounds("theme-author-color-background-component")
        .expect("Named color picker");
    visual.simulate_click(color.center(), Modifiers::none());
    visual.run_until_parked();
    visual.simulate_keystrokes("escape");
    probe.update(visual, |probe, cx| {
        assert_eq!(
            probe.settings.read(cx).intents.borrow().len(),
            0,
            "The popup consumes its first Escape"
        );
    });
    visual.simulate_keystrokes("escape");
    probe.update(visual, |probe, cx| {
        assert!(matches!(
            probe.settings.read(cx).intents.borrow().as_slice(),
            [SettingsIntent::Close]
        ));
    });
}

fn add_theme_choices(snapshot: &mut GpuiSettingsSnapshot) {
    for (id, label) in [
        ("appearance.mode", "Mode"),
        ("appearance.light.theme", "Light theme"),
        ("appearance.dark.theme", "Dark theme"),
    ] {
        let choices = vec![SettingsChoice {
            token: "default".to_owned(),
            label: "Default".to_owned(),
            description: None,
        }];
        snapshot
            .pages
            .first_mut()
            .unwrap()
            .items
            .push(SettingsPageItem::Setting(SettingsRow::Value {
            id: id.to_owned(),
            label: label.to_owned(),
            help: "Theme selection follows the application appearance and retains color overrides."
                .to_owned(),
            value: ScalarValue::Token("default".to_owned()),
            control: if id == "appearance.mode" {
                SettingsControl::Choice(choices)
            } else {
                SettingsControl::Theme(choices)
            },
            enabled: true,
        }));
    }
}
