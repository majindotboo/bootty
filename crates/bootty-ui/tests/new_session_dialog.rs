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
        icon: None,
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
        vec!["Favorites", "Projects"]
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
        Some("Choose a project   Ctrl+Shift+F favorite   Esc close")
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
    assert_eq!(result, None);
    assert_eq!(dialog.spec().title, "Choose checkout");
    let checkout = dialog.spec().rows[0].clone();
    assert_eq!(activate_picker_row(&mut dialog, &checkout, &[]), None);
    select_launcher(&mut dialog, "terminal");
    let launch = dialog.spec();
    assert_eq!(launch.title, "New session");
    let terminal = launch.rows.iter().find(|row| row.id.0 == "submit").unwrap();
    assert_eq!(
        activate_picker_row(&mut dialog, terminal, &[]),
        Some(NewSessionPickerEvent::CreateSession {
            cwd: favorite.to_owned(),
            command: None,
        })
    );
}

#[rstest::rstest]
#[case("/projects/current")]
#[case("/remote/current project")]
fn current_checkout_is_reviewable_and_project_switching_can_return(#[case] cwd: &str) {
    let mut dialog = NewSessionDialog::from_projects(vec![project("/projects/other", true)]);
    dialog.set_checkout(cwd.to_owned());
    let spec = dialog.spec();
    assert_eq!(spec.title, "New session");
    assert!(
        spec.footer
            .as_deref()
            .is_some_and(|path| path.ends_with(cwd))
    );
    let switch = spec
        .rows
        .iter()
        .find(|row| row.id.0 == "choose-project")
        .unwrap();
    assert_eq!(activate_picker_row(&mut dialog, switch, &[]), None);
    let spec = dialog.spec();
    assert!(project_row(&spec, "/projects/other").action.is_some());
    let current = spec
        .rows
        .iter()
        .find(|row| row.id.0 == "current-checkout")
        .unwrap();
    assert_eq!(activate_picker_row(&mut dialog, current, &[]), None);
    select_launcher(&mut dialog, "terminal");
    let spec = dialog.spec();
    let terminal = spec.rows.iter().find(|row| row.id.0 == "submit").unwrap();
    assert_eq!(
        activate_picker_row(&mut dialog, terminal, &[]),
        Some(NewSessionPickerEvent::CreateSession {
            cwd: cwd.to_owned(),
            command: None,
        })
    );
}

#[rstest::rstest]
#[case::shell("", "terminal")]
#[case::command("printf 'literal task'", "terminal")]
#[case::agent("Review the current changes", "codex")]
fn launch_draft_survives_project_switching_and_names_its_execution(
    #[case] draft: &str,
    #[case] launcher: &str,
) {
    let mut dialog = NewSessionDialog::from_projects(Vec::new());
    dialog.set_checkout("/projects/current".to_owned());
    dialog.apply(
        &DialogIntent::TextChanged {
            dialog: dialog.spec().id,
            value: draft.to_owned(),
        },
        &[],
    );
    let spec = dialog.spec();
    let switch = spec
        .rows
        .iter()
        .find(|row| row.id.0 == "choose-project")
        .unwrap();
    activate_picker_row(&mut dialog, switch, &[]);
    let spec = dialog.spec();
    let back = spec
        .rows
        .iter()
        .find(|row| row.id.0 == "current-checkout")
        .unwrap();
    activate_picker_row(&mut dialog, back, &[]);
    select_launcher(&mut dialog, launcher);
    let spec = dialog.spec();
    assert_eq!(spec.text.as_deref(), Some(draft));
    let row = spec.rows.iter().find(|row| row.id.0 == "submit").unwrap();
    let expected = if launcher == "terminal" {
        assert_eq!(
            row.label,
            if draft.is_empty() {
                "Open terminal"
            } else {
                "Start command"
            }
        );
        NewSessionPickerEvent::CreateSession {
            cwd: "/projects/current".to_owned(),
            command: (!draft.is_empty()).then(|| draft.to_owned()),
        }
    } else {
        NewSessionPickerEvent::CreateAgentSession {
            cwd: "/projects/current".to_owned(),
            provider: bootty_agents::AgentKind::Codex,
            prompt: draft.to_owned(),
        }
    };
    assert_eq!(activate_picker_row(&mut dialog, row, &[]), Some(expected));
}

#[rstest::rstest]
#[case("terminal")]
#[case("codex")]
fn selected_checkout_can_start_while_the_other_project_catalog_loads(#[case] launcher: &str) {
    let mut dialog = NewSessionDialog::open_local(std::sync::Arc::new(|| {}));
    dialog.set_checkout("/projects/current".to_owned());
    select_launcher(&mut dialog, launcher);
    let spec = dialog.spec();
    let row = spec.rows.iter().find(|row| row.id.0 == "submit").unwrap();
    assert!(activate_picker_row(&mut dialog, row, &[]).is_some());
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

#[rstest::fixture]
fn worktree_project() -> assert_fs::TempDir {
    let repo = assert_fs::TempDir::new().expect("project directory");
    for args in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=Bootty Test",
            "-c",
            "user.email=bootty@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    repo
}

fn select_launcher(dialog: &mut NewSessionDialog, launcher: &str) {
    let value = match launcher {
        "terminal" => "Terminal",
        "codex" => "Codex",
        "claude" => "Claude",
        "pi" => "Pi",
        _ => panic!("unknown launcher"),
    };
    assert_eq!(
        dialog.apply(
            &DialogIntent::FieldChanged {
                dialog: dialog.spec().id,
                field: if launcher == "terminal" {
                    "mode"
                } else {
                    "provider"
                }
                .to_owned(),
                value: value.to_owned(),
            },
            &[]
        ),
        None,
        "selecting a launcher does not start a session"
    );
}

fn activate_picker_row(
    dialog: &mut NewSessionDialog,
    row: &bootty_gpui::DialogRow,
    open_cwds: &[String],
) -> Option<NewSessionPickerEvent> {
    let action = row.action.clone().expect("enabled picker row");
    dialog.apply(
        &DialogIntent::Activate {
            dialog: dialog.spec().id,
            row: row.id.clone(),
            action: action.id,
            payload: action.payload,
        },
        open_cwds,
    )
}

#[rstest::rstest]
fn worktree_form_keeps_its_project_and_normalizes_captured_fields(
    worktree_project: assert_fs::TempDir,
) {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    let path = worktree_project.path().to_string_lossy().into_owned();
    let mut dialog = NewSessionDialog::from_projects(vec![project(&path, false)]);
    let spec = dialog.spec();
    let row = project_row(&spec, &path);
    assert_eq!(
        activate_picker_row(&mut dialog, row, std::slice::from_ref(&path)),
        None
    );
    let spec = dialog.spec();
    let row = spec
        .rows
        .iter()
        .find(|row| row.label == "New worktree")
        .expect("new worktree action");
    assert_eq!(activate_picker_row(&mut dialog, row, &[]), None);
    let id = dialog.spec().id;
    assert_eq!(dialog.spec().text.as_deref(), Some("task/new-task"));
    assert!(dialog.spec().rows[0].enabled);
    dialog.apply(
        &DialogIntent::TextChanged {
            dialog: id.clone(),
            value: "-invalid".to_owned(),
        },
        &[],
    );
    let row = dialog.spec().rows[0].clone();
    assert_eq!(activate_picker_row(&mut dialog, &row, &[]), None);
    assert!(
        dialog.spec().rows[0].detail.is_some(),
        "invalid branch is explained"
    );

    let strategy = (
        "[a-z][a-z0-9]{0,12}",
        proptest::option::of("[a-z][a-z0-9]{0,12}"),
        any::<bool>(),
    );
    let dialog = std::cell::RefCell::new(dialog);
    TestRunner::new(Config {
        cases: 24,
        ..Config::default()
    })
    .run(&strategy, |(branch, folder, use_head)| {
        let mut dialog = dialog.borrow_mut();
        dialog.apply(
            &DialogIntent::TextChanged {
                dialog: id.clone(),
                value: format!("  {branch}  "),
            },
            &[],
        );
        for (field, value) in [
            (
                "folder",
                folder
                    .as_ref()
                    .map_or_else(|| "  ".to_owned(), |name| format!(" {name} ")),
            ),
            (
                "start-ref",
                if use_head { " HEAD " } else { "  " }.to_owned(),
            ),
        ] {
            dialog.apply(
                &DialogIntent::FieldChanged {
                    dialog: id.clone(),
                    field: field.to_owned(),
                    value,
                },
                &[],
            );
        }
        dialog.apply(
            &DialogIntent::TextChanged {
                dialog: bootty_gpui::DialogId::new("another-dialog"),
                value: "wrong-project".to_owned(),
            },
            &[],
        );
        let row = dialog.spec().rows[0].clone();
        assert_eq!(
            row.detail, None,
            "editing clears the earlier validation error"
        );
        assert_eq!(
            activate_picker_row(&mut dialog, &row, &[]),
            Some(NewSessionPickerEvent::CreateWorktree {
                repo: path.clone(),
                request: bootty_git::WorktreeRequest {
                    branch,
                    name: folder,
                    start_ref: use_head.then(|| "HEAD".to_owned())
                },
            })
        );
        Ok(())
    })
    .expect("worktree form property");
}

#[rstest::rstest]
#[case::absolute_home(false)]
#[case::abbreviated_home(true)]
fn directory_search_accepts_both_home_path_spellings(#[case] abbreviated: bool) {
    let home = bootty_git::home_dir().expect("home directory");
    let path = home
        .join("bootty-picker-project")
        .to_string_lossy()
        .into_owned();
    let mut dialog = NewSessionDialog::from_projects(vec![project(&path, false)]);
    let filter = if abbreviated {
        "~/bootty-picker-project".to_owned()
    } else {
        path.clone()
    };
    dialog.apply(
        &DialogIntent::TextChanged {
            dialog: dialog.spec().id,
            value: filter,
        },
        &[],
    );
    assert!(project_row(&dialog.spec(), &path).action.is_some());
}

#[rstest::rstest]
#[case("codex", bootty_agents::AgentKind::Codex)]
#[case("claude", bootty_agents::AgentKind::Claude)]
#[case("pi", bootty_agents::AgentKind::Pi)]
fn setup_launches_the_selected_native_provider_in_the_selected_checkout(
    #[case] row_id: &str,
    #[case] provider: bootty_agents::AgentKind,
) {
    let mut dialog = NewSessionDialog::from_projects(Vec::new());
    dialog.set_checkout("/projects/feature-checkout".to_owned());
    select_launcher(&mut dialog, row_id);
    let spec = dialog.spec();
    let row = spec.rows.iter().find(|row| row.id.0 == "submit").unwrap();
    assert_eq!(
        activate_picker_row(&mut dialog, row, &[]),
        Some(NewSessionPickerEvent::CreateAgentSession {
            cwd: "/projects/feature-checkout".to_owned(),
            provider,
            prompt: String::new(),
        })
    );
}

#[rstest::rstest]
fn native_directory_selection_is_reviewable_before_session_creation() {
    let mut dialog = NewSessionDialog::from_projects(Vec::new());
    let spec = dialog.spec();
    let browse = spec
        .rows
        .iter()
        .find(|row| row.id.0 == "browse-directory")
        .unwrap();
    assert_eq!(
        activate_picker_row(&mut dialog, browse, &[]),
        Some(NewSessionPickerEvent::BrowseDirectory)
    );
    // Choosing a folder changes the draft. The explicit checkout and launch choices still apply.
    dialog.set_directory("/newly-selected/folder".to_owned());
    let selected = dialog.spec();
    assert_eq!(selected.text.as_deref(), Some("/newly-selected/folder"));
    assert!(project_row(&selected, "/newly-selected/folder").enabled);
}

#[rstest::rstest]
fn project_picker_projects_real_artwork_with_name_and_path() {
    let artwork = bootty_git::ProjectIcon {
        source: "icon.png".to_owned(),
        width: 1,
        height: 1,
        bgra: vec![30, 20, 10, 255],
    };
    let mut entry = project("/projects/bootty", false);
    entry.icon = Some(artwork.clone());
    let dialog = NewSessionDialog::from_projects(vec![entry]);
    let spec = dialog.spec();
    let row = project_row(&spec, "/projects/bootty");
    assert_eq!(row.label, "bootty");
    assert_eq!(row.detail.as_deref(), Some("/projects/bootty"));
    assert_eq!(row.artwork.as_deref(), Some(&artwork));
}

#[rstest::rstest]
#[case("Fix browser focus!", "task/fix-browser-focus")]
#[case("   🥟   ", "task/new-task")]
#[case("Review  API / permissions", "task/review-api-permissions")]
fn generated_worktree_has_a_reviewable_destination_and_preserves_task(
    worktree_project: assert_fs::TempDir,
    #[case] task: &str,
    #[case] branch: &str,
) {
    let path = worktree_project.path().to_string_lossy().into_owned();
    let mut dialog = NewSessionDialog::from_projects(Vec::new());
    dialog.set_checkout(path.clone());
    dialog.apply(
        &DialogIntent::TextChanged {
            dialog: dialog.spec().id,
            value: task.to_owned(),
        },
        &[],
    );
    let row = dialog
        .spec()
        .rows
        .into_iter()
        .find(|row| row.id.0 == "choose-checkout")
        .unwrap();
    assert_eq!(activate_picker_row(&mut dialog, &row, &[]), None);
    let row = dialog
        .spec()
        .rows
        .into_iter()
        .find(|row| row.id.0 == "new-worktree")
        .unwrap();
    assert_eq!(activate_picker_row(&mut dialog, &row, &[]), None);
    let spec = dialog.spec();
    assert_eq!(spec.text.as_deref(), Some(branch));
    let row = spec.rows[0].clone();
    let request = bootty_git::WorktreeRequest {
        branch: branch.to_owned(),
        name: None,
        start_ref: None,
    };
    let destination = request.destination(&path).unwrap();
    assert_eq!(
        spec.footer,
        Some(format!(
            "Destination: {}",
            request
                .destination(
                    &worktree_project
                        .path()
                        .canonicalize()
                        .unwrap()
                        .to_string_lossy()
                )
                .unwrap()
        ))
    );
    assert_eq!(
        activate_picker_row(&mut dialog, &row, &[]),
        Some(NewSessionPickerEvent::CreateWorktree {
            repo: path,
            request
        })
    );
    dialog.set_checkout(destination.clone());
    let spec = dialog.spec();
    assert_eq!(spec.role, bootty_gpui::DialogRole::SessionLaunch);
    assert_eq!(spec.text.as_deref(), Some(task));
    assert_eq!(spec.footer.as_deref(), Some(destination.as_str()));
}

#[rstest::rstest]
fn filtering_projects_selects_the_match_instead_of_the_return_action() {
    let mut dialog = NewSessionDialog::from_projects(vec![project("/projects/other", false)]);
    dialog.set_checkout("/projects/current".to_owned());
    let switch = dialog
        .spec()
        .rows
        .into_iter()
        .find(|row| row.id.0 == "choose-project")
        .unwrap();
    activate_picker_row(&mut dialog, &switch, &[]);
    dialog.apply(
        &DialogIntent::TextChanged {
            dialog: dialog.spec().id,
            value: "/projects/other".to_owned(),
        },
        &[],
    );
    let rows = dialog.spec().rows;
    assert!(!rows.iter().any(|row| row.id.0 == "current-checkout"));
    let row = rows
        .iter()
        .find(|row| row.enabled && row.action.is_some())
        .unwrap();
    assert_eq!(row.id.0, "project:/projects/other");
    activate_picker_row(&mut dialog, row, &[]);
    assert_eq!(dialog.spec().title, "Choose checkout");
}

#[rstest::rstest]
fn dedicated_launch_keeps_provider_and_draft_when_modes_change() {
    let mut dialog = NewSessionDialog::from_projects(Vec::new());
    dialog.set_checkout("/projects/current".to_owned());
    let id = dialog.spec().id;
    let draft = "Review this change\nKeep the public API";
    dialog.apply(
        &DialogIntent::TextChanged {
            dialog: id.clone(),
            value: draft.to_owned(),
        },
        &[],
    );
    select_launcher(&mut dialog, "claude");
    select_launcher(&mut dialog, "terminal");
    let spec = dialog.spec();
    assert_eq!(spec.role, bootty_gpui::DialogRole::SessionLaunch);
    assert_eq!(spec.text_label.as_deref(), Some("Command (optional)"));
    assert_eq!(spec.rows[0].label, "Start command");
    assert_eq!(
        dialog.apply(
            &DialogIntent::FieldChanged {
                dialog: id,
                field: "mode".to_owned(),
                value: "Agent".to_owned(),
            },
            &[]
        ),
        None
    );
    let spec = dialog.spec();
    assert_eq!(spec.text.as_deref(), Some(draft));
    assert_eq!(spec.rows[0].label, "Start Claude");
    assert_eq!(
        activate_picker_row(&mut dialog, &spec.rows[0], &[]),
        Some(NewSessionPickerEvent::CreateAgentSession {
            cwd: "/projects/current".to_owned(),
            provider: bootty_agents::AgentKind::Claude,
            prompt: draft.to_owned(),
        })
    );
}
