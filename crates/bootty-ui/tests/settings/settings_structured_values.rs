#![cfg(test)]

use std::sync::Arc;

use bootty_config::{
    color::Color,
    config::{
        SegmentAlign, StatusSegment, commit_config_document, load_config_from_path,
        load_or_create_config_document,
    },
    settings_schema::SettingsSchema,
};
use bootty_ui::settings_session::{
    AcceptedSettings, Catalogs, SettingsEffect, SettingsOutcome, SettingsSession, StatusSegmentEdit,
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
fn status_segments_preserve_alignment_colors_and_icons() {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut session = session(&path);
    let segments = vec![StatusSegment {
        module: "clock".to_owned(),
        align: SegmentAlign::Right,
        fg: Some(Color::from_hex("#abcdef").expect("valid color")),
        bg: Some(Color::from_hex("#01020380").expect("valid color")),
        icon: Some("◷".to_owned()),
    }];

    assert!(session.set_status_segments(true, segments.clone()));
    commit(&path, &mut session);

    assert_eq!(
        load_config_from_path(&path)
            .expect("reload config")
            .chrome
            .top_segments,
        segments
    );
}

#[rstest]
fn status_segment_edits_preserve_unedited_fields_and_persist_in_order() {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut session = session(&path);
    let original = vec![
        StatusSegment {
            module: "session".to_owned(),
            align: SegmentAlign::Left,
            fg: Some(Color::from_hex("#112233").expect("valid color")),
            bg: None,
            icon: Some("S".to_owned()),
        },
        StatusSegment {
            module: "clock".to_owned(),
            align: SegmentAlign::Right,
            fg: None,
            bg: Some(Color::from_hex("#44556680").expect("valid color")),
            icon: None,
        },
    ];
    assert!(session.set_status_segments(true, original));
    commit(&path, &mut session);

    assert!(session.edit_status_segments(
        true,
        StatusSegmentEdit::SetAlignment {
            index: 0,
            alignment: SegmentAlign::Center,
        },
    ));
    commit(&path, &mut session);
    let aligned = load_config_from_path(&path)
        .expect("reload aligned segment")
        .chrome
        .top_segments;
    assert_eq!(aligned[0].module, "session");
    assert_eq!(aligned[0].fg, Color::from_hex("#112233").ok());
    assert_eq!(aligned[0].icon.as_deref(), Some("S"));

    assert!(session.edit_status_segments(
        true,
        StatusSegmentEdit::Move {
            index: 1,
            offset: -1,
        },
    ));
    commit(&path, &mut session);
    assert_eq!(
        load_config_from_path(&path)
            .expect("reload reordered segments")
            .chrome
            .top_segments
            .iter()
            .map(|segment| segment.module.as_str())
            .collect::<Vec<_>>(),
        ["clock", "session"]
    );
}

#[rstest]
fn invalid_status_segment_edits_do_not_submit_a_document() {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut session = session(&path);

    assert!(!session.edit_status_segments(
        false,
        StatusSegmentEdit::SetModule {
            index: 0,
            module: "clock".to_owned(),
        },
    ));
    assert!(session.take_effects().is_empty());
    assert_eq!(
        session.write_error(),
        Some("Status segment 0 no longer exists.")
    );
}

#[rstest]
fn status_segment_add_style_and_remove_edits_share_typed_writeback() {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut session = session(&path);

    assert!(session.edit_status_segments(
        false,
        StatusSegmentEdit::Add {
            module: "clock".to_owned(),
        },
    ));
    session.take_effects();
    assert!(session.edit_status_segments(
        false,
        StatusSegmentEdit::SetForeground {
            index: 0,
            color: Color::from_hex("#abcdef").ok(),
        },
    ));
    session.take_effects();
    assert!(session.edit_status_segments(
        false,
        StatusSegmentEdit::SetBackground {
            index: 0,
            color: Color::from_hex("#01020380").ok(),
        },
    ));
    session.take_effects();
    assert!(session.edit_status_segments(
        false,
        StatusSegmentEdit::SetIcon {
            index: 0,
            icon: Some("◷".to_owned()),
        },
    ));
    commit(&path, &mut session);

    assert_eq!(
        load_config_from_path(&path)
            .expect("reload styled segment")
            .chrome
            .bottom_segments,
        [StatusSegment {
            module: "clock".to_owned(),
            align: SegmentAlign::Left,
            fg: Color::from_hex("#abcdef").ok(),
            bg: Color::from_hex("#01020380").ok(),
            icon: Some("◷".to_owned()),
        }]
    );

    assert!(session.edit_status_segments(false, StatusSegmentEdit::Remove { index: 0 }));
    commit(&path, &mut session);
    assert_eq!(
        load_config_from_path(&path)
            .expect("reload removed segment")
            .chrome
            .bottom_segments,
        Vec::<bootty_config::config::StatusSegment>::new()
    );
}

#[rstest]
#[case::top(true)]
#[case::bottom(false)]
fn catalog_refresh_cannot_replace_a_rejected_status_segment_draft(#[case] top: bool) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut session = session(&directory.path().join("config.toml"));
    let segments = vec![StatusSegment {
        module: "clock".to_owned(),
        icon: Some("unsaved".to_owned()),
        ..StatusSegment::default()
    }];
    session.set_status_segments(top, segments.clone());
    session.take_effects();
    session.apply_outcome(SettingsOutcome::DocumentRejected {
        source: bootty_ui::settings_session::SettingsWriteSource::Document,
        error: "read-only config".to_owned(),
    });

    session.set_catalogs(Catalogs::default());

    assert_eq!(session.status_segments(top), segments);
    assert_eq!(session.write_error(), Some("read-only config"));
    assert!(session.edit_status_segments(
        top,
        StatusSegmentEdit::SetAlignment {
            index: 0,
            alignment: SegmentAlign::Right,
        }
    ));
    let edited = session.status_segments(top);
    assert_eq!(edited[0].icon.as_deref(), Some("unsaved"));
    assert_eq!(edited[0].align, SegmentAlign::Right);
}

#[rstest]
fn accepted_config_is_the_source_of_structured_settings() {
    let directory = assert_fs::TempDir::new().unwrap();
    let session = session(&directory.path().join("config.toml"));
    assert_eq!(
        session.status_segments(true),
        bootty_config::config::BoottyConfig::default()
            .chrome
            .top_segments
    );
}

#[rstest]
#[case::top(true)]
#[case::bottom(false)]
fn config_reload_updates_only_unedited_structured_values(#[case] top: bool) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    let mut session = session(&path);
    let draft = vec![StatusSegment {
        module: "clock".to_owned(),
        icon: Some("draft".to_owned()),
        ..StatusSegment::default()
    }];
    session.set_status_segments(top, draft.clone());
    session.take_effects();
    session.apply_outcome(SettingsOutcome::DocumentRejected {
        source: bootty_ui::settings_session::SettingsWriteSource::Document,
        error: "external edit".to_owned(),
    });
    std::fs::write(&path, "[[chrome.top-segment]]\nmodule = 'clock'\nicon = 'external top'\n[[chrome.bottom-segment]]\nmodule = 'sysinfo'\nicon = 'external bottom'\n[session]\nenv = [{ name = 'EXTERNAL', value = 'changed' }]\n").unwrap();
    let accepted = accepted(&path, 2);
    let unedited = if top {
        accepted.config.chrome.bottom_segments.clone()
    } else {
        accepted.config.chrome.top_segments.clone()
    };

    session.reconcile_accepted(accepted.clone());
    session.set_catalogs(Catalogs::default());

    assert_eq!(session.status_segments(top), draft);
    assert_eq!(session.status_segments(!top), unedited);
    assert_eq!(session.environment()[0].name, "EXTERNAL");
    assert!(session.has_unsaved_changes());
    session.discard_document_changes(accepted.clone());
    assert_eq!(
        session.status_segments(true),
        accepted.config.chrome.top_segments
    );
    assert_eq!(
        session.status_segments(false),
        accepted.config.chrome.bottom_segments
    );
    assert!(!session.has_unsaved_changes());
}

#[rstest]
fn a_rejected_status_reset_stays_visible_and_is_the_base_for_the_next_edit() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        "[[chrome.top-segment]]\nmodule = 'clock'\nicon = 'old custom'\n",
    )
    .unwrap();
    let mut session = session(&path);
    assert!(!session.is_default("chrome.top-segment"));
    session.remove_custom_value("chrome.top-segment");
    session.take_effects();
    session.apply_outcome(SettingsOutcome::DocumentRejected {
        source: bootty_ui::settings_session::SettingsWriteSource::Document,
        error: "read-only config".to_owned(),
    });
    session.reconcile_accepted(accepted(&path, 2));
    session.set_catalogs(Catalogs::default());
    let defaults = bootty_config::config::BoottyConfig::default()
        .chrome
        .top_segments;
    assert_eq!(session.status_segments(true), defaults);
    assert!(session.is_default("chrome.top-segment"));
    session.edit_status_segments(
        true,
        StatusSegmentEdit::SetIcon {
            index: 0,
            icon: Some("new".to_owned()),
        },
    );
    assert_eq!(session.status_segments(true).len(), defaults.len());
    assert_eq!(session.status_segments(true)[0].module, defaults[0].module);
    assert_eq!(
        session.status_segments(true)[0].icon.as_deref(),
        Some("new")
    );
}

#[rstest]
fn older_document_acceptance_cannot_replace_a_newer_draft() {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.path().join("config.toml");
    let mut session = session(&path);
    let old = accepted(&path, 1);
    session.reconcile_accepted(accepted(&path, 2));
    let draft = vec![StatusSegment {
        module: "clock".to_owned(),
        icon: Some("new draft".to_owned()),
        ..StatusSegment::default()
    }];
    session.set_status_segments(true, draft.clone());

    session.apply_outcome(SettingsOutcome::DocumentAccepted {
        source: bootty_ui::settings_session::SettingsWriteSource::Document,
        accepted: Box::new(old),
        warning: None,
    });

    assert_eq!(session.status_segments(true), draft);
    assert!(session.has_unsaved_changes());
}
