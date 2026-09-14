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
    AcceptedSettings, Catalogs, SettingsEffect, SettingsSession, StatusSegmentEdit,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn session(path: &std::path::Path) -> SettingsSession {
    SettingsSession::new(
        AcceptedSettings {
            config: std::sync::Arc::new(bootty_config::config::BoottyConfig::default()),
            revision: 1,
            document: load_or_create_config_document(path).expect("empty config document"),
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        },
        Catalogs::default(),
    )
}

fn submitted_document(session: &mut SettingsSession) -> bootty_config::config::ConfigDocument {
    let effects = session.take_effects();
    let [SettingsEffect::SubmitDocument(document)] = effects.as_slice() else {
        panic!("one typed document submission expected");
    };
    document.clone()
}

fn commit(path: &std::path::Path, document: bootty_config::config::ConfigDocument) {
    commit_config_document(path, document, |_| Ok(())).expect("commit typed configuration");
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
    commit(&path, submitted_document(&mut session));

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
    commit(&path, submitted_document(&mut session));

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
    commit(&path, submitted_document(&mut session));

    assert!(session.edit_status_segments(
        true,
        StatusSegmentEdit::SetAlignment {
            index: 0,
            alignment: SegmentAlign::Center,
        },
    ));
    commit(&path, submitted_document(&mut session));
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
    commit(&path, submitted_document(&mut session));
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
    commit(&path, submitted_document(&mut session));

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
    commit(&path, submitted_document(&mut session));
    assert_eq!(
        load_config_from_path(&path)
            .expect("reload removed segment")
            .chrome
            .bottom_segments,
        Vec::<bootty_config::config::StatusSegment>::new()
    );
}
