#![cfg(test)]

use std::sync::Arc;

use bootty_config::{
    config::{commit_config_document, load_config_from_path, load_or_create_config_document},
    settings_schema::SettingsSchema,
};
use bootty_ui::settings_session::{
    AcceptedSettings, Catalogs, SettingsEffect, SettingsOutcome, SettingsSession,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn accepted(path: &std::path::Path, revision: u64) -> AcceptedSettings {
    AcceptedSettings {
        config: Arc::new(load_config_from_path(path).expect("resolved config")),
        revision,
        document: load_or_create_config_document(path).expect("config document"),
        schema: Arc::new(SettingsSchema::new(
            SettingsSchema::builtin().specs().to_vec(),
        )),
    }
}

fn session(path: &std::path::Path) -> SettingsSession {
    SettingsSession::new(accepted(path, 1), Catalogs::default())
}

fn submitted_document(session: &mut SettingsSession) -> bootty_config::config::ConfigDocument {
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("one typed document submission expected");
    };
    document.clone()
}

fn commit(path: &std::path::Path, session: &mut SettingsSession) {
    let document = submitted_document(session);
    let (accepted, ()) =
        commit_config_document(path, document, |_| Ok(())).expect("commit typed configuration");
    session.apply_outcome(SettingsOutcome::DocumentAccepted {
        source: bootty_ui::settings_session::SettingsWriteSource::Document,
        accepted: Box::new(AcceptedSettings {
            revision: 2,
            config: Arc::new(accepted.config),
            document: accepted.document,
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        }),
        warning: accepted
            .write_outcome
            .durability_warning()
            .map(str::to_owned),
    });
}

#[rstest]
fn environment_entries_round_trip_as_config_tables() {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut session = session(&path);
    let environment = vec![
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
    ];

    assert!(session.set_environment(environment.clone()));
    commit(&path, &mut session);

    assert_eq!(
        load_config_from_path(&path)
            .expect("reload config")
            .session
            .env,
        environment
    );
}

#[rstest]
fn accepted_config_is_the_source_of_structured_settings() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        "[session]\nenv = [{ name = 'SAVED', value = 'yes' }]\n",
    )
    .unwrap();
    let session = session(&path);
    assert_eq!(session.environment()[0].name, "SAVED");
}

#[rstest]
fn older_document_acceptance_cannot_replace_a_newer_draft() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    let mut session = session(&path);
    let old = accepted(&path, 1);
    session.reconcile_accepted(accepted(&path, 2));
    assert!(session.set_environment(vec![("DRAFT".to_owned(), "new".to_owned())]));

    session.apply_outcome(SettingsOutcome::DocumentAccepted {
        source: bootty_ui::settings_session::SettingsWriteSource::Document,
        accepted: Box::new(old),
        warning: None,
    });

    assert_eq!(session.environment()[0].name, "DRAFT");
    assert!(session.has_unsaved_changes());
}
