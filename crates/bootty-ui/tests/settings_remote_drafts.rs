#![cfg(test)]

use std::sync::Arc;

use bootty_config::{
    config::{load_config_from_path, load_or_create_config_document},
    settings_schema::SettingsSchema,
};
use bootty_ui::settings_session::{
    AcceptedSettings, Catalogs, RemoteDraft, RemoteOutcome, RemoteProfile, SettingsEffect,
    SettingsOutcome, SettingsSession,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn session() -> SettingsSession {
    let path = std::env::temp_dir().join(format!(
        "bootty-settings-remote-drafts-{}-{}.toml",
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
fn remote_test_uses_the_current_new_draft_and_intent_fields() {
    let mut session = session();
    session.new_remote("new-remote".to_owned());

    assert!(session.test_remote_with_fields(
        "new-remote",
        vec![
            ("name".to_owned(), "Unsaved remote".to_owned()),
            ("host".to_owned(), "draft.example.test".to_owned()),
            ("user".to_owned(), "luan".to_owned()),
        ],
    ));

    let effects = session.take_effects();
    let [SettingsEffect::TestRemote { profile, .. }] = effects.as_slice() else {
        panic!("testing a valid new draft emits exactly one remote test");
    };
    assert_eq!(
        profile,
        &RemoteProfile {
            id: "new-remote".to_owned(),
            name: "Unsaved remote".to_owned(),
            host: "draft.example.test".to_owned(),
            user: Some("luan".to_owned()),
            authentication: "auto".to_owned(),
            host_key_policy: "strict".to_owned(),
            program: "ssh".to_owned(),
            ..RemoteProfile::default()
        }
    );
}

#[rstest]
fn remote_test_never_replaces_a_new_draft_with_a_saved_profile() {
    let mut session = session();
    session.new_remote("new-remote".to_owned());

    assert!(!session.test_remote_with_fields(
        "different-profile",
        vec![("host".to_owned(), "wrong.example.test".to_owned())],
    ));
    assert!(session.take_effects().is_empty());
    assert_eq!(
        session.remotes().draft.map(|draft| draft.id),
        Some("new-remote".to_owned())
    );
}

#[rstest]
fn remote_test_result_does_not_follow_a_different_draft() {
    let mut session = session();
    session.new_remote("first".to_owned());
    assert!(
        session
            .test_remote_with_fields("first", vec![("host".to_owned(), "first.test".to_owned())])
    );
    let effects = session.take_effects();
    let [SettingsEffect::TestRemote { request_id, .. }] = effects.as_slice() else {
        panic!("one test request")
    };
    session.new_remote("second".to_owned());
    session.apply_outcome(SettingsOutcome::Remote(RemoteOutcome {
        request_id: *request_id,
        result: Ok(()),
    }));
    let snapshot = session.remotes();
    assert_eq!(snapshot.testing, None);
    assert_eq!(snapshot.message, None);
    assert_eq!(snapshot.draft.expect("second draft").id, "second");
}

#[rstest]
#[case("0", None)]
#[case("1", Some(1))]
#[case("65535", Some(65535))]
#[case("65536", None)]
fn remote_port_is_a_nonzero_network_port(#[case] port: &str, #[case] expected: Option<u16>) {
    let draft = RemoteDraft {
        name: "Host".to_owned(),
        host: "host.test".to_owned(),
        authentication: "auto".to_owned(),
        port: port.to_owned(),
        ..RemoteDraft::default()
    };
    assert_eq!(
        draft.validate().ok().and_then(|profile| profile.port),
        expected
    );
}

#[rstest]
fn selecting_native_removes_the_default_remote_in_one_valid_document(
    #[values(false, true)] reset: bool,
) {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        "[multiplexer]\nbackend = \"tmux\"\n\n[multiplexer.remote]\nhost = \"devbox\"\n",
    )
    .expect("write remote config");
    let document = load_or_create_config_document(&path).expect("load remote config document");
    let mut session = SettingsSession::new(
        AcceptedSettings {
            config: std::sync::Arc::new(bootty_config::config::BoottyConfig::default()),
            revision: 1,
            document,
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs::default(),
    );

    assert!(if reset {
        session.remove_value("multiplexer.backend")
    } else {
        session.set_multiplexer_backend("native")
    });

    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("native selection submits exactly one complete config document");
    };
    assert_eq!(
        document.str_at(&["multiplexer", "backend"]),
        if reset { None } else { Some("native") }
    );
    assert!(!document.contains(&["multiplexer", "remote"]));

    bootty_config::config::commit_config_document(
        &path,
        document.clone(),
        |_| Ok::<(), String>(()),
    )
    .expect("the submitted document remains valid");
    assert_eq!(
        load_config_from_path(&path)
            .expect("load committed native config")
            .multiplexer
            .remote,
        None
    );
}

#[rstest]
fn saving_a_default_remote_with_native_selected_is_rejected_without_an_effect() {
    let mut session = session();
    assert!(session.edit_default_remote("host", "devbox".to_owned()));

    assert!(!session.save_default_remote());
    assert!(session.take_effects().is_empty());
    assert_eq!(
        session.remotes().default.error.as_deref(),
        Some("Choose herdr, rmux, or tmux before saving a default remote.")
    );
}
