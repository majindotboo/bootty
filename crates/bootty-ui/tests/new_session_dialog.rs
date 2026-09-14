#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::collections::HashMap;

use bootty_config::config::MultiplexerBackendConfig;
use bootty_git::ProjectPickerEntry;
use bootty_mux::repository::{RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride};
use bootty_mux::{
    controller::SpaceId,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag},
    workspace::BindingSessionGroup,
};
use bootty_ui::gpui::{DialogIntent, RemoteSpaceSnapshot, RowId};
use bootty_ui::presentation::dialogs::{
    NewSessionDialog, NewSessionPickerEvent, SessionPickerDialog, SpaceEditorDialog,
};
use pretty_assertions::assert_eq;

fn project(path: &str, favorite: bool) -> ProjectPickerEntry {
    ProjectPickerEntry {
        path: path.to_owned(),
        favorite,
    }
}

fn project_row<'a>(spec: &'a bootty_gpui::DialogSpec, path: &str) -> &'a bootty_gpui::DialogRow {
    spec.rows
        .iter()
        .find(|row| row.id == RowId::new(format!("project:{path}")))
        .expect("project row")
}

#[test]
fn project_picker_projects_are_grouped_and_favorites_are_visible() {
    let dialog = NewSessionDialog::from_projects(vec![
        project("/projects/plain", false),
        project("/projects/favorite", true),
    ]);

    let spec = dialog.spec();
    assert_eq!(
        spec.rows
            .iter()
            .filter(|row| row.action.is_none())
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Favorites", "Directories"]
    );
    assert_eq!(
        project_row(&spec, "/projects/favorite").icon.as_deref(),
        Some("star")
    );
    assert_eq!(
        project_row(&spec, "/projects/plain").icon.as_deref(),
        Some("folder")
    );
    assert_eq!(
        spec.hint.as_deref(),
        Some("Enter open   Ctrl+Shift+F favorite   Esc close")
    );
}

#[test]
fn project_row_ids_map_activation_across_groups() {
    let favorite = "/projects/favorite";
    let mut dialog = NewSessionDialog::from_projects(vec![
        project("/projects/plain", false),
        project(favorite, true),
    ]);
    let spec = dialog.spec();
    let favorite_row = project_row(&spec, favorite);
    assert_eq!(favorite_row.id, RowId::new(format!("project:{favorite}")));
    let favorite_id = favorite_row.id.clone();
    let action = favorite_row.action.clone().expect("project action");
    let result = dialog.apply(
        &DialogIntent::Activate {
            dialog: spec.id.clone(),
            row: favorite_id,
            action: action.id,
            payload: action.payload,
        },
        &[],
    );
    assert!(matches!(
        result,
        Some(NewSessionPickerEvent::CreateSession { cwd }) if cwd == favorite
    ));
}

#[test]
fn local_picker_starts_with_async_loading_state() {
    let repaint: bootty_mux::RepaintHandle = std::sync::Arc::new(|| {});
    let dialog = NewSessionDialog::open_local(repaint);

    let spec = dialog.spec();

    assert_eq!(spec.empty_text, "loading directories…");
    assert_eq!(spec.rows, Vec::<bootty_ui::gpui::DialogRow>::new());
}

#[test]
fn local_picker_ignores_activation_while_loading() {
    let repaint: bootty_mux::RepaintHandle = std::sync::Arc::new(|| {});
    let mut dialog = NewSessionDialog::open_local(repaint);
    let spec = dialog.spec();

    let result = dialog.apply(
        &DialogIntent::Activate {
            dialog: spec.id,
            row: RowId::new("project:/stale-row"),
            action: bootty_gpui::ActionId::new("open"),
            payload: bootty_gpui::DialogPayload::default(),
        },
        &[],
    );

    assert!(result.is_none());
    assert_eq!(dialog.spec().empty_text, "loading directories…");
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn session_picker_preserves_display_identity_and_captured_targets_after_refresh(
    #[case] remove_target: bool,
) {
    let scope = SpaceId::from_persistence(1);
    let mut groups = vec![BindingSessionGroup {
        scope,
        label: "Local".to_owned(),
        sessions: vec![
            MuxSession {
                id: "session-id".to_owned(),
                name: "backend-name".to_owned(),
                active: false,
                anchor: MuxPaneAnchor::default(),
                active_window_id: None,
                windows: Vec::new(),
                tag: MuxSessionTag::default(),
            },
            MuxSession {
                id: "another-session".to_owned(),
                name: "another-project".to_owned(),
                active: false,
                anchor: MuxPaneAnchor::default(),
                active_window_id: None,
                windows: Vec::new(),
                tag: MuxSessionTag::default(),
            },
        ],
        selected_session: None,
        active: false,
        can_return_to_last_session: false,
        display_names: HashMap::from([("session-id".to_owned(), "Friendly name".to_owned())]),
    }];
    let mut dialog = SessionPickerDialog::open();
    dialog.update_groups(&groups);

    let spec = dialog.spec(&groups);

    let color = spec.rows[0].color.expect("session identity color");
    assert_ne!(Some(color), spec.rows[1].color);
    for query in ["backend-name", "Friendly"] {
        dialog.apply(&DialogIntent::TextChanged {
            dialog: spec.id.clone(),
            value: query.to_owned(),
        });
        let matched = dialog.spec(&groups);
        assert_eq!(matched.rows.len(), 1);
        assert_eq!(matched.rows[0].label, "Friendly name");
        assert_eq!(matched.rows[0].color, Some(color));
    }
    dialog.apply(&DialogIntent::TextChanged {
        dialog: spec.id.clone(),
        value: String::new(),
    });
    let row = &spec.rows[0];
    let action = row.action.clone().expect("session activation");
    if remove_target {
        groups[0].sessions.remove(0);
    } else {
        groups[0].sessions.reverse();
    }
    dialog.update_groups(&groups);
    let result = dialog.apply(&DialogIntent::Activate {
        dialog: spec.id.clone(),
        row: row.id.clone(),
        action: action.id,
        payload: action.payload,
    });
    assert!(matches!(result,
        Some(bootty_ui::presentation::dialogs::SessionPickerEvent::ActivateSession(target))
        if target == bootty_mux::workspace::ScopedSessionTarget::new(scope, "session-id")
    ));
}

#[test]
fn remote_space_editor_starts_catalog_after_profiles_are_loaded() {
    let dialog = SpaceEditorDialog::edit_space(
        SpaceId::from_persistence(1),
        "Development".to_owned(),
        "folder".to_owned(),
        [0x7a, 0xa2, 0xf7],
        false,
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Tmux),
            remote: SpaceRemoteOverride::Profile(RemoteSpaceRef {
                profile_id: "missing".to_owned(),
                remote_space_id: "remote-space".to_owned(),
                remote_space_name: "Development".to_owned(),
                backend: MultiplexerBackendConfig::Tmux,
            }),
        },
    )
    .with_profiles(std::iter::empty());

    assert!(matches!(
        dialog.snapshot().remote,
        RemoteSpaceSnapshot::Failed { .. }
    ));
}

#[rstest::rstest]
fn theme_preview_uses_the_rendered_theme_and_restores_the_original() {
    use bootty_ui::presentation::dialogs::{ThemePickerDialog, ThemePickerEvent};
    let mut dialog = ThemePickerDialog::open(
        vec!["Current Dark".to_owned(), "Other Dark".to_owned()],
        Some("Current Dark".to_owned()),
        "Built in".to_owned(),
    );
    let spec = dialog.spec();
    let preview = |name: &str| {
        let row = spec
            .rows
            .iter()
            .find(|row| row.label == name)
            .expect("theme row");
        let action = row.preview.clone().expect("theme preview");
        DialogIntent::Preview {
            dialog: spec.id.clone(),
            row: row.id.clone(),
            action: action.id,
            payload: action.payload,
        }
    };
    dialog.apply(&DialogIntent::TextChanged {
        dialog: spec.id.clone(),
        value: "Current".to_owned(),
    });
    assert_eq!(
        dialog.apply(&preview("Other Dark")),
        Some(ThemePickerEvent::Preview("Other Dark".to_owned()))
    );
    assert_eq!(dialog.apply(&preview("Other Dark")), None);
    assert_eq!(
        dialog.apply(&preview("Current Dark")),
        Some(ThemePickerEvent::RestorePreview)
    );
}

#[rstest::rstest]
#[case("  renamed  ", Some("renamed"))]
#[case("  ", None)]
fn rename_dialogs_preserve_their_distinct_empty_name_policies(
    #[case] name: &str,
    #[case] session_name: Option<&str>,
) {
    use bootty_ui::presentation::dialogs::{
        RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent,
    };
    let mut session = RenameSessionDialog::open("session".to_owned(), name.to_owned());
    let mut tab = RenameTabDialog::open("session".to_owned(), "tab".to_owned(), name.to_owned());
    let submit = |spec: bootty_gpui::DialogSpec| {
        let row = &spec.rows[0];
        let action = row.action.clone().expect("submit action");
        DialogIntent::Activate {
            dialog: spec.id,
            row: row.id.clone(),
            action: action.id,
            payload: action.payload,
        }
    };
    assert_eq!(session.spec().rows[0].enabled, session_name.is_some());
    assert_eq!(
        session.apply(&submit(session.spec())),
        session_name.map(|name| RenameSessionEvent::Rename {
            session_id: "session".to_owned(),
            name: name.to_owned()
        })
    );
    assert_eq!(
        tab.apply(&submit(tab.spec())),
        Some(RenameTabEvent::Rename {
            session_id: "session".to_owned(),
            window_id: "tab".to_owned(),
            name: name.trim().to_owned()
        })
    );
}
