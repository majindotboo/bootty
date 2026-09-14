#![cfg(test)]

use pretty_assertions::assert_eq;

#[gpui_kit::test]
fn window_options_preserve_decoration_ownership(cx: &gpui_kit::TestAppContext) {
    use bootty_config::config::{BoottyConfig, WindowDecoration};
    use gpui_kit::WindowDecorations;

    for (preference, expected) in [
        (WindowDecoration::Auto, WindowDecorations::Server),
        (WindowDecoration::Server, WindowDecorations::Server),
        (WindowDecoration::Client, WindowDecorations::Client),
        (WindowDecoration::None, WindowDecorations::Client),
    ] {
        let mut config = BoottyConfig::default();
        config.window.window_decoration = preference;
        let options = cx.update(|cx| bootty_ui::platform::native_options_for_config(&config, cx));
        assert_eq!(options.window_decorations, Some(expected), "{preference:?}");
        assert!(options.is_resizable);
        assert!(options.is_movable);
    }
}

#[test]
fn an_unknown_target_display_never_falls_back_to_the_active_screen() {
    assert_eq!(
        bootty_ui::window::macos_screen_facts(Some(u32::MAX)),
        bootty_ui::window::MacosScreenFacts::default()
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_window_adapter_keeps_native_fullscreen_with_app() {
    assert!(!bootty_ui::window::handles_macos_non_native_fullscreen_frame());
    assert_eq!(
        bootty_ui::window::macos_active_screen_notch_height().classify(),
        std::num::FpCategory::Zero
    );
    assert!(!bootty_ui::window::macos_active_screen_is_notched());
    assert_eq!(bootty_ui::window::macos_active_screen_notch_span(), None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_window_adapter_noops_native_mutations() {
    bootty_ui::window::macos_disable_titlebar_separator();
    bootty_ui::window::macos_set_window_shadow("Bootty test", true);
    bootty_ui::window::disable_automatic_window_tabbing();
}
