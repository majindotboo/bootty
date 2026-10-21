use bootty_ui::window_state::WindowState;
use gpui_kit::{Bounds, WindowBounds, point, px, size};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn placement(fullscreen: bool) -> WindowState {
    WindowState {
        bounds: Bounds::new(point(px(80.0), px(90.0)), size(px(800.0), px(600.0))),
        fullscreen,
        maximized: false,
        display: None,
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn closing_windows_retains_each_geometry_and_fullscreen_state(#[case] fullscreen: bool) {
    let directory = assert_fs::TempDir::new().unwrap();
    let config = directory.path().join("config.toml");
    assert_eq!(WindowState::load(&config, "main").unwrap(), None);
    let main = placement(fullscreen);
    let mut second = placement(!fullscreen);
    second.bounds.origin.x = px(200.0);
    main.save(&config, "main").unwrap();
    second.save(&config, "main:window:1").unwrap();
    assert_eq!(WindowState::load(&config, "main").unwrap(), Some(main));
    assert_eq!(
        WindowState::load(&config, "main:window:1").unwrap(),
        Some(second)
    );
    let other_identity = directory.path().join("development/config.toml");
    assert_eq!(WindowState::load(&other_identity, "main").unwrap(), None);
}

#[gpui_kit::test]
fn saved_windowed_state_overrides_the_fullscreen_launch_preference(cx: &gpui_kit::TestAppContext) {
    let mut config = bootty_config::config::BoottyConfig::default();
    config.window.fullscreen = bootty_config::config::WindowFullscreen::Native;
    config.window.fullscreen_enabled = true;
    cx.update(|cx| {
        let mut options = bootty_ui::platform::native_options_for_config(&config, cx);
        let saved = placement(false);
        saved.restore(&config, &mut options, cx);
        assert_eq!(
            options.window_bounds,
            Some(WindowBounds::Windowed(saved.bounds))
        );
        let saved = placement(true);
        saved.restore(&config, &mut options, cx);
        assert_eq!(
            options.window_bounds,
            Some(WindowBounds::Fullscreen(saved.bounds))
        );
    });
}

#[rstest]
fn a_disconnected_display_keeps_the_entire_window_reachable() {
    let mut saved = placement(true);
    saved.bounds = Bounds::new(point(px(-2000.0), px(1400.0)), size(px(2000.0), px(1200.0)));
    let display = Bounds::new(point(px(0.0), px(25.0)), size(px(1440.0), px(875.0)));
    assert_eq!(saved.visible_bounds(display), display);
}

#[rstest]
fn a_failed_save_keeps_the_previous_window_state() {
    let directory = assert_fs::TempDir::new().unwrap();
    let config = directory.path().join("config.toml");
    let previous = placement(false);
    previous.save(&config, "main").unwrap();
    let mut invalid = placement(true);
    invalid.bounds.size.width = px(f32::NAN);
    assert!(invalid.save(&config, "main").is_err());
    assert_eq!(WindowState::load(&config, "main").unwrap(), Some(previous));
}
