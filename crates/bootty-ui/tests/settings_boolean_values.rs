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
        "bootty-settings-boolean-values-{}-{}.toml",
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
fn cursor_blink_writes_a_boolean_value() {
    let mut session = session();

    assert!(session.set_value("cursor.blink", &SettingValue::Bool(true)));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("boolean setting produces exactly one document submission");
    };
    assert_eq!(document.bool_at(&["cursor", "blink"]), Some(true));
}
