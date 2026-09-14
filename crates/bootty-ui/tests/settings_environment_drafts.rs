#![cfg(test)]

use std::sync::Arc;

use assert_fs::TempDir;
use bootty_config::{
    config::{commit_config_document, load_config_from_path, load_or_create_config_document},
    settings_schema::SettingsSchema,
};
use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsEffect, SettingsSession};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn session(environment: Vec<(String, String)>, path: &std::path::Path) -> SettingsSession {
    SettingsSession::new(
        AcceptedSettings {
            config: std::sync::Arc::new(bootty_config::config::BoottyConfig::default()),
            revision: 1,
            document: load_or_create_config_document(path).expect("empty config document"),
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs {
            environment,
            ..Catalogs::default()
        },
    )
}

#[rstest]
fn incomplete_environment_draft_is_preserved_without_submission_or_error() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut session = session(Vec::new(), &directory.path().join("config.toml"));

    session.add_environment_variable();
    assert!(session.set_environment_value(0, "development".to_owned()));

    assert!(session.take_effects().is_empty());
    assert_eq!(session.write_error(), None);
    assert_eq!(session.environment().len(), 1);
    assert_eq!(session.environment()[0].name, "");
    assert_eq!(session.environment()[0].value, "development");
}

#[rstest]
fn completing_environment_name_submits_the_whole_typed_table() {
    let directory = TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut session = session(Vec::new(), &path);
    session.add_environment_variable();
    session.set_environment_value(0, "development".to_owned());

    assert!(session.set_environment_name(0, "BOOTTY_MODE".to_owned()));

    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("completing the draft submits exactly one document");
    };
    commit_config_document(&path, document.clone(), |_| Ok::<(), String>(()))
        .expect("commit environment document");
    assert_eq!(
        load_config_from_path(&path)
            .expect("reload environment config")
            .session
            .env,
        vec![("BOOTTY_MODE".to_owned(), "development".to_owned())]
    );
}

#[rstest]
fn incomplete_rename_keeps_the_last_persisted_environment_active() {
    let directory = TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let environment = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
    let mut session = session(environment, &path);

    assert!(session.set_environment_name(0, String::new()));

    assert!(session.take_effects().is_empty());
    assert_eq!(session.write_error(), None);
    assert_eq!(session.environment()[0].value, "xterm-256color");
}

#[rstest]
fn duplicate_names_remain_editable_with_a_validation_error() {
    let directory = TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let environment = vec![
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
    ];
    let mut session = session(environment, &path);

    assert!(session.set_environment_name(1, "TERM".to_owned()));

    assert!(session.take_effects().is_empty());
    assert!(
        session
            .write_error()
            .is_some_and(|error| error.contains("more than once"))
    );
    assert_eq!(session.environment()[1].name, "TERM");
    session.set_environment_name(1, "COLORTERM".to_owned());
    assert_eq!(session.write_error(), None);
    assert_eq!(session.take_effects().len(), 1);
}

#[rstest]
#[case("1NAME")]
#[case("BAD-NAME")]
fn invalid_environment_names_explain_why_the_draft_was_not_saved(#[case] name: &str) {
    let directory = TempDir::new().expect("temporary config directory");
    let mut session = session(Vec::new(), &directory.path().join("config.toml"));
    session.add_environment_variable();
    session.set_environment_name(0, name.to_owned());
    assert!(session.take_effects().is_empty());
    assert!(session.write_error().is_some());
    assert_eq!(session.environment()[0].name, name);
}

#[rstest]
fn resetting_environment_discards_unsubmitted_rows_and_removes_the_override() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    let mut session = session(Vec::new(), &path);
    session.add_environment_variable();
    session.set_environment_value(0, "unfinished".to_owned());
    assert!(session.remove_value("session.env"));
    assert_eq!(session.environment(), []);
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("reset submits config");
    };
    commit_config_document(&path, document.clone(), |_| Ok::<(), String>(())).unwrap();
    assert_eq!(
        load_config_from_path(&path).unwrap().session.env,
        Vec::<(std::string::String, std::string::String)>::new()
    );
}
