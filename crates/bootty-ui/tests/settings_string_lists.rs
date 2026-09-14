#![cfg(test)]

use std::sync::Arc;

use bootty_config::{config::load_or_create_config_document, settings_schema::SettingsSchema};
use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsEffect, SettingsSession};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn session() -> SettingsSession {
    let path = std::env::temp_dir().join(format!(
        "bootty-settings-string-lists-{}-{}.toml",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let document = load_or_create_config_document(path).expect("empty config document");
    let schema = Arc::new(SettingsSchema::new(
        SettingsSchema::builtin().specs().to_vec(),
    ));
    SettingsSession::new(
        AcceptedSettings {
            config: std::sync::Arc::new(bootty_config::config::BoottyConfig::default()),
            revision: 1,
            document,
            schema,
        },
        Catalogs {
            font_families: vec!["Berkeley Mono".to_owned(), "Symbols Nerd Font".to_owned()].into(),
            ..Catalogs::default()
        },
    )
}

#[rstest]
fn ordered_font_stack_is_written_as_one_typed_submission() {
    let mut session = session();
    let stack = vec!["Berkeley Mono".to_owned(), "Symbols Nerd Font".to_owned()];

    assert!(session.set_string_list("font.family", &stack));

    assert_eq!(session.string_list("font.family").as_ref(), Some(&stack));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("font stack produces exactly one document submission");
    };
    assert_eq!(document.string_array(&["font", "family"]), Some(stack));
}

#[rstest]
fn empty_font_stack_removes_the_override_and_catalog_survives_snapshot() {
    let mut session = session();
    session.set_string_list("font.family", &["Berkeley Mono".to_owned()]);
    session.take_effects();

    assert!(session.set_string_list("font.family", &[]));

    assert_eq!(session.string_list("font.family"), None);
    assert_eq!(
        session.font_families().as_ref(),
        ["Berkeley Mono", "Symbols Nerd Font"]
    );
}
