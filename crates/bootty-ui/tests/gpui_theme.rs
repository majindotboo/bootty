#![cfg(test)]

use bootty_ui::gpui::{
    Rgba, UiPalette, UiTheme, init_theme, init_ui_theme, readable_color, setup_ui_font,
    update_ui_font, update_ui_font_families, update_ui_font_size, update_ui_font_weights,
    update_ui_theme,
};
use gpui_kit::{AppContext as _, Empty, FontWeight, TestAppContext, px};
use num_traits::ToPrimitive as _;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[gpui_kit::test]
fn live_font_updates_preserve_fallbacks_and_window_scale(cx: &TestAppContext) {
    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        bootty_ui::assets::BoottyAssets
            .load_fonts(cx.text_system())
            .unwrap();
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            let default = setup_ui_font(window, cx);
            assert_eq!(default.family.as_ref(), "IBM Plex Sans");
            assert_eq!(default.weight, FontWeight::NORMAL);
            assert_eq!(window.rem_size(), px(16.0));
            update_ui_font(
                &["Lilex-Regular".to_owned(), ".ZedSans".to_owned()],
                18.0,
                cx,
            );
            let font = setup_ui_font(window, cx);
            assert_eq!(
                font.family,
                gpui_kit::component::Theme::global(cx).font_family
            );
            assert_eq!(
                font.fallbacks.as_ref().expect("fallback").fallback_list(),
                &["IBM Plex Sans".to_owned()]
            );
            assert_eq!(font.features.tag_value_list(), &[("calt".to_owned(), 0)]);
            assert_eq!(window.rem_size(), px(18.0));
            update_ui_font_size(19.0, cx);
            update_ui_font_families(&["Lilex-Bold".to_owned()], cx);
            update_ui_theme(
                UiTheme {
                    palette: UiPalette::default(),
                },
                cx,
            );
            let changed_font = setup_ui_font(window, cx);
            assert_eq!(window.rem_size(), px(19.0));
            assert_eq!(
                gpui_kit::component::Theme::global(cx).font_family,
                changed_font.family
            );
            assert_eq!(gpui_kit::component::Theme::global(cx).font_size, px(19.0));
            update_ui_font(&[], 17.0, cx);
            let font = setup_ui_font(window, cx);
            assert_eq!(font.weight, FontWeight::NORMAL);
            assert!(font.fallbacks.is_none());
            assert_eq!(window.rem_size(), px(17.0));
            cx.new(|_| Empty)
        })
        .expect("font probe window");
    });
}

#[gpui_kit::test]
fn custom_theme_switch_resets_editor_and_component_surfaces(cx: &TestAppContext) {
    cx.update(|cx| {
        let dark = UiPalette::from_terminal_colors(
            Some(Rgba::rgb(0x20, 0x22, 0x26)),
            Some(Rgba::rgb(0xe8, 0xea, 0xed)),
            [None; 16],
        );
        init_ui_theme(UiTheme { palette: dark }, cx);
        assert_eq!(
            gpui_kit::component::Theme::global(cx).mode,
            gpui_kit::component::ThemeMode::Dark
        );
        assert_eq!(
            gpui_kit::component::Theme::global(cx).colors.skeleton,
            gpui_kit::component::ThemeColor::dark().skeleton,
        );

        let light = UiPalette::from_terminal_colors(
            Some(Rgba::rgb(0xef, 0xf1, 0xf5)),
            Some(Rgba::rgb(0x4c, 0x4f, 0x69)),
            [None; 16],
        );
        update_ui_theme(UiTheme { palette: light }, cx);

        let component = gpui_kit::component::Theme::global(cx);
        assert_eq!(component.mode, gpui_kit::component::ThemeMode::Light);
        assert_component_color(component.colors.background, light.base);
        assert_component_color(component.colors.sidebar, light.pane);
        assert_component_color(component.colors.foreground, light.text);
        assert_component_color(component.colors.button_secondary_foreground, light.text);
        assert_component_color(component.colors.group_box, light.surface);
        assert_eq!(
            component.colors.skeleton,
            gpui_kit::component::ThemeColor::light().skeleton
        );
        assert_eq!(
            component.colors.overlay,
            gpui_kit::component::ThemeColor::light().overlay
        );
    });
}

#[gpui_kit::test]
fn component_chrome_colors_and_tokens_follow_the_bootty_palette(cx: &TestAppContext) {
    cx.update(|cx| {
        let palette = UiPalette::default();
        init_theme(palette, cx);

        let theme = gpui_kit::component::Theme::global(cx);
        assert_component_color(theme.colors.tab_bar, palette.tab_bar);
        assert_component_color(theme.colors.tab, palette.tab_inactive);
        assert_component_color(theme.colors.tab_foreground, palette.muted);
        assert_component_color(theme.colors.tab_active, palette.tab_active);
        assert_component_color(theme.colors.tab_active_foreground, palette.text);
        assert_component_color(theme.colors.title_bar, palette.tab_bar);
        assert_component_color(theme.colors.title_bar_border, palette.border);
        assert_component_color(theme.colors.sidebar, palette.pane);
        assert_component_color(theme.colors.sidebar_accent, palette.element_selected);
        assert_component_color(theme.colors.status_bar, palette.mantle);
        assert_component_color(theme.colors.scrollbar_thumb, palette.border);
        assert_component_color(theme.colors.secondary_foreground, palette.text);
        assert_component_color(theme.colors.slider_bar, palette.muted);
        assert_component_color(theme.colors.slider_thumb, palette.text);
        assert_eq!(
            theme.colors.overlay,
            gpui_kit::component::ThemeColor::dark().overlay
        );

        assert_eq!(theme.tokens.tab_bar.color, theme.colors.tab_bar);
        assert_eq!(theme.tokens.tab.color, theme.colors.tab);
        assert_eq!(theme.tokens.tab_active.color, theme.colors.tab_active);
        assert_eq!(theme.tokens.title_bar.color, theme.colors.title_bar);
        assert_eq!(theme.tokens.sidebar.color, theme.colors.sidebar);
        assert_eq!(theme.tokens.status_bar.color, theme.colors.status_bar);
        assert_eq!(
            theme.tokens.scrollbar_thumb.color,
            theme.colors.scrollbar_thumb
        );
    });
}

#[gpui_kit::test]
fn component_chrome_tokens_refresh_when_the_palette_changes(cx: &TestAppContext) {
    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        let palette = UiPalette::from_terminal_colors(
            Some(Rgba::rgb(0xe8, 0xea, 0xed)),
            Some(Rgba::rgb(0x20, 0x22, 0x26)),
            [None; 16],
        );
        update_ui_theme(UiTheme { palette }, cx);

        let theme = gpui_kit::component::Theme::global(cx);
        assert_component_color(theme.tokens.tab_bar.color, palette.tab_bar);
        assert_component_color(theme.tokens.title_bar.color, palette.tab_bar);
        assert_component_color(theme.tokens.sidebar.color, palette.pane);
        assert_component_color(theme.tokens.status_bar.color, palette.mantle);
        assert_component_color(theme.tokens.scrollbar_thumb.color, palette.border);
        assert_component_color(theme.tokens.slider_bar.color, palette.muted);
        assert_component_color(theme.tokens.slider_thumb.color, palette.text);
    });
}

fn assert_component_color(actual: gpui_kit::Hsla, expected: Rgba) {
    assert_eq!(
        rgba8(actual),
        [expected.red, expected.green, expected.blue, expected.alpha]
    );
}

fn rgba8(color: gpui_kit::Hsla) -> [u8; 4] {
    let color = gpui_kit::Rgba::from(color);
    let channel = |value: f32| {
        (value * 255.0)
            .round()
            .clamp(0.0, 255.0)
            .to_u8()
            .unwrap_or(0)
    };
    [
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a),
    ]
}

#[rstest]
fn palette_exposes_distinct_control_state_roles() {
    let mut terminal = [None; 16];
    terminal[4] = Some(Rgba::rgb(0x74, 0xb9, 0xff));
    let palette = UiPalette::from_terminal_colors(
        Some(Rgba::rgb(0x18, 0x1a, 0x1f)),
        Some(Rgba::rgb(0xe8, 0xea, 0xed)),
        terminal,
    );

    assert_eq!(palette.element_background, palette.surface);
    assert_eq!(palette.element_hover, palette.hover);
    assert_eq!(palette.element_active, palette.ghost_element_active);
    assert_eq!(palette.text_placeholder, palette.muted);
    assert_eq!(palette.icon, palette.text);
    assert_eq!(palette.icon_accent, palette.accent);
    assert_eq!(palette.tab_active, palette.base);
    assert_eq!(palette.tab_inactive, palette.base);
    assert_eq!(palette.border_selected, palette.border_focused);
}

#[rstest]
fn terminal_derived_surfaces_follow_the_base_luminance() {
    let dark = UiPalette::from_terminal_colors(
        Some(Rgba::rgb(0x18, 0x1a, 0x1f)),
        Some(Rgba::rgb(0xe8, 0xea, 0xed)),
        [None; 16],
    );
    let light = UiPalette::from_terminal_colors(
        Some(Rgba::rgb(0xec, 0xed, 0xef)),
        Some(Rgba::rgb(0x1d, 0x1e, 0x20)),
        [None; 16],
    );

    assert!(dark.mantle.red < dark.base.red);
    assert!(dark.surface.red > dark.base.red);
    assert!(light.mantle.red > light.base.red);
    assert!(light.surface.red < light.base.red);
}

#[rstest]
fn readable_color_preserves_a_preferred_hue_when_adjusting_contrast() {
    let preferred = Rgba::rgb(0x30, 0x58, 0x80);
    let adjusted = readable_color(Rgba::rgb(0x14, 0x16, 0x1b), preferred);

    assert!(adjusted.red >= preferred.red);
    assert!(adjusted.green >= preferred.green);
    assert!(adjusted.blue >= preferred.blue);
    assert_ne!(adjusted, Rgba::rgb(u8::MAX, u8::MAX, u8::MAX));
}

#[gpui_kit::test]
fn live_ui_weight_assignment_publishes_a_new_family_to_kit(cx: &TestAppContext) {
    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        bootty_ui::assets::BoottyAssets
            .load_fonts(cx.text_system())
            .unwrap();
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            update_ui_font_families(&["Lilex-Regular".to_owned()], cx);
            let old_bold = setup_ui_font(window, cx).bold();

            let weights = [(
                bootty_config::FontWeightRole::Bold,
                bootty_config::FontStyleAssignment::Named("Regular".to_owned()),
            )]
            .into();
            update_ui_font_weights(&weights, cx);
            let current = setup_ui_font(window, cx);
            assert_ne!(current.family, old_bold.family);
            assert_eq!(current.weight, FontWeight::NORMAL);
            assert_eq!(
                current.family,
                gpui_kit::component::Theme::global(cx).font_family
            );
            cx.new(|_| Empty)
        })
        .unwrap();
    });
}
