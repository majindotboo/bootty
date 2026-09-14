#![cfg(test)]

use std::sync::Arc;
use std::time::Instant;

use assert_fs::{TempDir, fixture::ChildPath, prelude::*};
use bootty_config::FontFeature;
use bootty_config::config::load_config_from_path;
use bootty_ui::gpui::{InputEvent, Key, Modifiers};
use bootty_ui::terminal_text::{CodepointFontMap, TerminalTextConfig};
use bootty_ui::{AppEffect, AppState};
use pretty_assertions::assert_eq;

mod support;

#[path = "support/frames.rs"]
mod frame_inputs;

fn state_with_config(source: &str) -> (TempDir, ChildPath, AppState) {
    let directory = TempDir::new().expect("temporary config directory");
    let config = directory.child("config.toml");
    config.write_str(source).expect("write initial config");
    let loaded = load_config_from_path(config.path()).expect("load initial config");
    let state = AppState::new(loaded, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");
    (directory, config, state)
}

#[test]
fn font_reload_publishes_the_complete_realized_text_config() {
    let (_directory, config, mut state) = state_with_config("");

    config
        .write_str(
            r#"
font-feature = ["ss05"]

[font]
family = ["Test Mono", "monospace"]
features = ["-liga", "cv01=2"]
size = 18
cell-width = 10
cell-height = 22
fit-cell-height = false
fit-cell-width = true
baseline-adjustment = 4
underline-position = 3
underline-thickness = 2
style-bold = "Regular"
style-italic = false
style-bold-italic = "Bold Italic"
"#,
        )
        .expect("write changed config");
    let mut effects = Vec::new();

    assert!(state.reload_config(&mut effects));
    let text = effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::SetTerminalTextConfig(config) => Some(config),
            _ => None,
        })
        .expect("font change publishes a terminal text config");
    assert_eq!(
        text,
        &TerminalTextConfig {
            families: vec!["Test Mono".to_owned(), "monospace".to_owned()],
            font_features: vec![
                FontFeature::new(*b"liga", 0),
                FontFeature::new(*b"cv01", 2),
                FontFeature::new(*b"ss05", 1),
            ],
            codepoint_overrides: CodepointFontMap::default(),
            style_bold: bootty_config::FontStyleAssignment::Named("Regular".to_owned()),
            style_italic: bootty_config::FontStyleAssignment::Disabled,
            style_bold_italic: bootty_config::FontStyleAssignment::Named("Bold Italic".to_owned()),
            font_size: 18.0,
            cell_width: Some(10.0),
            cell_height: Some(22.0),
            fit_cell_height: false,
            fit_cell_width: true,
            baseline_adjustment: 4.0,
            underline_position: 3.0,
            underline_thickness: 2.0,
        }
    );
}

#[test]
fn ui_font_reload_publishes_the_accepted_family_stack_and_size() {
    let (_directory, config, mut state) = state_with_config("");

    config
        .write_str(
            r#"
[font]
ui-family = ["Interface Primary", "Interface Fallback"]
ui-size = 18
"#,
        )
        .expect("write changed UI font config");
    let mut effects = Vec::new();

    assert!(state.reload_config(&mut effects));
    let families = effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::SetUiFonts(families) => Some(families),
            _ => None,
        })
        .expect("UI family change publishes its accepted stack");
    assert_eq!(
        families,
        &[
            "Interface Primary".to_owned(),
            "Interface Fallback".to_owned()
        ]
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::SetUiFontSize(size) if size.to_bits() == 18.0_f32.to_bits()))
    );
}

#[test]
fn invalid_font_reload_keeps_the_last_good_font_config() {
    let (_directory, config, mut state) = state_with_config("[font]\nfeatures = [\"-liga\"]\n");
    let last_good_font = state.config().font.clone();

    config
        .write_str("[font]\nfeatures = [\"toolong\"]\n")
        .expect("write invalid config");
    let mut effects = Vec::new();

    assert!(!state.reload_config(&mut effects));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::SetTerminalTextConfig(_)))
    );
    assert_eq!(state.config().font, last_good_font);
    assert_eq!(
        state.last_error().as_deref(),
        Some("invalid font feature: toolong")
    );
}

#[test]
fn reset_font_size_restores_the_loaded_font_size() {
    let (_directory, _config, mut state) = state_with_config("[font]\nsize = 15\n");
    let command = Modifiers {
        platform: true,
        ..Modifiers::default()
    };
    let effects = state.update_frame(frame_inputs::frame(
        Instant::now(),
        vec![InputEvent::Key {
            key: Key::Digit(0),
            pressed: true,
            repeat: false,
            modifiers: command,
        }],
    ));
    assert!(
        !effects
            .iter()
            .any(|effect| { matches!(effect, AppEffect::SetTerminalTextConfig(_)) })
    );
    assert_eq!(state.config().font.size.to_bits(), 15.0_f32.to_bits());
}

#[rstest::rstest]
#[case("\"Regular\"", bootty_config::FontStyleAssignment::Named("Regular".to_owned()))]
#[case("false", bootty_config::FontStyleAssignment::Disabled)]
#[case("\"auto\"", bootty_config::FontStyleAssignment::Automatic)]
fn ui_weight_reload_publishes_the_accepted_assignment(
    #[case] value: &str,
    #[case] expected: bootty_config::FontStyleAssignment,
) {
    let (_directory, config, mut state) =
        state_with_config("[font.ui-weights]\nbold = \"Heavy\"\n");
    config
        .write_str(&format!("[font.ui-weights]\nbold = {value}\n"))
        .unwrap();
    let mut effects = Vec::new();
    assert!(state.reload_config(&mut effects));
    let weights = effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::SetUiFontWeights(weights) => Some(weights),
            _ => None,
        })
        .expect("accepted assignment is published");
    assert_eq!(
        weights.get(&bootty_config::FontWeightRole::Bold),
        Some(&expected)
    );
}
