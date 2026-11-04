use std::sync::Arc;

use assert_fs::TempDir;
use bootty_mux::{
    command::MuxCommand,
    native::NativeBackend,
    repository::WorkspaceRepository,
    session_membership::{SessionMembership, SessionState, WorkspaceSession},
    session_snapshot::{SavedTerminalPane, SavedTerminalSession, SavedTerminalWindow},
    snapshot::MuxSessionTag,
};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn checkpoint() -> SavedTerminalSession {
    SavedTerminalSession {
        captured_at: 12,
        session_id: "logical".into(),
        backend_id: "original".into(),
        active_window_id: Some("window".into()),
        windows: vec![SavedTerminalWindow {
            id: "window".into(),
            backend_id: "old-window".into(),
            title: "Work".into(),
            focused_pane_id: "pane".into(),
            layout: None,
            panes: vec![SavedTerminalPane {
                id: "pane".into(),
                backend_id: "old-pane".into(),
                cwd: "/tmp".into(),
                cols: 0,
                rows: 0,
                text: "prior output\n".into(),
                omitted_lines: 2,
            }],
        }],
    }
}

#[rstest]
#[case::unknown_geometry(0, 0, true)]
#[case::known_geometry(80, 24, true)]
#[case::missing_rows(80, 0, false)]
#[case::missing_cols(0, 24, false)]
fn checkpoint_geometry_remains_truthful(
    mut checkpoint: SavedTerminalSession,
    #[case] cols: u16,
    #[case] rows: u16,
    #[case] accepted: bool,
) {
    let pane = checkpoint
        .windows
        .first_mut()
        .unwrap()
        .panes
        .first_mut()
        .unwrap();
    pane.cols = cols;
    pane.rows = rows;
    assert_eq!(checkpoint.validate().is_ok(), accepted);
}

#[rstest]
#[case::unix_absolute("/work", true)]
#[case::local_platform_absolute(if cfg!(windows) { r"C:\work" } else { "/work" }, true)]
#[case::relative("work", false)]
#[case::drive_relative(r"C:work", false)]
#[case::shell_expansion("~/work", false)]
#[case::empty("", false)]
fn checkpoint_cwd_accepts_absolute_paths_without_shell_expansion(
    mut checkpoint: SavedTerminalSession,
    #[case] cwd: &str,
    #[case] accepted: bool,
) {
    checkpoint
        .windows
        .first_mut()
        .unwrap()
        .panes
        .first_mut()
        .unwrap()
        .cwd = cwd.to_owned();
    assert_eq!(checkpoint.validate().is_ok(), accepted);
}

/// Checkpoints saved while panes could hold native conversations keep loading.
#[rstest]
fn checkpoints_with_retired_native_agent_metadata_still_load(checkpoint: SavedTerminalSession) {
    let mut value = serde_json::to_value(&checkpoint).unwrap();
    value["windows"][0]["panes"][0]["native_agent"] = "native:codex:captured".into();
    let loaded: SavedTerminalSession = serde_json::from_value(value).unwrap();
    assert_eq!(loaded, checkpoint);
}

#[rstest]
fn native_restoration_preserves_saved_topology_and_rejects_adoption(
    checkpoint: SavedTerminalSession,
) {
    let directory = TempDir::new().expect("create isolated workspace");
    bootty_test_support::native_mux::local(directory.path()).unwrap();
    let backend = NativeBackend::for_workspace(directory.path());
    let command = MuxCommand::RestoreSession {
        session_id: "restored".into(),
        tag: MuxSessionTag {
            identity: Some(checkpoint.session_id.clone()),
            space: Some("space".into()),
        },
        snapshot: checkpoint,
    };
    backend
        .execute(command.clone())
        .expect("restore saved topology");
    let before = backend.snapshot().expect("read native topology");
    let session = before.sessions.first().unwrap();
    assert_eq!(session.tag.identity.as_deref(), Some("logical"));
    assert_eq!(session.windows.first().unwrap().name, "Work");
    assert_eq!(
        session
            .windows
            .first()
            .unwrap()
            .panes
            .first()
            .unwrap()
            .cwd
            .as_deref(),
        Some("/tmp")
    );
    assert_ne!(
        session
            .windows
            .first()
            .unwrap()
            .panes
            .first()
            .unwrap()
            .pane_id
            .as_deref(),
        Some("old-pane")
    );
    assert!(backend.execute(command).is_err());
    assert_eq!(backend.snapshot().expect("read native topology"), before);
}

#[rstest]
fn checkpoint_reload_and_lifecycle_write_preserve_committed_history(
    checkpoint: SavedTerminalSession,
) {
    let directory = TempDir::new().expect("create isolated workspace");
    let config = directory.path().join("config.toml");
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config).expect("open workspace repository");
    let scope = snapshot.spaces().first().unwrap().id();
    let saved = WorkspaceSession {
        identity: "logical".into(),
        backend_name: "original".into(),
        display_name: "Purpose".into(),
        explicit: true,
        cwd: "/tmp".into(),
        state: SessionState::default(),
        terminal_snapshot: Some(Arc::new(checkpoint.clone())),
    };
    repository
        .commit_binding_state(
            scope,
            &SessionMembership::from_sessions(vec![saved.clone()]),
        )
        .expect("commit saved workspace state");
    let (_, reloaded) = WorkspaceRepository::open(&config).expect("open workspace repository");
    assert_eq!(
        reloaded
            .spaces()
            .first()
            .unwrap()
            .binding()
            .sessions()
            .get("logical"),
        Some(&saved)
    );
    let mut earlier_ui = saved.clone();
    earlier_ui.terminal_snapshot = None;
    earlier_ui.state.pinned = true;
    repository
        .commit_binding_state(scope, &SessionMembership::from_sessions(vec![earlier_ui]))
        .expect("commit lifecycle change without replacing history");
    let (_, reloaded) = WorkspaceRepository::open(&config).expect("open workspace repository");
    let current = reloaded
        .spaces()
        .first()
        .unwrap()
        .binding()
        .sessions()
        .get("logical")
        .unwrap();
    assert!(current.state.pinned);
    assert_eq!(current.terminal_snapshot.as_deref(), Some(&checkpoint));
    assert_eq!(current.display_name, saved.display_name);
}

proptest! {
    #[test]
    fn checkpoint_plain_history_rejects_every_terminal_control(control in prop_oneof![0u8..9,11u8..13,14u8..32,Just(127u8)]) {
        let mut saved = checkpoint();
        saved.windows.first_mut().unwrap().panes.first_mut().unwrap().text = format!("saved{}output", char::from(control));
        prop_assert!(saved.validate().is_err());
    }
}

#[rstest]
#[case::plain("old plain checkpoint\n", true)]
#[case::styled("\x1b[1;38;2;10;20;30mcolored\x1b[0m\n", true)]
#[case::underline("\x1b[4:3;48;5;45mstyled\x1b[0m", true)]
#[case::clipboard("\x1b]52;c;YWJj\x07", false)]
#[case::query("\x1b[6n", false)]
#[case::cursor("\x1b[2J", false)]
#[case::malformed_rgb("\x1b[38;2;256;0;0m", false)]
fn persisted_history_admits_only_bounded_text_and_styles(
    mut checkpoint: SavedTerminalSession,
    #[case] text: &str,
    #[case] accepted: bool,
) {
    checkpoint
        .windows
        .first_mut()
        .unwrap()
        .panes
        .first_mut()
        .unwrap()
        .text = text.into();
    let encoded = serde_json::to_string(&checkpoint).expect("persist checkpoint shape");
    let decoded: SavedTerminalSession =
        serde_json::from_str(&encoded).expect("read checkpoint shape");
    assert_eq!(decoded, checkpoint);
    assert_eq!(decoded.validate().is_ok(), accepted);
}

#[rstest]
fn failed_selection_write_preserves_both_saved_and_backend_selection(
    checkpoint: SavedTerminalSession,
) {
    let directory = TempDir::new().expect("create isolated workspace");
    let config = directory.path().join("config.toml");
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config).expect("open workspace repository");
    let scope = snapshot.spaces().first().unwrap().id();
    let saved = WorkspaceSession {
        identity: "logical".into(),
        backend_name: "original".into(),
        display_name: "Purpose".into(),
        explicit: true,
        cwd: "/tmp".into(),
        state: SessionState::default(),
        terminal_snapshot: Some(Arc::new(checkpoint)),
    };
    repository
        .commit_binding_state(scope, &SessionMembership::from_sessions(vec![saved]))
        .expect("commit saved task");
    repository
        .set_binding_saved_selection(scope, Some("logical"), "original", Some("old-window"))
        .expect("commit saved workspace state");
    let database = rusqlite::Connection::open(directory.path().join("session-order.sqlite3"))
        .expect("open checkpoint database");
    database.execute_batch("CREATE TRIGGER reject_selection BEFORE UPDATE OF selected_session_identity ON workspace_spaces BEGIN SELECT RAISE(FAIL, 'selection failure'); END;").expect("install selection failure boundary");
    assert!(
        repository
            .set_binding_saved_selection(scope, Some("logical"), "new-backend", Some("new-window"))
            .is_err()
    );
    let (_, restored) = WorkspaceRepository::open(&config).expect("open workspace repository");
    let binding = restored.spaces().first().unwrap().binding();
    assert_eq!(binding.selected_session_identity(), Some("logical"));
    assert_eq!(binding.selection().unwrap().session_id(), "original");
    assert_eq!(binding.selection().unwrap().window_id(), Some("old-window"));
    assert_eq!(
        binding.sessions().get("logical").unwrap().display_name,
        "Purpose"
    );
}

#[rstest]
fn supported_lifecycle_schema_evolves_without_losing_saved_work() {
    let directory = TempDir::new().expect("create isolated workspace");
    let config = directory.path().join("config.toml");
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config).expect("open workspace repository");
    let scope = snapshot.spaces().first().unwrap().id();
    let saved = WorkspaceSession {
        identity: "logical".into(),
        backend_name: "original".into(),
        display_name: "Purpose".into(),
        explicit: true,
        cwd: "/tmp".into(),
        state: SessionState {
            pinned: true,
            ..SessionState::default()
        },
        terminal_snapshot: None,
    };
    repository
        .commit_binding_state(
            scope,
            &SessionMembership::from_sessions(vec![saved.clone()]),
        )
        .expect("commit saved workspace state");
    let database = rusqlite::Connection::open(directory.path().join("session-order.sqlite3"))
        .expect("open checkpoint database");
    database.execute_batch("ALTER TABLE workspace_sessions DROP COLUMN terminal_snapshot; ALTER TABLE workspace_spaces DROP COLUMN selected_session_identity;").expect("construct supported version six schema");
    database
        .pragma_update(None, "user_version", 6)
        .expect("mark supported version six schema");
    let (_, restored) = WorkspaceRepository::open(&config).expect("open workspace repository");
    assert_eq!(
        restored
            .spaces()
            .first()
            .unwrap()
            .binding()
            .sessions()
            .get("logical"),
        Some(&saved)
    );
    assert_eq!(
        database
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("read evolved schema revision"),
        7
    );
}

#[fixture]
fn live_workspace() -> anyhow::Result<(
    TempDir,
    bootty_mux::workspace::WorkspaceRuntime,
    bootty_mux::controller::SpaceId,
    String,
)> {
    use bootty_config::config::{AppearanceVariant, BoottyConfig};
    let directory = TempDir::new()?;
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };

    let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    let space = snapshot
        .spaces()
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing Space"))?;
    let scope = space.id();
    let saved = WorkspaceSession {
        identity: "logical".into(),
        backend_name: "original".into(),
        display_name: "Purpose".into(),
        explicit: true,
        cwd: directory.path().to_string_lossy().into_owned(),
        state: SessionState::default(),
        terminal_snapshot: None,
    };
    repository.commit_binding_state(
        scope,
        &SessionMembership::from_sessions(vec![saved.clone()]),
    )?;
    bootty_test_support::native_mux::local(&config.config_path)?;
    let backend = NativeBackend::for_workspace(&config.config_path);
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: saved.backend_name.clone(),
        cwd: saved.cwd,
        tag: MuxSessionTag {
            identity: Some(saved.identity),
            space: Some(space.remote_id().to_owned()),
        },
        // A plain sh never retitles its window, so snapshots stay comparable across hosts.
        argv: Some(vec!["/bin/sh".into()]),
    })?;
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});

    bootty_test_support::native_mux::local(&config.config_path)?;
    let mut workspace = bootty_mux::workspace::WorkspaceRuntime::open(
        &config,
        "main",
        AppearanceVariant::Light,
        Arc::clone(&repaint),
    )?;
    let binding = workspace
        .binding_mut(scope)
        .ok_or_else(|| anyhow::anyhow!("missing Binding"))?;
    binding
        .mux_mut()
        .refresh_sessions(&repaint, &config.multiplexer, std::time::Duration::ZERO);
    let pane = binding
        .session_attachment("logical")
        .and_then(|session| session.windows.first())
        .and_then(|window| window.panes.first())
        .and_then(|pane| pane.pane_id.clone())
        .ok_or_else(|| anyhow::anyhow!("missing exact pane"))?;
    Ok((directory, workspace, scope, pane))
}

fn pane_capture(
    pane: &str,
    cwd: Option<&str>,
    text: &str,
) -> bootty_mux::session_snapshot::SessionPaneCapture {
    bootty_mux::session_snapshot::SessionPaneCapture {
        pane_id: pane.to_owned(),
        cwd: cwd.map(str::to_owned),
        cols: 0,
        rows: 0,
        text: text.to_owned(),
        omitted_lines: 0,
    }
}

#[rstest]
#[case::without_checkpoint(false)]
#[case::with_checkpoint(true)]
fn reopening_a_window_before_its_first_frame_reattaches_the_existing_native_session(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
    #[case] save_checkpoint: bool,
) {
    use bootty_config::config::{AppearanceVariant, BoottyConfig};
    let (directory, mut original, scope, pane) = live_workspace.unwrap();
    if save_checkpoint {
        let generation = original.binding(scope).unwrap().mux().binding_generation();
        let receipt = original
            .prepare_session_checkpoint(scope, "logical", generation, 10)
            .unwrap()
            .save(vec![pane_capture(&pane, None, "retained output")])
            .unwrap();
        assert!(original.publish_session_checkpoint(receipt).unwrap());
    }
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };

    bootty_test_support::native_mux::local(&config.config_path).unwrap();
    let backend = NativeBackend::for_workspace(&config.config_path);
    let before = backend.snapshot().unwrap();
    let mut reopened = bootty_mux::workspace::WorkspaceRuntime::open(
        &config,
        "second-window",
        AppearanceVariant::Light,
        Arc::new(|| {}),
    )
    .unwrap();
    let (command, mutation) = reopened.begin_session_reopen(scope, "logical").unwrap();
    assert_eq!(
        command,
        MuxCommand::ActivateWindow {
            session_id: before.sessions[0].id.clone(),
            window_id: before.sessions[0].windows[0].id.clone(),
        }
    );
    assert!(mutation.is_none());
    assert_eq!(backend.snapshot().unwrap(), before);
    assert_eq!(
        reopened
            .binding(scope)
            .unwrap()
            .session_attachment("logical"),
        before.sessions.first()
    );
}

/// A checkpoint stored with since-retired pane metadata is still the prior state the next save
/// replaces, so saving keeps working after an upgrade.
#[rstest]
fn checkpoints_replace_stored_state_with_retired_native_agent_metadata(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
) {
    let (directory, mut workspace, scope, pane) =
        live_workspace.expect("create live exact-tagged workspace");
    let generation = workspace.binding(scope).unwrap().mux().binding_generation();
    let receipt = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 10)
        .expect("prepare first checkpoint")
        .save(vec![pane_capture(&pane, None, "first history")])
        .expect("persist first checkpoint");
    assert!(workspace.publish_session_checkpoint(receipt).unwrap());
    let database = rusqlite::Connection::open(directory.path().join("session-order.sqlite3"))
        .expect("open checkpoint database");
    database
        .execute(
            "UPDATE workspace_sessions SET terminal_snapshot = json_set(terminal_snapshot, '$.windows[0].panes[0].native_agent', 'native:codex:old') WHERE identity = 'logical'",
            [],
        )
        .expect("store retired pane metadata");
    workspace
        .prepare_session_checkpoint(scope, "logical", generation, 11)
        .expect("prepare next checkpoint")
        .save(vec![pane_capture(&pane, None, "next history")])
        .expect("replace the stored checkpoint");
    let stored: String = database
        .query_row(
            "SELECT terminal_snapshot FROM workspace_sessions WHERE identity = 'logical'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        stored.contains("next history") && !stored.contains("native_agent"),
        "{stored}"
    );
}

#[rstest]
fn stale_captures_and_closed_bindings_cannot_overwrite_a_committed_checkpoint(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
) {
    let (directory, mut workspace, scope, pane) =
        live_workspace.expect("create live exact-tagged workspace");
    let generation = workspace.binding(scope).unwrap().mux().binding_generation();
    let first = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 10)
        .expect("prepare first checkpoint");
    let stale = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 11)
        .expect("prepare stale checkpoint");
    let captures = vec![pane_capture(&pane, Some("/var/tmp"), "committed history")];
    let receipt = first
        .save(captures.clone())
        .expect("persist exact captured checkpoint");
    assert!(
        workspace
            .publish_session_checkpoint(receipt)
            .expect("publish committed checkpoint")
    );
    let identical = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 10)
        .expect("prepare identical checkpoint");
    let receipt = identical
        .save(captures)
        .expect("persist exact captured checkpoint");
    assert!(
        workspace
            .publish_session_checkpoint(receipt)
            .expect("publish committed checkpoint"),
        "same-second identical committed checkpoints remain admitted"
    );
    assert!(
        stale
            .save(vec![pane_capture(&pane, None, "stale history")])
            .is_err()
    );
    let backwards = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 9)
        .expect("prepare backwards checkpoint");
    assert!(
        backwards
            .save(vec![pane_capture(&pane, None, "earlier history")])
            .is_err()
    );
    let database = rusqlite::Connection::open(directory.path().join("session-order.sqlite3"))
        .expect("open checkpoint database");
    database.execute_batch("CREATE TRIGGER reject_checkpoint BEFORE UPDATE OF terminal_snapshot ON workspace_sessions BEGIN SELECT RAISE(FAIL, 'checkpoint failure'); END;").expect("install checkpoint write failure boundary");
    let failed_write = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 12)
        .expect("prepare failed_write checkpoint");
    assert!(
        failed_write
            .save(vec![pane_capture(&pane, None, "failed write history")])
            .is_err()
    );
    database
        .execute_batch("DROP TRIGGER reject_checkpoint;")
        .expect("remove checkpoint write failure boundary");
    let retired = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 12)
        .expect("prepare retired checkpoint");
    drop(workspace);
    assert!(
        retired
            .save(vec![pane_capture(&pane, None, "closed owner history")])
            .is_err()
    );
    let (_, restored) = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("reload last admitted checkpoint");
    let snapshot = restored
        .spaces()
        .first()
        .unwrap()
        .binding()
        .sessions()
        .get("logical")
        .unwrap()
        .terminal_snapshot
        .as_ref()
        .unwrap();
    assert_eq!(snapshot.captured_at, 10);
    let captured_pane = snapshot.windows.first().unwrap().panes.first().unwrap();
    assert_eq!(captured_pane.text, "committed history");
    assert_eq!(captured_pane.cwd, "/var/tmp");
}

#[rstest]
fn native_restore_with_missing_saved_cwd_keeps_checkpoint_and_creates_nothing(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
) {
    use bootty_config::config::BoottyConfig;
    use bootty_mux::workspace::SessionRequestError;
    let (directory, mut workspace, scope, pane) =
        live_workspace.expect("create live exact-tagged workspace");
    let config_path = directory.path().join("config.toml");
    let missing_cwd = directory.path().join("missing-directory");
    std::fs::create_dir(&missing_cwd).expect("create original working directory");
    let generation = workspace.binding(scope).unwrap().mux().binding_generation();
    let receipt = workspace
        .prepare_session_checkpoint(scope, "logical", generation, 10)
        .expect("prepare captured session")
        .save(vec![pane_capture(
            &pane,
            Some(missing_cwd.to_str().unwrap()),
            "retained output",
        )])
        .expect("persist original working directory and history");
    assert!(workspace.publish_session_checkpoint(receipt).unwrap());
    std::fs::remove_dir(&missing_cwd).expect("remove captured working directory");
    let saved = workspace
        .binding(scope)
        .unwrap()
        .sessions()
        .get("logical")
        .unwrap()
        .clone();
    bootty_test_support::native_mux::local(&config_path).unwrap();
    let backend = NativeBackend::for_workspace(&config_path);
    let original = workspace
        .binding(scope)
        .unwrap()
        .session_attachment("logical")
        .unwrap()
        .id
        .clone();
    backend
        .execute(MuxCommand::DitchSession {
            session_id: original,
        })
        .expect("remove original attachment");
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    workspace
        .binding_mut(scope)
        .unwrap()
        .mux_mut()
        .refresh_sessions(
            &repaint,
            &BoottyConfig::default().multiplexer,
            std::time::Duration::ZERO,
        );
    let before = backend.snapshot().unwrap();
    assert!(matches!(
        workspace.begin_session_reopen(scope, "logical"),
        Err(SessionRequestError::Invalid(_))
    ));
    assert_eq!(backend.snapshot().unwrap(), before);
    assert_eq!(
        workspace.binding(scope).unwrap().sessions().get("logical"),
        Some(&saved)
    );
    let (_repository, restored) = WorkspaceRepository::open(&config_path).unwrap();
    assert_eq!(
        restored
            .spaces()
            .first()
            .unwrap()
            .binding()
            .sessions()
            .get("logical"),
        Some(&saved)
    );
}

#[rstest]
#[case::default_shell_tab(false)]
#[case::default_shell_split(true)]
fn legacy_native_topology_creation_tracks_only_its_new_pane(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
    #[case] split: bool,
) {
    use bootty_config::config::BoottyConfig;
    use bootty_mux::command::MuxSplitDirection;
    let (directory, mut workspace, scope, original_pane) = live_workspace.unwrap();
    let original = workspace
        .binding(scope)
        .unwrap()
        .session_attachment("logical")
        .unwrap();
    let original_id = original.id.clone();
    let original_window = original.windows.first().unwrap().id.clone();
    let command = if split {
        MuxCommand::SplitPane {
            session_id: original_id.clone(),
            pane_id: Some(original_pane.clone()),
            direction: MuxSplitDirection::Right,
        }
    } else {
        MuxCommand::NewWindow {
            session_id: original_id.clone(),
            cwd: None,
            argv: None,
        }
    };
    bootty_test_support::native_mux::local(&directory.path().join("config.toml")).unwrap();
    let backend = NativeBackend::for_workspace(&directory.path().join("config.toml"));
    backend.execute(command.clone()).unwrap();
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    workspace
        .binding_mut(scope)
        .unwrap()
        .mux_mut()
        .refresh_sessions(
            &repaint,
            &BoottyConfig::default().multiplexer,
            std::time::Duration::ZERO,
        );
    let starting = workspace
        .starting_session(scope, &command)
        .unwrap()
        .unwrap();
    assert_eq!(starting.session_id(), original_id);
    assert_eq!(starting.scope(), scope);
    assert!(!starting.created_session());
    assert_ne!(starting.pane_id(), original_pane);
    if split {
        assert_eq!(starting.window_id(), original_window);
    } else {
        assert_ne!(starting.window_id(), original_window);
    }
    assert!(workspace.holds_starting_session(&starting));
    let current = workspace
        .binding(scope)
        .unwrap()
        .session_attachment("logical")
        .unwrap();
    let window = current
        .windows
        .iter()
        .find(|window| window.id == starting.window_id())
        .unwrap();
    assert_eq!(window.anchor.pane_id.as_deref(), Some(starting.pane_id()));
    assert!(
        current
            .windows
            .iter()
            .flat_map(|window| &window.panes)
            .any(|pane| pane.pane_id.as_deref() == Some(&original_pane))
    );
}

fn sparse_native_topology(
    backend: &NativeBackend,
    session: &str,
    pane: &str,
) -> anyhow::Result<()> {
    use bootty_mux::command::MuxSplitDirection;
    let split = MuxCommand::SplitPane {
        session_id: session.to_owned(),
        pane_id: Some(pane.to_owned()),
        direction: MuxSplitDirection::Right,
    };
    backend.execute(split.clone())?;
    let removed = backend
        .snapshot()?
        .sessions
        .into_iter()
        .find(|item| item.id == session)
        .and_then(|item| {
            item.windows
                .first()
                .and_then(|window| window.anchor.pane_id.clone())
        })
        .ok_or_else(|| anyhow::anyhow!("missing intermediate pane"))?;
    backend.execute(split)?;
    backend.execute(MuxCommand::ClosePane {
        session_id: session.to_owned(),
        pane_id: Some(removed),
    })?;
    let new_window = MuxCommand::NewWindow {
        session_id: session.to_owned(),
        cwd: None,
        argv: None,
    };
    backend.execute(new_window.clone())?;
    let removed = backend
        .snapshot()?
        .sessions
        .into_iter()
        .find(|item| item.id == session)
        .and_then(|item| item.windows.into_iter().find(|window| window.active))
        .and_then(|window| window.anchor.pane_id)
        .ok_or_else(|| anyhow::anyhow!("missing intermediate tab"))?;
    backend.execute(new_window)?;
    backend.execute(MuxCommand::ClosePane {
        session_id: session.to_owned(),
        pane_id: Some(removed),
    })?;
    Ok(())
}

fn save_current_topology(
    workspace: &mut bootty_mux::workspace::WorkspaceRuntime,
    scope: bootty_mux::controller::SpaceId,
    at: i64,
) -> anyhow::Result<()> {
    let binding = workspace
        .binding(scope)
        .ok_or_else(|| anyhow::anyhow!("missing binding"))?;
    let generation = binding.mux().binding_generation();
    let session = binding
        .session_attachment("logical")
        .ok_or_else(|| anyhow::anyhow!("missing exact task attachment"))?;
    let captures = session
        .windows
        .iter()
        .flat_map(|window| &window.panes)
        .map(|pane| {
            pane.pane_id
                .as_deref()
                .map(|id| pane_capture(id, None, "retained output"))
                .ok_or_else(|| anyhow::anyhow!("missing pane id"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let receipt = workspace
        .prepare_session_checkpoint(scope, "logical", generation, at)
        .map_err(|error| anyhow::anyhow!("{error:?}"))?
        .save(captures)?;
    anyhow::ensure!(
        workspace.publish_session_checkpoint(receipt)?,
        "checkpoint not admitted"
    );
    Ok(())
}

fn restore_saved_topology(
    workspace: &mut bootty_mux::workspace::WorkspaceRuntime,
    scope: bootty_mux::controller::SpaceId,
) -> anyhow::Result<()> {
    use bootty_mux::{controller::CommandSelection, executor};
    let (command, membership) = workspace
        .begin_session_reopen(scope, "logical")
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let submitted = executor::submit_authoritative_command_for_scope(
        workspace,
        &repaint,
        scope,
        command,
        membership.map(Box::new),
        None,
        CommandSelection::Preserve,
    )
    .ok_or_else(|| anyhow::anyhow!("missing restore binding"))?;
    let result = submitted.result.recv()?;
    let (result, sync_error) = executor::complete_authoritative_command(
        workspace,
        scope,
        &submitted.command,
        submitted.membership.as_deref(),
        result,
    )?;
    result.map_err(|error| anyhow::anyhow!("{error}"))?;
    anyhow::ensure!(sync_error.is_none(), "restore sync failed: {sync_error:?}");
    Ok(())
}

fn cold_workspace(
    original: &std::path::Path,
    repaint: &bootty_mux::RepaintHandle,
) -> anyhow::Result<(
    TempDir,
    bootty_config::config::BoottyConfig,
    bootty_mux::workspace::WorkspaceRuntime,
)> {
    use bootty_config::config::{AppearanceVariant, BoottyConfig};
    // A separate native owner namespace starts empty while loading the same durable Space.
    let cold = TempDir::new()?;
    std::fs::copy(
        original.join("session-order.sqlite3"),
        cold.path().join("session-order.sqlite3"),
    )?;
    let config = BoottyConfig {
        config_path: cold.path().join("config.toml"),
        ..BoottyConfig::default()
    };

    bootty_test_support::native_mux::local(&config.config_path)?;
    let workspace = bootty_mux::workspace::WorkspaceRuntime::open(
        &config,
        "main",
        AppearanceVariant::Light,
        Arc::clone(repaint),
    )?;
    Ok((cold, config, workspace))
}

#[rstest]
#[case::named("Purpose", true, false)]
#[case::generated("Purpose", false, false)]
#[case::backend_label("", false, false)]
#[case::occupied_name("Purpose", true, true)]
fn reopening_without_a_checkpoint_starts_fresh_and_keeps_saved_work(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
    #[case] display_name: &str,
    #[case] explicit: bool,
    #[case] occupied: bool,
) {
    let (original, workspace, scope, _) = live_workspace.unwrap();
    let mut saved = workspace
        .binding(scope)
        .unwrap()
        .sessions()
        .get("logical")
        .unwrap()
        .clone();
    assert!(saved.terminal_snapshot.is_none());
    saved.display_name = display_name.to_owned();
    saved.explicit = explicit;
    saved.state.pinned = true;
    drop(workspace);
    let (mut repository, _) =
        WorkspaceRepository::open(&original.path().join("config.toml")).unwrap();
    repository
        .commit_binding_state(
            scope,
            &SessionMembership::from_sessions(vec![saved.clone()]),
        )
        .unwrap();
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let (_cold, config, mut workspace) = cold_workspace(original.path(), &repaint).unwrap();
    bootty_test_support::native_mux::local(&config.config_path).unwrap();
    let backend = NativeBackend::for_workspace(&config.config_path);
    if occupied {
        backend
            .execute(MuxCommand::CreateProjectSession {
                session_id: saved.backend_name.clone(),
                cwd: saved.cwd.clone(),
                tag: MuxSessionTag::default(),
                argv: None,
            })
            .unwrap();
        workspace
            .binding_mut(scope)
            .unwrap()
            .mux_mut()
            .refresh_sessions(&repaint, &config.multiplexer, std::time::Duration::ZERO);
    }
    restore_saved_topology(&mut workspace, scope).unwrap();
    let binding = workspace.binding(scope).unwrap();
    let session = binding.session_attachment("logical").unwrap();
    let pane = session.windows.first().unwrap().panes.first().unwrap();
    assert_eq!(session.windows.len(), 1);
    assert_eq!(session.windows.first().unwrap().panes.len(), 1);
    assert_eq!(pane.cwd.as_deref(), Some(saved.cwd.as_str()));
    let pane_id = pane.pane_id.clone().unwrap();
    saved.backend_name = if occupied {
        "original-restore-1"
    } else {
        "original"
    }
    .to_owned();
    assert_eq!(session.name, saved.backend_name);
    assert_eq!(binding.sessions().get("logical"), Some(&saved));
    let session_id = session.id.clone();
    assert!(workspace.space_terminal_runtime(scope, &pane_id).is_some());
    let (command, mutation) = workspace.begin_session_reopen(scope, "logical").unwrap();
    assert!(
        matches!(command, MuxCommand::ActivateWindow { session_id: id, .. } if id == session_id)
    );
    assert!(mutation.is_none());
    assert_eq!(
        backend.snapshot().unwrap().sessions.len(),
        if occupied { 2 } else { 1 }
    );
    let (_repository, reloaded) = WorkspaceRepository::open(&config.config_path).unwrap();
    assert_eq!(
        reloaded
            .spaces()
            .first()
            .unwrap()
            .binding()
            .sessions()
            .get("logical"),
        Some(&saved)
    );
}

fn prepare_sparse_checkpoint(
    workspace: &mut bootty_mux::workspace::WorkspaceRuntime,
    backend: &NativeBackend,
    scope: bootty_mux::controller::SpaceId,
    original_pane: &str,
    repaint: &bootty_mux::RepaintHandle,
) -> anyhow::Result<WorkspaceSession> {
    let id = workspace
        .binding(scope)
        .and_then(|binding| binding.session_attachment("logical"))
        .map(|session| session.id.clone())
        .ok_or_else(|| anyhow::anyhow!("missing original session"))?;
    sparse_native_topology(backend, &id, original_pane)?;
    workspace
        .binding_mut(scope)
        .ok_or_else(|| anyhow::anyhow!("missing original binding"))?
        .mux_mut()
        .refresh_sessions(
            repaint,
            &bootty_config::config::BoottyConfig::default().multiplexer,
            std::time::Duration::ZERO,
        );
    save_current_topology(workspace, scope, 10)?;
    workspace
        .binding(scope)
        .and_then(|binding| binding.sessions().get("logical"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing saved checkpoint"))
}

#[rstest]
fn sparse_saved_keys_survive_cold_restore_then_new_tab_and_split(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
) {
    use bootty_mux::command::MuxSplitDirection;
    let (original_directory, mut workspace, scope, original_pane) = live_workspace.unwrap();
    let original_config = original_directory.path().join("config.toml");
    bootty_test_support::native_mux::local(&original_config).unwrap();
    let backend = NativeBackend::for_workspace(&original_config);
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let original_saved =
        prepare_sparse_checkpoint(&mut workspace, &backend, scope, &original_pane, &repaint)
            .unwrap();
    drop(workspace);
    let (_cold, config, mut workspace) =
        cold_workspace(original_directory.path(), &repaint).unwrap();
    restore_saved_topology(&mut workspace, scope).unwrap();
    bootty_test_support::native_mux::local(&config.config_path).unwrap();
    let backend = NativeBackend::for_workspace(&config.config_path);
    let restored_id = workspace
        .binding(scope)
        .unwrap()
        .session_attachment("logical")
        .unwrap()
        .id
        .clone();
    backend
        .execute(MuxCommand::NewWindow {
            session_id: restored_id.clone(),
            cwd: None,
            argv: None,
        })
        .unwrap();
    backend
        .execute(MuxCommand::SplitPane {
            session_id: restored_id,
            pane_id: None,
            direction: MuxSplitDirection::Right,
        })
        .unwrap();
    workspace
        .binding_mut(scope)
        .unwrap()
        .mux_mut()
        .refresh_sessions(&repaint, &config.multiplexer, std::time::Duration::ZERO);
    save_current_topology(&mut workspace, scope, 11).unwrap();
    let current = workspace
        .binding(scope)
        .unwrap()
        .sessions()
        .get("logical")
        .unwrap();
    assert_eq!(current.identity, original_saved.identity);
    assert_eq!(current.display_name, original_saved.display_name);
    let before = original_saved.terminal_snapshot.as_ref().unwrap();
    let after = current.terminal_snapshot.as_ref().unwrap();
    assert_eq!(after.windows.len(), 3);
    assert_eq!(
        after
            .windows
            .iter()
            .map(|window| window.panes.len())
            .sum::<usize>(),
        5
    );
    for window in &before.windows {
        let retained = after
            .windows
            .iter()
            .find(|item| item.id == window.id)
            .unwrap();
        assert_eq!(retained.title, window.title);
        assert_eq!(
            retained
                .panes
                .iter()
                .map(|pane| &pane.id)
                .collect::<Vec<_>>(),
            window.panes.iter().map(|pane| &pane.id).collect::<Vec<_>>()
        );
    }
    let (_, loaded) = WorkspaceRepository::open(&config.config_path).unwrap();
    assert_eq!(
        loaded
            .spaces()
            .first()
            .unwrap()
            .binding()
            .sessions()
            .get("logical"),
        Some(current)
    );
}

fn complete_pane_command(
    workspace: &mut bootty_mux::workspace::WorkspaceRuntime,
    scope: bootty_mux::controller::SpaceId,
    command: MuxCommand,
    cancellation: bootty_control::CommandCancellation,
) -> anyhow::Result<bootty_mux::controller::MuxCommandResult> {
    use bootty_mux::{controller::CommandSelection, executor};
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let deadline = std::time::Instant::now()
        .checked_add(executor::COMMAND_TIMEOUT)
        .ok_or_else(|| anyhow::anyhow!("pane command deadline does not fit"))?;
    let pending = executor::submit_authoritative_command_for_scope(
        workspace,
        &repaint,
        scope,
        command,
        None,
        Some((deadline, cancellation)),
        CommandSelection::Follow,
    )
    .ok_or_else(|| anyhow::anyhow!("pane binding is unavailable"))?;
    let result = pending.result.recv()?;
    let (result, sync_error) =
        executor::complete_authoritative_command(workspace, scope, &pending.command, None, result)?;
    anyhow::ensure!(sync_error.is_none(), "pane sync failed: {sync_error:?}");
    Ok(result)
}

struct PaneCreationFixture {
    directory: TempDir,
    workspace: bootty_mux::workspace::WorkspaceRuntime,
    scope: bootty_mux::controller::SpaceId,
    original_pane: String,
    before: bootty_mux::snapshot::MuxSession,
    layout: bootty_mux::pane_layout::PaneLayout,
}

#[fixture]
fn pane_creation_fixture(
    live_workspace: anyhow::Result<(
        TempDir,
        bootty_mux::workspace::WorkspaceRuntime,
        bootty_mux::controller::SpaceId,
        String,
    )>,
) -> anyhow::Result<PaneCreationFixture> {
    use bootty_control::CommandCancellation;
    use bootty_mux::command::MuxSplitDirection;
    let (directory, mut workspace, scope, original_pane) = live_workspace?;
    let original = workspace
        .binding(scope)
        .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?
        .session_attachment("logical")
        .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?
        .clone();
    let window_id = original
        .windows
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?
        .id
        .clone();
    let (first, _) = workspace
        .begin_pane_create(
            scope,
            &original.id,
            &original_pane,
            MuxSplitDirection::Right,
            Some(
                directory
                    .path()
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?,
            ),
            Vec::new(),
        )
        .map_err(|error| anyhow::anyhow!("initial pane preparation failed: {error:?}"))?;
    complete_pane_command(&mut workspace, scope, first, CommandCancellation::new())?
        .map_err(|error| anyhow::anyhow!("initial pane creation failed: {error}"))?;
    let before = workspace
        .binding(scope)
        .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?
        .session_attachment("logical")
        .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?
        .clone();
    let layout = workspace
        .binding(scope)
        .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?
        .window_pane_layout(&original.id, &window_id)
        .ok_or_else(|| anyhow::anyhow!("missing pane fixture topology"))?
        .clone();
    Ok(PaneCreationFixture {
        directory,
        workspace,
        scope,
        original_pane,
        before,
        layout,
    })
}

#[rstest]
#[case::right(bootty_mux::command::MuxSplitDirection::Right)]
#[case::down(bootty_mux::command::MuxSplitDirection::Down)]
fn literal_native_pane_creation_preserves_captured_placement(
    pane_creation_fixture: anyhow::Result<PaneCreationFixture>,
    #[case] direction: bootty_mux::command::MuxSplitDirection,
    #[values(false, true)] nonfocused_source: bool,
    #[values(false, true)] cancelled: bool,
) {
    use bootty_control::CommandCancellation;
    use bootty_mux::{command::MuxSplitDirection, pane_layout::SplitDirection};
    let PaneCreationFixture {
        directory,
        mut workspace,
        scope,
        original_pane,
        before,
        layout: initial_layout,
    } = pane_creation_fixture.unwrap();
    let window_id = before.windows.first().unwrap().id.clone();
    let other = before
        .windows
        .first()
        .unwrap()
        .anchor
        .pane_id
        .clone()
        .unwrap();
    assert_ne!(other, original_pane);
    assert_eq!(initial_layout.focused(), other);
    let source = if nonfocused_source {
        &original_pane
    } else {
        &other
    };
    let (command, _) = workspace
        .begin_pane_create(
            scope,
            &before.id,
            source,
            direction,
            Some(directory.path().to_str().unwrap()),
            Vec::new(),
        )
        .unwrap();
    let cancellation = CommandCancellation::new();
    if cancelled {
        assert!(cancellation.cancel());
    }
    let result = complete_pane_command(&mut workspace, scope, command, cancellation).unwrap();
    let binding = workspace.binding(scope).unwrap();
    let current = binding.session_attachment("logical").unwrap();
    let layout = binding.window_pane_layout(&before.id, &window_id).unwrap();
    if cancelled {
        assert!(matches!(
            result,
            Err(bootty_mux::controller::MuxCommandError::Cancelled)
        ));
        assert_eq!(current, &before);
        assert_eq!(layout.snapshot(), initial_layout.snapshot());
        assert_eq!(layout.focused(), initial_layout.focused());
    } else {
        assert!(result.is_ok(), "{result:?}");
        let new_pane = current
            .windows
            .first()
            .unwrap()
            .panes
            .iter()
            .filter_map(|pane| pane.pane_id.as_ref())
            .find(|pane| !initial_layout.contains(pane))
            .unwrap();
        let mut expected = initial_layout;
        assert!(expected.set_focus(source));
        expected.split_focused(
            new_pane.clone(),
            match direction {
                MuxSplitDirection::Right => SplitDirection::Right,
                MuxSplitDirection::Down => SplitDirection::Down,
            },
        );
        assert_eq!(layout.snapshot(), expected.snapshot());
        assert_eq!(layout.focused(), new_pane);
        assert_eq!(current.tag, before.tag);
        assert_eq!(current.windows.first().unwrap().panes.len(), 3);
        for pane in &before.windows.first().unwrap().panes {
            assert!(current.windows.first().unwrap().panes.contains(pane));
        }
    }
}

#[rstest]
#[case::same_server(false)]
#[case::replacement_server(true)]
fn sessions_ended_while_app_is_closed_only_recover_on_a_replacement_server(
    checkpoint: SavedTerminalSession,
    #[case] replacement_server: bool,
) -> anyhow::Result<()> {
    use bootty_config::config::{AppearanceVariant, BoottyConfig};
    use bootty_mux::workspace::WorkspaceRuntime;
    let directory = TempDir::new()?;
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    bootty_test_support::native_mux::topology(&config.config_path)?;
    let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    let space = &snapshot.spaces()[0];
    let scope = space.id();
    repository.commit_binding_state(
        scope,
        &SessionMembership::from_sessions(vec![WorkspaceSession {
            identity: "logical".into(),
            backend_name: "original".into(),
            display_name: "Purpose".into(),
            explicit: true,
            cwd: directory.path().to_string_lossy().into_owned(),
            state: SessionState::default(),
            terminal_snapshot: Some(Arc::new(checkpoint)),
        }]),
    )?;
    let backend = NativeBackend::for_workspace(&config.config_path);
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: "original".into(),
        cwd: directory.path().to_string_lossy().into_owned(),
        tag: MuxSessionTag {
            identity: Some("logical".into()),
            space: Some(space.remote_id().into()),
        },
        argv: None,
    })?;
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let mut app = WorkspaceRuntime::open(
        &config,
        "main",
        AppearanceVariant::Light,
        Arc::clone(&repaint),
    )?;
    app.binding_mut(scope).unwrap().mux_mut().refresh_sessions(
        &repaint,
        &config.multiplexer,
        std::time::Duration::ZERO,
    );
    app.reconcile_binding_states()?;
    drop(app);
    let session = backend.snapshot()?.sessions[0].id.clone();
    backend.execute(MuxCommand::DitchSession {
        session_id: session,
    })?;
    let replacement = TempDir::new()?;
    let reopened_config = if replacement_server {
        std::fs::copy(
            directory.path().join("session-order.sqlite3"),
            replacement.path().join("session-order.sqlite3"),
        )?;
        BoottyConfig {
            config_path: replacement.path().join("config.toml"),
            ..BoottyConfig::default()
        }
    } else {
        config
    };
    bootty_test_support::native_mux::topology(&reopened_config.config_path)?;
    let mut reopened = WorkspaceRuntime::open(
        &reopened_config,
        "main",
        AppearanceVariant::Light,
        Arc::clone(&repaint),
    )?;
    reopened
        .binding_mut(scope)
        .unwrap()
        .mux_mut()
        .refresh_sessions(
            &repaint,
            &reopened_config.multiplexer,
            std::time::Duration::ZERO,
        );
    reopened.reconcile_binding_states()?;
    anyhow::ensure!(
        reopened
            .binding(scope)
            .unwrap()
            .sessions()
            .contains("logical")
            == replacement_server,
        "membership must follow server continuity"
    );
    if replacement_server {
        restore_saved_topology(&mut reopened, scope)?;
    } else {
        anyhow::ensure!(
            reopened.begin_session_reopen(scope, "logical").is_err(),
            "ended session must not restore"
        );
    }
    anyhow::ensure!(
        NativeBackend::for_workspace(&reopened_config.config_path)
            .snapshot()?
            .sessions
            .len()
            == usize::from(replacement_server),
        "only a replacement server restores the checkpoint"
    );
    Ok(())
}

#[rstest]
fn session_moves_require_two_local_spaces() -> anyhow::Result<()> {
    use bootty_config::config::{AppearanceVariant, BoottyConfig, SshRemoteConfig};
    use bootty_mux::repository::{SpaceMuxOverride, SpaceRemoteOverride};
    let directory = TempDir::new()?;
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    bootty_test_support::native_mux::topology(&config.config_path)?;
    let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    let local = snapshot.spaces()[0].id();
    let remote = SpaceRemoteOverride::Inline(
        SshRemoteConfig {
            host: "must-not-connect.invalid".into(),
            user: None,
            port: None,
            program: "must-not-run-ssh".into(),
            args: vec![],
        }
        .into(),
    );
    let mut ids = Vec::new();
    for (name, remote) in [
        ("local", SpaceRemoteOverride::Local),
        ("remote-a", remote.clone()),
        ("remote-b", remote),
    ] {
        ids.push(
            repository
                .create_space(
                    name,
                    "terminal",
                    [1, 2, 3],
                    false,
                    SpaceMuxOverride { remote },
                )?
                .unwrap()
                .id(),
        );
    }
    let workspace = bootty_mux::workspace::WorkspaceRuntime::open(
        &config,
        "main",
        AppearanceVariant::Light,
        Arc::new(|| {}),
    )?;
    anyhow::ensure!(
        workspace.session_move_is_possible(local, ids[0]),
        "moves require distinct local Spaces"
    );
    anyhow::ensure!(
        !workspace.session_move_is_possible(local, local),
        "moves require distinct local Spaces"
    );
    anyhow::ensure!(
        !workspace.session_move_is_possible(local, ids[1]),
        "moves require distinct local Spaces"
    );
    anyhow::ensure!(
        !workspace.session_move_is_possible(ids[1], local),
        "moves require distinct local Spaces"
    );
    anyhow::ensure!(
        !workspace.session_move_is_possible(ids[1], ids[2]),
        "moves require distinct local Spaces"
    );
    Ok(())
}
