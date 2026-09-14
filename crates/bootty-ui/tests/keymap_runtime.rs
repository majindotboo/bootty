#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::{sync::Arc, time::Duration};

use assert_fs::{TempDir, prelude::*};
use bootty_config::{
    config::{BoottyConfig, MultiplexerBackendConfig},
    keymap_file::{KeymapBindingSource, KeymapContext},
};
use bootty_ui::gpui::{InputEvent, Key, Modifiers, Point, ScrollPhase, WheelUnit};
use bootty_ui::{AppEffect, AppState};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[path = "support/events.rs"]
mod events;
#[path = "support/frames.rs"]
mod frames;
mod support;

fn state(directory: &TempDir, toml_binding: &str, keymap: &str) -> AppState {
    let config_path = directory.path().join("config.toml");
    directory
        .child("keymap.json")
        .write_str(keymap)
        .expect("write keymap");
    let mut config = BoottyConfig {
        config_path,
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    config.input.keybind = vec![toml_binding.to_owned()];
    AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state")
}

fn ctrl_key(key: char) -> bootty_gpui::InputEvent {
    events::key_event(
        Key::Letter(key),
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    )
}

#[rstest]
fn command_bindings_are_catalogued_editable_and_isolate_the_modal_context() {
    use bootty_gpui::CommandAction;
    use bootty_ui::keymap_runtime::KeymapFocus;
    let directory = TempDir::new().unwrap();
    let mut state = state(
        &directory,
        "ctrl+k=command_palette",
        r#"[
        { "context": "Command", "unbind": { "ctrl-n": "ui.command.next" },
          "bindings": { "ctrl-j": "ui.command.next" } }
    ]"#,
    );
    assert!(
        state.keymap_snapshot().diagnostics.is_empty(),
        "{:?}",
        state.keymap_snapshot().diagnostics
    );
    let now = std::time::Instant::now();
    state.update_frame(frames::frame(now, vec![ctrl_key('k')]));
    assert_eq!(state.keymap_focus(), KeymapFocus::Command);
    let effects = state.update_frame(frames::frame(now, vec![ctrl_key('j')]));
    assert!(effects.contains(&AppEffect::CommandAction(CommandAction::Next)));
    let effects = state.update_frame(frames::frame(now, vec![ctrl_key('n')]));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::CommandAction(_)))
    );
    // The same global shortcut must not reopen/toggle the underlying palette while modal.
    state.update_frame(frames::frame(now, vec![ctrl_key('k')]));
    assert_eq!(state.keymap_focus(), KeymapFocus::Command);
}

#[rstest]
fn named_unbind_does_not_consume_an_unrelated_binding() {
    let directory = TempDir::new().unwrap();
    let mut state = state(
        &directory,
        "ctrl+k=open_settings",
        r#"[{ "unbind": { "ctrl-k": "new_window" } }]"#,
    );

    let effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));
    assert!(effects.contains(&AppEffect::OpenSettings));
    assert!(!effects.contains(&AppEffect::OpenWindow));
}

#[rstest]
fn named_unbind_does_not_shadow_an_unrelated_chord_with_the_same_action() {
    use bootty_ui::keymap_runtime::KeymapFocus;

    let directory = TempDir::new().unwrap();
    let mut state = state(
        &directory,
        "ctrl+k>ctrl+b=open_settings",
        r#"[{ "unbind": { "ctrl-k ctrl-a": "open_settings" } }]"#,
    );

    let now = std::time::Instant::now();
    let prefix_effects = state.update_frame(frames::frame(now, vec![ctrl_key('k')]));
    assert!(!prefix_effects.contains(&AppEffect::OpenSettings));
    assert_eq!(state.keymap_focus(), KeymapFocus::Terminal);

    let effects = state.update_frame(frames::frame(now, vec![ctrl_key('b')]));
    assert!(effects.contains(&AppEffect::OpenSettings));
}

#[rstest]
fn user_keymap_is_published_after_the_toml_compatibility_layer() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        "ctrl+k=open_settings",
        r#"[{ "bindings": { "ctrl-k": "new_window" } }]"#,
    );

    let snapshot = state.keymap_snapshot();
    let global = snapshot
        .effective_bindings
        .iter()
        .filter(|binding| binding.context == KeymapContext::Global)
        .collect::<Vec<_>>();
    assert_eq!(global.len(), 2);
    assert_eq!(global[0].source, KeymapBindingSource::BuiltIn);
    assert_eq!(global[1].source, KeymapBindingSource::User);

    let effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));
    assert!(effects.contains(&AppEffect::OpenWindow));
    assert!(!effects.contains(&AppEffect::OpenSettings));
}

#[rstest]
fn disabling_global_defaults_keeps_global_user_bindings_active() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        "ctrl+k=open_settings",
        r#"[
          {
            "use_builtin_defaults": false,
            "bindings": { "ctrl-j": "new_window" }
          }
        ]"#,
    );

    let global = state
        .keymap_snapshot()
        .effective_bindings
        .into_iter()
        .filter(|binding| binding.context == KeymapContext::Global)
        .collect::<Vec<_>>();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].source, KeymapBindingSource::User);

    let disabled_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));
    assert!(!disabled_effects.contains(&AppEffect::OpenSettings));

    let user_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('j')],
    ));
    assert!(user_effects.contains(&AppEffect::OpenWindow));
}

#[rstest]
fn disabling_one_backend_context_keeps_global_defaults() {
    let directory = TempDir::new().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    directory
        .child("keymap.json")
        .write_str(r#"[{ "context": "Native", "use_builtin_defaults": false }]"#)
        .expect("write keymap");
    let mut config = BoottyConfig {
        config_path,
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    config.input.keybind = vec!["ctrl+k=open_settings".to_owned()];
    config.input.backend_keybinds.native = vec!["ctrl+j=new_window".to_owned()];
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");

    let snapshot = state.keymap_snapshot();
    assert!(snapshot.effective_bindings.iter().any(|binding| {
        binding.context == KeymapContext::Global && binding.source == KeymapBindingSource::BuiltIn
    }));
    assert!(!snapshot.effective_bindings.iter().any(|binding| {
        binding.context == KeymapContext::Native && binding.source == KeymapBindingSource::BuiltIn
    }));

    let global_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));
    assert!(global_effects.contains(&AppEffect::OpenSettings));

    let disabled_native_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('j')],
    ));
    assert!(!disabled_native_effects.contains(&AppEffect::OpenWindow));
}

#[cfg(target_os = "macos")]
#[rstest]
fn effective_builtin_snapshots_preserve_macos_option_side_resolution() {
    use bootty_config::config::MacosOptionAsAltConfig;

    let directory = TempDir::new().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    directory
        .child("keymap.json")
        .write_str("[]")
        .expect("write keymap");
    let mut config = BoottyConfig {
        config_path,
        ..BoottyConfig::default()
    };
    config.input.macos_option_as_alt = MacosOptionAsAltConfig::Right;
    config.input.keybind = vec!["alt+n=next_tab".to_owned()];
    config.input.backend_keybinds.native = vec!["alt+j=next_pane".to_owned()];
    let state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");

    let snapshot = state.keymap_snapshot();
    assert!(snapshot.effective_bindings.iter().any(|binding| {
        binding.context == KeymapContext::Global && binding.keystrokes == "right_alt+n"
    }));
    assert!(snapshot.effective_bindings.iter().any(|binding| {
        binding.context == KeymapContext::Native && binding.keystrokes == "right_alt+j"
    }));
    assert!(
        !snapshot
            .effective_bindings
            .iter()
            .any(|binding| binding.keystrokes.starts_with("left_alt"))
    );
}

#[rstest]
fn later_user_sections_win_and_backend_contexts_are_active_in_the_terminal() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        "ctrl+k=open_settings",
        r#"[
          { "bindings": { "ctrl-k": "open_settings" } },
          { "context": "Terminal && backend == native", "bindings": {
              "ctrl-k": "new_window"
          } }
        ]"#,
    );

    let effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));

    assert!(effects.contains(&AppEffect::OpenWindow));
    assert_eq!(
        state
            .keymap_snapshot()
            .keymap
            .sections()
            .next_back()
            .expect("backend section")
            .context,
        KeymapContext::Native
    );
}

#[rstest]
fn context_expressions_gate_bindings_against_the_active_terminal_backend() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        "ctrl+x=open_settings",
        r#"[
          { "context": "Terminal && backend != tmux", "bindings": {
              "ctrl-j": "new_window"
          } },
          { "context": "Terminal && backend == rmux", "bindings": {
              "ctrl-k": "new_window"
          } }
        ]"#,
    );

    let active_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('j')],
    ));
    assert!(active_effects.contains(&AppEffect::OpenWindow));

    let inactive_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));
    assert!(!inactive_effects.contains(&AppEffect::OpenWindow));
}

#[rstest]
fn invalid_context_expressions_are_diagnostic_and_do_not_publish_bindings() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        "ctrl+x=open_settings",
        r#"[
          { "context": "Terminal &&", "bindings": {
              "ctrl-k": "new_window"
          } },
          { "bindings": { "ctrl-j": "new_window" } }
        ]"#,
    );

    assert!(
        state
            .keymap_snapshot()
            .diagnostics
            .iter()
            .any(|diagnostic| {
                diagnostic.field.as_deref() == Some("context")
                    && diagnostic.message.contains("invalid keybinding context")
            })
    );

    let invalid_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));
    assert!(!invalid_effects.contains(&AppEffect::OpenWindow));

    let valid_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('j')],
    ));
    assert!(valid_effects.contains(&AppEffect::OpenWindow));
}

#[rstest]
fn invalid_user_entries_do_not_hide_valid_bindings() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        "ctrl+x=open_settings",
        r#"[{ "bindings": {
          "ctrl-j": "command.that.does.not.exist",
          "ctrl-k": "new_window"
        } }]"#,
    );

    assert_eq!(state.keymap_snapshot().diagnostics.len(), 1);
    let effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));
    assert!(effects.contains(&AppEffect::OpenWindow));
}

#[rstest]
fn file_watch_reloads_a_complete_candidate_before_handling_the_frame() {
    let directory = TempDir::new().expect("temporary config directory");
    let keymap = directory.child("keymap.json");
    let mut state = state(
        &directory,
        "ctrl+x=open_settings",
        r#"[{ "bindings": { "ctrl-k": "open_settings" } }]"#,
    );
    keymap
        .write_str(r#"[{ "bindings": { "ctrl-k": "new_window" } }]"#)
        .expect("replace keymap");
    let now = std::time::Instant::now()
        .checked_add(Duration::from_millis(300))
        .expect("test deadline fits");

    let effects = state.update_frame(frames::frame(now, vec![ctrl_key('k')]));

    assert!(effects.contains(&AppEffect::OpenWindow));
}

#[rstest]
fn syntax_failure_keeps_the_last_published_keymap() {
    let directory = TempDir::new().expect("temporary config directory");
    let keymap = directory.child("keymap.json");
    let mut state = state(
        &directory,
        "ctrl+x=open_settings",
        r#"[{ "bindings": { "ctrl-k": "new_window" } }]"#,
    );
    keymap.write_str("[{ broken").expect("break keymap");

    state
        .reload_keymap()
        .expect("report parse failure without replacing runtime");
    let effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k')],
    ));

    assert!(effects.contains(&AppEffect::OpenWindow));
    assert_eq!(state.keymap_snapshot().diagnostics.len(), 1);
}

#[rstest]
fn capable_keymap_triggers_compile_chords_wheel_sides_and_legacy_flags() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        "ctrl+x=open_settings",
        r#"[{ "bindings": {
          "performable:unconsumed:ctrl-k ctrl-j": "new_window",
          "scroll_up": "open_settings",
          "left_ctrl+l": "new_tab"
        } }]"#,
    );

    assert_eq!(
        state.keymap_snapshot().diagnostics,
        Vec::<bootty_config::keymap_file::KeymapDiagnostic>::new()
    );
    let chord_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![ctrl_key('k'), ctrl_key('j')],
    ));
    assert!(chord_effects.contains(&AppEffect::OpenWindow));

    let wheel_effects = state.update_frame(frames::frame(
        std::time::Instant::now(),
        vec![InputEvent::MouseWheel {
            unit: WheelUnit::Lines,
            delta: Point { x: 0.0, y: 1.0 },
            phase: ScrollPhase::Moved,
            modifiers: Modifiers::default(),
        }],
    ));
    assert!(wheel_effects.contains(&AppEffect::OpenSettings));
}

#[rstest]
fn escape_returns_from_sidebar_without_becoming_a_terminal_shortcut() {
    let directory = TempDir::new().unwrap();
    let mut state = state(&directory, "ctrl+k=toggle_sidebar_focus", "[]");
    let now = std::time::Instant::now();
    state.update_frame(frames::frame(now, vec![ctrl_key('k')]));
    assert!(state.sidebar_focused());
    let escape = || events::key_event(Key::Escape, Modifiers::default());
    let effects = state.update_frame(frames::frame(now, vec![escape()]));
    assert!(state.terminal_focused());
    assert!(effects.contains(&AppEffect::FocusTerminal));
    let effects = state.update_frame(frames::frame(now, vec![escape()]));
    assert!(!effects.contains(&AppEffect::FocusTerminal));
}
