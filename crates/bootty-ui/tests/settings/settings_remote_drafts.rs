#![cfg(test)]

use std::sync::Arc;

use bootty_config::{
    config::{load_config_from_path, load_or_create_config_document},
    settings_schema::SettingsSchema,
};
use bootty_ui::settings_session::{
    AcceptedSettings, Catalogs, RemoteDraft, RemoteOutcome, RemoteProfile, SettingsEffect,
    SettingsOutcome, SettingsSession, SettingsWriteSource,
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

type RemoteSettings = (assert_fs::TempDir, AcceptedSettings, SettingsSession);

#[rstest::fixture]
fn remote_settings() -> anyhow::Result<RemoteSettings> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        "[multiplexer]\nbackend = 'tmux'\n[multiplexer.remote]\nhost = 'accepted.test'\n",
    )?;
    let accepted = AcceptedSettings {
        config: Arc::new(load_config_from_path(&path)?),
        revision: 1,
        document: load_or_create_config_document(&path)?,
        schema: Arc::new(SettingsSchema::new(
            SettingsSchema::builtin().specs().to_vec(),
        )),
    };
    let session = SettingsSession::new(accepted.clone(), Catalogs::default());
    Ok((directory, accepted, session))
}

fn persist_snapshot(mut accepted: AcceptedSettings) -> anyhow::Result<AcceptedSettings> {
    let (written, ()) = bootty_config::config::commit_config_document(
        &accepted.config.config_path,
        accepted.document,
        |_| Ok::<(), String>(()),
    )?;
    accepted.config = Arc::new(written.config);
    accepted.document = written.document;
    Ok(accepted)
}

#[rstest]
fn accepted_config_is_the_source_of_remote_settings(
    remote_settings: anyhow::Result<RemoteSettings>,
) {
    let (_directory, _, session) = remote_settings.unwrap();
    assert_eq!(session.remotes().default.host, "accepted.test");
}

#[rstest]
fn rejected_default_remote_writes_preserve_the_draft(
    remote_settings: anyhow::Result<RemoteSettings>,
    #[values(false, true)] clear: bool,
) {
    let (_directory, accepted, mut session) = remote_settings.unwrap();
    session.edit_default_remote("host", "draft.test".to_owned());
    if clear {
        session.clear_default_remote();
    } else {
        assert!(session.save_default_remote());
    }
    assert_eq!(session.take_effects().len(), 1);
    session.apply_outcome(SettingsOutcome::DocumentRejected {
        source: SettingsWriteSource::DefaultRemote,
        error: "write rejected".to_owned(),
    });
    session.reconcile_accepted(accepted);
    session.set_catalogs(Catalogs::default());
    assert_eq!(session.remotes().default.host, "draft.test");
    assert_eq!(
        session.remotes().default.error.as_deref(),
        Some("write rejected")
    );
}

#[rstest]
fn accepting_a_remote_write_preserves_other_unsaved_settings(
    remote_settings: anyhow::Result<RemoteSettings>,
) {
    let (_directory, mut accepted, mut session) = remote_settings.unwrap();
    session.set_value(
        "font.size",
        &bootty_config::settings_schema::SettingValue::Number(19.0),
    );
    session.take_effects();
    session.apply_outcome(SettingsOutcome::DocumentRejected {
        source: SettingsWriteSource::Document,
        error: "font write rejected".to_owned(),
    });
    session.add_environment_variable();
    session.set_environment_value(0, "unfinished".to_owned());
    let environment = session.environment().to_vec();
    session.edit_default_remote("host", "saved.test".to_owned());
    assert!(session.save_default_remote());
    session.take_effects();
    accepted
        .document
        .set_str(&["multiplexer", "remote", "host"], "saved.test")
        .unwrap();
    accepted.revision = 2;
    accepted = persist_snapshot(accepted).unwrap();
    session.apply_outcome(SettingsOutcome::DocumentAccepted {
        source: SettingsWriteSource::DefaultRemote,
        accepted: Box::new(accepted),
        warning: None,
    });
    assert!(session.has_unsaved_changes());
    assert_eq!(
        session.value("font.size"),
        Some(bootty_config::settings_schema::SettingValue::Number(19.0))
    );
    assert_eq!(session.remotes().default.host, "saved.test");
    assert_eq!(session.environment(), environment);
    assert_eq!(session.write_error(), Some("font write rejected"));
}

#[rstest]
fn accepted_default_remote_writes_clear_only_the_saved_draft(
    remote_settings: anyhow::Result<RemoteSettings>,
    #[values(false, true)] clear: bool,
) {
    let (_directory, mut accepted, mut session) = remote_settings.unwrap();
    session.edit_default_remote("host", " saved.test ".to_owned());
    session.new_remote("unfinished".to_owned());
    let profile_draft = session.remotes().draft;
    if clear {
        session.clear_default_remote();
        accepted.document.remove_multiplexer_remote().unwrap();
    } else {
        assert!(session.save_default_remote());
        accepted
            .document
            .set_str(&["multiplexer", "remote", "host"], "saved.test")
            .unwrap();
    }
    session.take_effects();
    accepted.revision = 2;
    accepted = persist_snapshot(accepted).unwrap();
    session.apply_outcome(SettingsOutcome::DocumentAccepted {
        source: SettingsWriteSource::DefaultRemote,
        accepted: Box::new(accepted.clone()),
        warning: None,
    });
    assert_eq!(
        session.remotes().default.host,
        if clear { "" } else { "saved.test" }
    );
    assert_eq!(session.remotes().draft, profile_draft);
    // A confirmed write leaves this editor clean, so later accepted reloads follow it.
    accepted.revision = 3;
    accepted
        .document
        .set_str(&["multiplexer", "remote", "host"], "external.test")
        .unwrap();
    accepted = persist_snapshot(accepted).unwrap();
    session.reconcile_accepted(accepted);
    assert_eq!(session.remotes().default.host, "external.test");
}

#[rstest]
fn remote_profile_outcomes_belong_to_the_profile_editor(
    remote_settings: anyhow::Result<RemoteSettings>,
    #[values(false, true)] accepted_write: bool,
) {
    let (_directory, mut accepted, mut session) = remote_settings.unwrap();
    session.edit_default_remote("host", "unsaved-default.test".to_owned());
    session.new_remote("server".to_owned());
    session.edit_remote(RemoteDraft {
        id: "server".to_owned(),
        name: " Server ".to_owned(),
        host: " server.test ".to_owned(),
        authentication: "auto".to_owned(),
        host_key_policy: "strict".to_owned(),
        program: "ssh".to_owned(),
        ..RemoteDraft::default()
    });
    assert!(session.save_remote());
    let effects = session.take_effects();
    let [SettingsEffect::UpsertRemote(profile)] = effects.as_slice() else {
        panic!("one profile save");
    };
    assert_eq!(profile.host, "server.test");
    if accepted_write {
        accepted
            .document
            .set_ssh_profile(
                "server",
                &bootty_config::config::SshProfileConfig {
                    name: "Server".to_owned(),
                    host: "server.test".to_owned(),
                    user: None,
                    port: None,
                    authentication: bootty_config::config::SshAuthenticationConfig::Auto,
                    host_key_policy: bootty_config::config::SshHostKeyPolicyConfig::Strict,
                    identity_file: None,
                    proxy_jump: None,
                    program: "ssh".to_owned(),
                    args: Vec::new(),
                },
            )
            .unwrap();
        accepted = persist_snapshot(accepted).unwrap();
        accepted.revision = 2;
        session.apply_outcome(SettingsOutcome::DocumentAccepted {
            source: SettingsWriteSource::RemoteProfile("server".to_owned()),
            accepted: Box::new(accepted),
            warning: None,
        });
        assert_eq!(session.remotes().selected.as_deref(), Some("server"));
        assert_eq!(session.remotes().draft.unwrap().host, "server.test");
    } else {
        session.apply_outcome(SettingsOutcome::DocumentRejected {
            source: SettingsWriteSource::RemoteProfile("server".to_owned()),
            error: "write rejected".to_owned(),
        });
        session.reconcile_accepted(accepted);
        let draft = session.remotes().draft.unwrap();
        assert_eq!(draft.host, " server.test ");
        assert_eq!(draft.error.as_deref(), Some("write rejected"));
    }
    assert_eq!(session.remotes().default.host, "unsaved-default.test");
}

#[rstest]
#[case("native", false)]
#[case("rmux", true)]
#[case("tmux", true)]
#[case("herdr", true)]
fn default_remote_validation_uses_the_effective_backend_from_includes(
    #[case] backend: &str,
    #[case] supported: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "include = ['backend.toml']\n").unwrap();
    std::fs::write(
        directory.path().join("backend.toml"),
        format!("[multiplexer]\nbackend = '{backend}'\n"),
    )
    .unwrap();
    let mut session = SettingsSession::new(
        AcceptedSettings {
            config: Arc::new(load_config_from_path(&path).unwrap()),
            revision: 1,
            document: load_or_create_config_document(&path).unwrap(),
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs::default(),
    );
    session.edit_default_remote("host", "server.test".to_owned());
    assert_eq!(session.save_default_remote(), supported);
    assert_eq!(session.take_effects().is_empty(), !supported);
}

#[rstest::fixture]
fn selected_profile(
    remote_settings: anyhow::Result<RemoteSettings>,
) -> anyhow::Result<RemoteSettings> {
    let (directory, mut accepted, _) = remote_settings?;
    accepted
        .document
        .set_str(&["ssh-profiles", "server", "name"], "Server")?;
    accepted
        .document
        .set_str(&["ssh-profiles", "server", "host"], "saved.test")?;
    accepted = persist_snapshot(accepted)?;
    let mut session = SettingsSession::new(accepted.clone(), Catalogs::default());
    anyhow::ensure!(
        session.select_remote("server"),
        "saved profile is selectable"
    );
    Ok((directory, accepted, session))
}

#[rstest]
fn profile_reload_preserves_edits_and_refreshes_untouched_editors(
    selected_profile: anyhow::Result<RemoteSettings>,
    #[values(false, true)] edited: bool,
    #[values(false, true)] removed: bool,
) {
    let (_directory, mut accepted, mut session) = selected_profile.unwrap();
    if edited {
        let mut draft = session.remotes().draft.unwrap();
        draft.host = "unsaved.test".to_owned();
        session.edit_remote(draft);
    }
    if removed {
        accepted.document.remove_ssh_profile("server").unwrap();
    } else {
        accepted
            .document
            .set_str(&["ssh-profiles", "server", "host"], "external.test")
            .unwrap();
    }
    accepted.revision = 2;
    accepted = persist_snapshot(accepted).unwrap();
    session.reconcile_accepted(accepted);
    let remotes = session.remotes();
    assert_eq!(remotes.selected.as_deref(), (!removed).then_some("server"));
    let expected = if edited {
        Some("unsaved.test")
    } else if removed {
        None
    } else {
        Some("external.test")
    };
    assert_eq!(
        remotes.draft.as_ref().map(|draft| draft.host.as_str()),
        expected
    );
    assert_eq!(remotes.profiles.is_empty(), removed);
    assert!(session.take_effects().is_empty());
}

#[rstest]
fn explicit_profile_removal_clears_its_editor_after_acceptance(
    selected_profile: anyhow::Result<RemoteSettings>,
    #[values(false, true)] accepted_write: bool,
) {
    let (_directory, mut accepted, mut session) = selected_profile.unwrap();
    let mut draft = session.remotes().draft.unwrap();
    draft.host = "unsaved.test".to_owned();
    session.edit_remote(draft.clone());
    session.remove_remote("server".to_owned());
    assert_eq!(session.remotes().draft, Some(draft));
    if accepted_write {
        accepted.document.remove_ssh_profile("server").unwrap();
        accepted.revision = 2;
        accepted = persist_snapshot(accepted).unwrap();
        session.apply_outcome(SettingsOutcome::DocumentAccepted {
            source: SettingsWriteSource::RemoteProfile("server".to_owned()),
            accepted: Box::new(accepted),
            warning: None,
        });
        assert_eq!(session.remotes().draft, None);
        assert_eq!(session.remotes().selected, None);
    } else {
        session.apply_outcome(SettingsOutcome::DocumentRejected {
            source: SettingsWriteSource::RemoteProfile("server".to_owned()),
            error: "removal rejected".to_owned(),
        });
        session.reconcile_accepted(accepted);
        let draft = session.remotes().draft.unwrap();
        assert_eq!(draft.host, "unsaved.test");
        assert_eq!(draft.error.as_deref(), Some("removal rejected"));
    }
}

#[rstest]
fn unchanged_profile_reload_preserves_an_in_flight_connection_test(
    selected_profile: anyhow::Result<RemoteSettings>,
) {
    let (_directory, mut accepted, mut session) = selected_profile.unwrap();
    session.test_remote();
    let pending = session.remotes().testing.expect("pending connection test");
    accepted.revision = 2;
    session.reconcile_accepted(accepted);
    assert_eq!(session.remotes().testing, Some(pending));
    session.apply_outcome(SettingsOutcome::Remote(RemoteOutcome {
        request_id: pending,
        result: Ok(()),
    }));
    assert_eq!(session.remotes().message, Some(Ok(())));
}
