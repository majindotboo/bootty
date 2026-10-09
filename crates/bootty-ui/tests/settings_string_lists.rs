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

#[rstest]
fn provider_profile_edits_submit_atomically_and_removal_preserves_account_files() {
    use assert_fs::prelude::*;
    use bootty_config::config::{commit_config_document, load_config_from_path};
    use bootty_ui::gpui::ScalarValue;
    let directory = assert_fs::TempDir::new().unwrap();
    let config = directory.child("config.toml");
    config.write_str("").unwrap();
    let account = directory.child("account/credentials.txt");
    account.write_str("dummy credential fixture").unwrap();
    let mut session = SettingsSession::new(
        AcceptedSettings {
            revision: 1,
            config: Arc::new(load_config_from_path(config.path()).unwrap()),
            document: load_or_create_config_document(config.path()).unwrap(),
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs::default(),
    );
    assert!(session.set_custom_value(
        "agents.codex.profiles.work.name",
        &ScalarValue::Text("Work".to_owned())
    ));
    assert!(
        session.set_custom_value(
            "agents.codex.profiles.work.directory",
            &ScalarValue::Text(
                account
                    .path()
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            )
        )
    );
    assert!(session.set_string_list(
        "agents.codex.profiles.work.arguments",
        &["--model".to_owned(), "literal model".to_owned()]
    ));
    assert!(session.set_custom_value(
        "agents.codex.selected",
        &ScalarValue::Text("work".to_owned())
    ));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("one atomic profile submission")
    };
    let (accepted, ()) =
        commit_config_document(config.path(), document.clone(), |_| Ok(())).unwrap();
    assert_eq!(
        accepted
            .config
            .agents
            .codex
            .selected_profile()
            .unwrap()
            .arguments,
        ["--model", "literal model"]
    );
    session.apply_outcome(
        bootty_ui::settings_session::SettingsOutcome::DocumentAccepted {
            source: bootty_ui::settings_session::SettingsWriteSource::Document,
            accepted: Box::new(AcceptedSettings {
                revision: 2,
                config: Arc::new(accepted.config),
                document: accepted.document,
                schema: Arc::new(SettingsSchema::new(
                    SettingsSchema::builtin().specs().to_vec(),
                )),
            }),
            warning: None,
        },
    );
    assert!(session.set_custom_value("agents.codex.selected", &ScalarValue::Text(String::new())));
    assert!(session.remove_custom_value("agents.codex.profiles.work"));
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("one atomic removal submission")
    };
    let (accepted, ()) =
        commit_config_document(config.path(), document.clone(), |_| Ok(())).unwrap();
    assert_eq!(accepted.config.agents.codex.selected_profile(), None);
    assert!(accepted.config.agents.codex.profiles.is_empty());
    assert_eq!(
        std::fs::read_to_string(account.path()).unwrap(),
        "dummy credential fixture"
    );
}
