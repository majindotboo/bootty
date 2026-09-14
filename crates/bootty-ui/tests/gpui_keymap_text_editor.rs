#![cfg(test)]

use bootty_host::text_file::save_text_file;
use std::sync::Arc;

use assert_fs::{TempDir, prelude::*};
use bootty_config::{
    config::{BoottyConfig, MultiplexerBackendConfig},
    keymap_file::{KeymapAction, KeymapContext},
};
use bootty_ui::{AppState, load_keymap_text_file, reload_saved_keymap_text};
use pretty_assertions::assert_eq;
use rstest::rstest;

mod support;

fn state(directory: &TempDir, keymap: &str) -> AppState {
    directory
        .child("keymap.json")
        .write_str(keymap)
        .expect("write keymap");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state")
}

#[rstest]
fn keymap_path_and_contents_are_projected_into_the_reusable_editor_document() {
    let directory = TempDir::new().expect("temporary config directory");
    let state = state(
        &directory,
        r#"[{ "bindings": { "ctrl-k": "new_window" } }]"#,
    );

    let loaded = load_keymap_text_file(&state).expect("load keymap text");

    assert_eq!(loaded.path, directory.path().join("keymap.json"));
    assert_eq!(
        loaded.contents,
        r#"[{ "bindings": { "ctrl-k": "new_window" } }]"#
    );
}

#[rstest]
fn saved_keymap_text_is_reloaded_by_the_app_state_owner() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        r#"[{ "bindings": { "ctrl-k": "new_window" } }]"#,
    );
    let initial_revision = state.keymap_snapshot().revision;
    let keymap_path = state.keymap_snapshot().path;

    save_text_file(
        &keymap_path,
        r#"[{ "context": "Terminal", "bindings": { "ctrl-j": "open_settings" } }]"#,
    )
    .expect("save keymap text");
    reload_saved_keymap_text(&mut state).expect("reload saved keymap text");

    let snapshot = state.keymap_snapshot();
    assert!(snapshot.revision > initial_revision);
    let section = snapshot.keymap.sections().next().expect("user section");
    assert_eq!(section.context, KeymapContext::Terminal);
    assert_eq!(
        section.bindings[0].action,
        KeymapAction::command("open_settings")
    );
    assert_eq!(
        snapshot.diagnostics,
        Vec::<bootty_config::keymap_file::KeymapDiagnostic>::new()
    );
}

#[rstest]
fn invalid_saved_text_keeps_the_last_published_keymap_and_reports_the_diagnostic() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut state = state(
        &directory,
        r#"[{ "bindings": { "ctrl-k": "new_window" } }]"#,
    );
    let accepted = state.keymap_snapshot().keymap;
    let keymap_path = state.keymap_snapshot().path;

    save_text_file(&keymap_path, "[{ broken").expect("save invalid keymap text");
    reload_saved_keymap_text(&mut state).expect("publish keymap diagnostic");

    let snapshot = state.keymap_snapshot();
    assert_eq!(snapshot.keymap, accepted);
    assert_eq!(snapshot.diagnostics.len(), 1);
}
