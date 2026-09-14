#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use pretty_assertions::{assert_eq, assert_ne};

use std::sync::Arc;
#[cfg(unix)]
use std::time::{Duration, Instant};

use assert_fs::{TempDir, fixture::ChildPath, prelude::*};
use bootty_config::{
    color::Color,
    config::{
        BoottyConfig, CursorStyleConfig, MultiplexerBackendConfig, WindowFullscreen,
        commit_config_document, load_config_from_path, load_or_create_config_document,
    },
    settings_schema::{SettingValue, SettingsSchema},
};
use bootty_mux::snapshot::MuxPaneAnchor;
use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsEffect, SettingsSession};
use bootty_ui::{AppEffect, AppState};
use rstest::rstest;

#[path = "support/events.rs"]
mod events;
#[path = "support/frames.rs"]
mod frames;
mod support;

fn app_state(config: BoottyConfig) -> AppState {
    AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state")
}

fn state_from_config(source: &str) -> (TempDir, ChildPath, AppState) {
    let directory = TempDir::new().expect("temporary config directory");
    let config_file = directory.child("config.toml");
    config_file.write_str(source).expect("write initial config");
    let config = load_config_from_path(config_file.path()).expect("load initial config");
    let state = app_state(config);
    (directory, config_file, state)
}

fn settings_session(config_file: &ChildPath) -> SettingsSession {
    SettingsSession::new(
        AcceptedSettings {
            config: Arc::new(
                load_config_from_path(config_file.path()).expect("resolve settings config"),
            ),
            revision: 1,
            document: load_or_create_config_document(config_file.path())
                .expect("load settings document"),
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs::default(),
    )
}

fn persist_settings_submissions(config_file: &ChildPath, session: &mut SettingsSession) {
    for effect in session.take_effects() {
        let SettingsEffect::SubmitDocument(document) = effect else {
            panic!("color edits only submit config documents");
        };
        commit_config_document(config_file.path(), document, |_| Ok::<(), String>(()))
            .expect("commit settings document");
    }
}

#[test]
fn selecting_a_missing_space_is_a_noop() {
    let directory = assert_fs::TempDir::new().expect("temporary app directory");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config
        .input
        .keybind
        .push("ctrl+3=select_space:3".to_owned());
    let mut state = app_state(config);

    state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![events::key_event(
            bootty_gpui::Key::Digit(3),
            bootty_gpui::Modifiers {
                control: true,
                ..bootty_gpui::Modifiers::default()
            },
        )],
    ));

    assert_eq!(state.last_error(), None);
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "The accepted width must retain the supplied value after a failed write."
)]
fn a_failed_app_write_keeps_the_error_visible() {
    let (_directory, config_file, mut state) =
        state_from_config("[chrome]\nsidebar-width = 320\n\n[multiplexer]\nbackend = \"rmux\"\n");
    std::fs::remove_file(config_file.path()).expect("remove config file");
    std::fs::create_dir(config_file.path()).expect("replace config with directory");

    state.set_sidebar_width_live(444.0);
    state.persist_sidebar_width(444.0, &mut Vec::new());

    assert_eq!(state.config().chrome.sidebar_width, 444.0);
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("config file"))
    );
    assert!(config_file.path().is_dir());
}

#[test]
fn every_accepted_config_change_advances_the_revision() {
    let (_directory, _config_file, mut state) =
        state_from_config("[chrome]\nsidebar-width = 320\n");

    let initial = state.config_revision();
    state.set_sidebar_width_live(444.0);
    let after_live = state.config_revision();
    assert_ne!(after_live, initial, "a live edit is a config change");

    state.persist_sidebar_width(444.0, &mut Vec::new());
    assert_ne!(
        state.config_revision(),
        after_live,
        "an accepted document is a config change"
    );
}

#[test]
fn live_terminal_policy_reload_accepts_one_complete_candidate() {
    let (_directory, config_file, mut state) = state_from_config("[appearance]\nmode = \"dark\"\n");

    config_file
        .write_str(
            r##"
[appearance]
mode = "dark"

[appearance.dark.colors]
background = "#010203"

[cursor]
style = "hollow-block"
blink = false

[session]
glyph-protocol = false
"##,
        )
        .expect("write changed terminal policy");

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(
        state.config().appearance.dark.colors.background,
        Some(Color {
            r: 1,
            g: 2,
            b: 3,
            a: u8::MAX,
        })
    );
    assert_eq!(
        state.config().cursor.style,
        Some(CursorStyleConfig::HollowBlock)
    );
    assert_eq!(state.config().cursor.blink, Some(false));
    assert!(!state.config().session.glyph_protocol);
}

#[rstest]
fn settings_colors_persist_and_reset_across_wildcard_branches_and_reload() {
    let (_directory, config_file, mut state) = state_from_config("[appearance]\nmode = \"dark\"\n");
    let default_cursor_text = state.config().appearance.light.colors.cursor_text;
    let default_dark_background = state.config().appearance.dark.colors.background;
    let mut session = settings_session(&config_file);

    assert!(session.set_custom_value(
        "colors.cursor-text",
        &SettingValue::Text("#102030".to_owned()),
    ));
    assert!(session.set_custom_value(
        "appearance.dark.colors.background",
        &SettingValue::Text("#40506080".to_owned()),
    ));
    persist_settings_submissions(&config_file, &mut session);

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(
        state.config().appearance.light.colors.cursor_text,
        Some(Color::from_hex("#102030").expect("valid RGB color"))
    );
    assert_eq!(
        state.config().appearance.dark.colors.background,
        Some(Color::from_hex("#40506080").expect("valid RGBA color"))
    );

    assert!(session.remove_custom_value("colors.cursor-text"));
    assert!(session.remove_custom_value("appearance.dark.colors.background"));
    persist_settings_submissions(&config_file, &mut session);

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(
        state.config().appearance.light.colors.cursor_text,
        default_cursor_text
    );
    assert_eq!(
        state.config().appearance.dark.colors.background,
        default_dark_background
    );
    let document =
        load_or_create_config_document(config_file.path()).expect("reload reset settings document");
    assert!(!document.contains(&["colors", "cursor-text"]));
    assert!(!document.contains(&["appearance", "dark", "colors", "background"]));
}

#[rstest]
fn incomplete_environment_rename_waits_for_a_complete_typed_writeback() {
    let (_directory, config_file, mut state) =
        state_from_config("[session]\nenv = [{ name = \"TERM\", value = \"xterm-256color\" }]\n");
    let mut session = SettingsSession::new(
        AcceptedSettings {
            config: Arc::new(
                load_config_from_path(config_file.path()).expect("resolve settings config"),
            ),
            revision: 1,
            document: load_or_create_config_document(config_file.path())
                .expect("load settings document"),
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs {
            environment: state.config().session.env.clone(),
            ..Catalogs::default()
        },
    );

    assert!(session.set_environment_name(0, String::new()));
    assert!(session.take_effects().is_empty());
    assert_eq!(session.write_error(), None);

    assert!(session.set_environment_name(0, "BOOTTY_TERM".to_owned()));
    persist_settings_submissions(&config_file, &mut session);

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(
        state.config().session.env,
        vec![("BOOTTY_TERM".to_owned(), "xterm-256color".to_owned())]
    );
}

#[rstest]
fn settings_colors_persist_and_reset_across_chrome_and_sidebar_reload() {
    let (_directory, config_file, mut state) = state_from_config("");
    let mut session = settings_session(&config_file);

    assert!(session.set_custom_value(
        "chrome.pane-divider-color",
        &SettingValue::Text("#abcdef".to_owned()),
    ));
    assert!(session.set_custom_value(
        "sidebar.background",
        &SettingValue::Text("#01020380".to_owned()),
    ));
    persist_settings_submissions(&config_file, &mut session);

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(
        state.config().chrome.pane_divider_color,
        Some(Color::from_hex("#abcdef").expect("valid RGB color"))
    );
    assert_eq!(
        state.config().sidebar.background,
        Some(Color::from_hex("#01020380").expect("valid RGBA color"))
    );

    assert!(session.remove_value("chrome.pane-divider-color"));
    assert!(session.remove_value("sidebar.background"));
    persist_settings_submissions(&config_file, &mut session);

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(state.config().chrome.pane_divider_color, None);
    assert_eq!(state.config().sidebar.background, None);
    let document =
        load_or_create_config_document(config_file.path()).expect("reload reset settings document");
    assert!(!document.contains(&["chrome", "pane-divider-color"]));
    assert!(!document.contains(&["sidebar", "background"]));
}

#[rstest]
#[case("[chrome]\nsidebar = false\n")]
#[case("[session]\nscrollbar = \"always\"\n")]
#[case("[session]\nglyph-protocol = false\n")]
fn live_reload_does_not_require_a_restart(#[case] source: &str) {
    let (_directory, config_file, mut state) = state_from_config("");

    config_file
        .write_str(source)
        .expect("write live chrome change");
    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(state.last_error(), None);
}

#[test]
fn fullscreen_launch_preference_does_not_resize_the_running_window() {
    let (_directory, config_file, mut state) = state_from_config("");
    config_file
        .write_str("[window]\nfullscreen = \"non-native-padded-notch\"\n")
        .expect("write borderless fullscreen config");
    let mut effects = Vec::new();

    assert!(state.reload_config(&mut effects));

    assert_eq!(state.last_error(), None);
    assert!(!state.macos_non_native_fullscreen_active());
    assert!(!effects.contains(&AppEffect::ApplyMacosNonNativeFullscreen));
    assert!(effects.contains(&AppEffect::SetFullscreen(false)));
    assert!(effects.contains(&AppEffect::RequestRepaint));
}

#[test]
fn fullscreen_style_changes_apply_live_but_launch_preference_changes_do_not() {
    let (_directory, config_file, mut state) = state_from_config("");
    let mut fullscreen_frame = frames::frame(std::time::Instant::now(), Vec::new());
    fullscreen_frame.viewport.fullscreen = true;
    state.update_frame(fullscreen_frame);
    config_file
        .write_str("[window]\nfullscreen = \"non-native-padded-notch\"\n")
        .expect("enable borderless fullscreen");
    let mut enter_effects = Vec::new();
    assert!(state.reload_config(&mut enter_effects));
    assert!(enter_effects.contains(&AppEffect::ApplyMacosNonNativeFullscreen));

    config_file
        .write_str(
            "[window]\nfullscreen = \"non-native-padded-notch\"\nfullscreen-enabled = false\n",
        )
        .expect("disable borderless fullscreen");
    let mut leave_effects = Vec::new();
    assert!(state.reload_config(&mut leave_effects));
    assert!(!leave_effects.contains(&AppEffect::RestoreMacosPresentation));
    assert!(state.macos_non_native_fullscreen_active());
}

#[test]
fn fullscreen_chrome_uses_the_frame_target_display() {
    let (_directory, _config_file, mut state) =
        state_from_config("[window]\nfullscreen = \"non-native-padded-notch\"\n");
    let mut frame = frames::frame(std::time::Instant::now(), Vec::new());
    frame.display_id = Some(u32::MAX);
    frame.input.window_focused = false;

    state.update_frame(frame);

    assert_eq!(
        state.window_chrome_facts(),
        bootty_ui::WindowChromeFacts {
            fullscreen: true,
            notched: false,
            notch_band: 0.0,
            notch_span: None,
        }
    );
}

#[rstest::rstest]
#[case(true, 23.0, None, 15.0)]
#[case(false, 23.0, None, 38.0)]
#[case(true, 23.0, Some(12.0), 12.0)]
fn fullscreen_notch_inset_respects_tab_placement(
    #[case] tabs_in_notch: bool,
    #[case] chrome_height: f32,
    #[case] configured_inset: Option<f32>,
    #[case] expected: f32,
) {
    let facts = bootty_ui::WindowChromeFacts {
        fullscreen: true,
        notched: true,
        notch_band: 38.0,
        notch_span: Some((730.0, 910.0)),
    };

    assert_eq!(
        facts
            .top_inset(tabs_in_notch, chrome_height, configured_inset)
            .to_bits(),
        expected.to_bits()
    );
}

#[test]
fn fullscreen_toggle_is_transient_and_keeps_the_configured_borderless_style() {
    let source = "[window]\nfullscreen = \"non-native-padded-notch\"\nfullscreen-enabled = false\n\n[input]\nkeybind = [\"clear\", \"ctrl+f=toggle_fullscreen\"]\n";
    let (_directory, config_file, mut state) = state_from_config(source);
    let mut leave = frames::frame(
        std::time::Instant::now(),
        vec![events::key_event(
            bootty_gpui::Key::Letter('f'),
            bootty_gpui::Modifiers {
                control: true,
                ..bootty_gpui::Modifiers::default()
            },
        )],
    );
    leave.viewport.maximized = true;

    state.update_frame(leave);

    assert!(!state.config().window.fullscreen_enabled);
    assert_eq!(
        state.config().window.fullscreen,
        WindowFullscreen::NonNativePaddedNotch
    );
    let restarted = load_config_from_path(config_file.path()).expect("reload launch preference");
    assert!(!restarted.window.fullscreen_enabled);
    assert_eq!(
        restarted.window.fullscreen,
        WindowFullscreen::NonNativePaddedNotch
    );

    let mut restarted_state = app_state(restarted);
    let enter = frames::frame(
        std::time::Instant::now(),
        vec![events::key_event(
            bootty_gpui::Key::Letter('f'),
            bootty_gpui::Modifiers {
                control: true,
                ..bootty_gpui::Modifiers::default()
            },
        )],
    );
    restarted_state.update_frame(enter);

    assert!(!restarted_state.config().window.fullscreen_enabled);
    assert_eq!(
        restarted_state.config().window.fullscreen,
        WindowFullscreen::NonNativePaddedNotch
    );
    let restarted_again =
        load_config_from_path(config_file.path()).expect("reload unchanged state");
    assert!(!restarted_again.window.fullscreen_enabled);
    assert_eq!(
        restarted_again.window.fullscreen,
        WindowFullscreen::NonNativePaddedNotch
    );
}

#[test]
fn default_fullscreen_toggle_uses_native_without_changing_launch_preferences() {
    let source = "[input]\nkeybind = [\"clear\", \"ctrl+f=toggle_fullscreen\"]\n";
    let (_directory, config_file, mut state) = state_from_config(source);

    state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![events::key_event(
            bootty_gpui::Key::Letter('f'),
            bootty_gpui::Modifiers {
                control: true,
                ..bootty_gpui::Modifiers::default()
            },
        )],
    ));

    assert!(!state.config().window.fullscreen_enabled);
    assert_eq!(state.config().window.fullscreen, WindowFullscreen::Native);
    let restarted = load_config_from_path(config_file.path()).expect("reload unchanged state");
    assert!(!restarted.window.fullscreen_enabled);
    assert_eq!(restarted.window.fullscreen, WindowFullscreen::Native);
}

#[test]
fn every_new_window_policy_reports_the_restart_requirement() {
    for (name, source) in [
        ("session", "[session]\nshell = \"/bin/sh\"\n"),
        ("size", "[window]\nwidth = 900\n"),
        ("decoration", "[window]\nwindow-decoration = \"none\"\n"),
        ("titlebar", "[window]\nmacos-titlebar-style = \"hidden\"\n"),
        ("restore", "restore_on_startup = \"none\"\n"),
        ("last-window", "on_last_window_closed = \"quit_app\"\n"),
    ] {
        let (_directory, config_file, mut state) = state_from_config("");

        config_file
            .write_str(source)
            .expect("write new-window change");
        assert!(state.reload_config(&mut Vec::new()), "{name}");
        assert_eq!(
            state.last_error().as_deref(),
            Some("config reloaded; session/window settings require a new window or restart"),
            "{name}",
        );
    }
}

#[cfg(unix)]
#[test]
fn a_dead_terminal_warns_after_acceptance_and_new_panes_use_the_accepted_config() {
    let (_directory, config_file, mut state) = state_from_config(
        "[multiplexer]\nbackend = \"native\"\n\n[session]\nshell = \"/bootty/missing-shell\"\n",
    );
    let failed = pane("failed", "%1");
    state
        .terminal_mut()
        .sync_native_window(
            std::slice::from_ref(&failed),
            Some(&failed),
            Some("window"),
            MultiplexerBackendConfig::Native,
            false,
        )
        .expect("start failing pane");
    let failure = wait_for_startup_result(&mut state, "%1").expect_err("startup must fail");
    assert_eq!(failure, "spawn shell in PTY");

    config_file
        .write_str(
            r##"
[multiplexer]
backend = "native"

[appearance]
mode = "dark"

[appearance.dark.colors]
background = "#010203"

[cursor]
style = "hollow-block"
blink = false

[session]
shell = "/bin/sh"
glyph-protocol = false
"##,
        )
        .expect("write accepted config");

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(state.config().session.shell.as_deref(), Some("/bin/sh"));
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("terminal config publication failed for SpaceId"))
    );

    state.terminal_mut().discard_active_pane();
    let replacement = pane("replacement", "%2");
    state
        .terminal_mut()
        .sync_native_window(
            std::slice::from_ref(&replacement),
            Some(&replacement),
            Some("window"),
            MultiplexerBackendConfig::Native,
            false,
        )
        .expect("start replacement pane");
    wait_for_startup_result(&mut state, "%2").expect("accepted shell starts replacement pane");
}

#[cfg(unix)]
fn pane(session_id: &str, pane_id: &str) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_id.to_owned(),
        pane_id: Some(pane_id.to_owned()),
        cwd: None,
        pane_pid: None,
        process: None,
    }
}

#[cfg(unix)]
fn wait_for_startup_result(state: &mut AppState, pane_id: &str) -> Result<(), String> {
    // The budget bounds a genuine hang. It stays far above the scheduler jitter that a fully
    // parallel test run adds to a pane spawn.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .ok_or("startup deadline overflow")?;
    loop {
        let runtime = state
            .terminal_mut()
            .focused_terminal_runtime(pane_id)
            .ok_or("focused terminal runtime is missing")?;
        runtime
            .current_working_directory()
            .map_err(|error| error.to_string())?;
        if runtime.tty_name().is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("terminal startup timed out".to_owned());
        }
        std::thread::yield_now();
    }
}
