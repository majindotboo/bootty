#![cfg(test)]

use std::sync::Arc;

use assert_fs::{TempDir, fixture::PathChild, prelude::FileWriteStr};
use bootty_config::{config::load_or_create_config_document, settings_schema::SettingsSchema};
use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsEffect, SettingsSession};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn session(source: &str) -> SettingsSession {
    let directory = TempDir::new().expect("temporary config directory");
    let config_file = directory.child("config.toml");
    config_file.write_str(source).expect("write config source");
    let document =
        load_or_create_config_document(config_file.path()).expect("load config document");
    SettingsSession::new(
        AcceptedSettings {
            config: std::sync::Arc::new(bootty_config::config::BoottyConfig::default()),
            revision: 1,
            document,
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs::default(),
    )
}

#[rstest]
fn indexed_palette_is_written_as_one_typed_array_submission() {
    let mut session = session("");
    let colors = vec!["#102030".to_owned(), "#abcdef".to_owned()];

    assert!(session.set_ansi_palette("appearance.dark.colors.palette", &colors));

    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("palette edit produces exactly one document submission");
    };
    assert_eq!(
        document.string_array(&["appearance", "dark", "colors", "palette"]),
        Some(colors)
    );
}

#[rstest]
fn dark_palette_reads_and_retires_the_legacy_override() {
    let mut session = session("[colors]\npalette = [\"#102030\", \"#abcdef\"]\n");
    assert_eq!(
        session.ansi_palette("appearance.dark.colors.palette"),
        Some(vec!["#102030".to_owned(), "#abcdef".to_owned()])
    );

    assert!(session.set_ansi_palette("appearance.dark.colors.palette", &["#010203".to_owned()]));

    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("palette edit produces exactly one document submission");
    };
    assert!(!document.contains(&["colors", "palette"]));
    assert_eq!(
        document.string_array(&["appearance", "dark", "colors", "palette"]),
        Some(vec!["#010203".to_owned()])
    );
}

#[rstest]
fn resetting_palette_removes_the_branch_and_legacy_overrides() {
    let mut session = session("[colors]\npalette = [\"#102030\"]\n");

    assert!(session.set_ansi_palette("appearance.dark.colors.palette", &[]));

    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("palette reset produces exactly one document submission");
    };
    assert!(!document.contains(&["colors", "palette"]));
    assert!(!document.contains(&["appearance", "dark", "colors", "palette"]));
}
