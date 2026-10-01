#![cfg(test)]

use std::sync::Arc;

use bootty_config::{
    config::load_or_create_config_document,
    settings_schema::{SettingValue, SettingsSchema},
};
use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsEffect, SettingsSession};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn session() -> SettingsSession {
    let path = std::env::temp_dir().join(format!(
        "bootty-settings-custom-values-{}-{}.toml",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let document = load_or_create_config_document(path).expect("empty config document");
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
fn custom_editor_leaf_is_schema_checked_and_can_return_to_default() {
    let mut session = session();

    assert!(session.set_custom_value(
        "colors.background",
        &SettingValue::Text("#102030".to_owned()),
    ));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("custom value produces exactly one document submission");
    };
    assert_eq!(document.str_at(&["colors", "background"]), Some("#102030"));

    assert!(session.remove_custom_value("colors.background"));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("reset produces exactly one document submission");
    };
    assert_eq!(document.str_at(&["colors", "background"]), None);
}

#[rstest]
fn appearance_color_overrides_keep_light_and_dark_paths_distinct() {
    let mut session = session();

    assert!(session.set_custom_value(
        "appearance.light.colors.background",
        &SettingValue::Text("#102030".to_owned()),
    ));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("light color edit produces exactly one document submission");
    };
    assert_eq!(
        document.str_at(&["appearance", "light", "colors", "background"]),
        Some("#102030")
    );
    assert_eq!(
        document.str_at(&["appearance", "dark", "colors", "background"]),
        None
    );

    assert!(session.set_custom_value(
        "appearance.dark.colors.background",
        &SettingValue::Text("#40506080".to_owned()),
    ));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("dark color edit produces exactly one document submission");
    };
    assert_eq!(
        document.str_at(&["appearance", "light", "colors", "background"]),
        Some("#102030")
    );
    assert_eq!(
        document.str_at(&["appearance", "dark", "colors", "background"]),
        Some("#40506080")
    );
}
