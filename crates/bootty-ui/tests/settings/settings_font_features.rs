#![cfg(test)]

use std::sync::Arc;

use bootty_config::{config::load_or_create_config_document, settings_schema::SettingsSchema};
use bootty_ui::settings_session::{
    AcceptedSettings, Catalogs, FontFeatureDraft, SettingsEffect, SettingsSession,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn session() -> SettingsSession {
    let path = std::env::temp_dir().join(format!(
        "bootty-settings-font-features-{}-{}.toml",
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
        Catalogs::default(),
    )
}

#[rstest]
fn arbitrary_features_are_deduplicated_and_written_as_one_typed_submission() {
    let mut session = session();

    assert!(session.set_font_features(vec![
        FontFeatureDraft::new("liga", 1).expect("ligature feature"),
        FontFeatureDraft::new("cv01", 1).expect("character variant"),
        FontFeatureDraft::new("cv01", 2).expect("updated character variant"),
    ]));

    assert_eq!(
        session.font_features(),
        Some(vec![
            FontFeatureDraft::new("liga", 1).expect("ligature feature"),
            FontFeatureDraft::new("cv01", 2).expect("character variant"),
        ])
    );
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("font features produce exactly one document submission");
    };
    assert_eq!(
        document.string_array(&["font", "features"]),
        Some(vec!["+liga".to_owned(), "cv01=2".to_owned()])
    );
}

#[rstest]
fn invalid_or_empty_features_do_not_leave_a_lossy_list() {
    let mut session = session();
    assert!(session.set_font_features(vec![
        FontFeatureDraft::new("liga", 0).expect("disabled ligature feature")
    ]));
    session.take_effects();

    assert!(session.set_font_features(Vec::new()));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("clearing features produces one document submission");
    };
    assert_eq!(document.string_array(&["font", "features"]), None);

    assert_eq!(
        FontFeatureDraft::new("cv1", 2),
        Err("OpenType feature tags must contain exactly 4 ASCII characters.".to_owned())
    );
    assert_eq!(
        FontFeatureDraft::new("éééé", 2),
        Err("OpenType feature tags must contain exactly 4 ASCII characters.".to_owned())
    );
}
