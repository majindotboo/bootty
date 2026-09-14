#![cfg(test)]

use bootty_config::config::AppearanceVariant;
use bootty_ui::gpui::{UiPalette, chrome::Rgba, init_theme, update_theme};
use bootty_ui::theme::appearance_variant;
use gpui_kit::WindowAppearance;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(WindowAppearance::Light, AppearanceVariant::Light)]
#[case(WindowAppearance::VibrantLight, AppearanceVariant::Light)]
#[case(WindowAppearance::Dark, AppearanceVariant::Dark)]
#[case(WindowAppearance::VibrantDark, AppearanceVariant::Dark)]
fn system_window_appearance_selects_the_matching_branch(
    #[case] appearance: WindowAppearance,
    #[case] expected: AppearanceVariant,
) {
    assert_eq!(appearance_variant(appearance), expected);
}

#[gpui_kit::test]
fn component_tabs_and_file_selection_follow_palette_changes(cx: &gpui_kit::TestAppContext) {
    use gpui_kit::component::ActiveTheme as _;

    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        for tone in [Rgba::rgb(0, 0, 0), Rgba::rgb(110, 90, 70)] {
            let palette = UiPalette {
                tab_bar: tone,
                tab_inactive: tone,
                tab_active: tone,
                element_selected: tone,
                border_focused: tone,
                ..UiPalette::default()
            };
            update_theme(palette, cx);
            let expected: gpui_kit::Hsla = gpui_kit::rgba(u32::from_be_bytes([
                tone.red, tone.green, tone.blue, tone.alpha,
            ]))
            .into();
            assert_eq!(cx.theme().tab_bar, expected);
            assert_eq!(cx.theme().tab, expected);
            assert_eq!(cx.theme().tab_active, cx.theme().background);
            assert_eq!(cx.theme().list_active, expected);
            assert_eq!(cx.theme().list_active_border, expected);
        }
    });
}
