use anyhow::Result;
use bootty_config::config::{AppearanceVariant, BoottyConfig};
use bootty_mux::{
    controller::SpaceId,
    repository::SpaceMuxOverride,
    workspace::{NewSpace, WorkspaceRuntime},
};
use bootty_terminal::{frame_source::TerminalFrameSource, geometry::TerminalGeometry};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::sync::Arc;

#[rstest::rstest]
fn idle_terminal_reuses_its_published_frame() -> Result<()> {
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )?;

    let first = terminal.extract_frame()?;
    let second = terminal.extract_frame()?;

    anyhow::ensure!(Arc::ptr_eq(&first, &second));
    Ok(())
}

#[rstest]
fn selecting_an_inactive_binding_target_preserves_the_active_space_and_its_selection() -> Result<()>
{
    let directory = assert_fs::TempDir::new()?;
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let variant = AppearanceVariant::Light;
    bootty_test_support::native_mux::local(&config.config_path).unwrap();
    let mut workspace = WorkspaceRuntime::open(&config, "main", variant, Arc::clone(&repaint))?;
    let parent_scope = workspace.active_space_id();
    workspace.activate_target(parent_scope, "parent", None, &repaint)?;
    let captured_scope = workspace
        .create_space(
            NewSpace {
                name: "Captured",
                icon: "folder",
                color: [0, 0, 0],
                tint_sidebar: false,
                mux: SpaceMuxOverride::default(),
            },
            &config,
            variant,
        )?
        .expect("valid captured Space");
    workspace.activate_target(captured_scope, "child", None, &repaint)?;
    assert_eq!(workspace.active_space_id(), parent_scope);
    assert_eq!(
        workspace.active.binding.mux().selected_session(),
        Some("parent")
    );
    assert_eq!(
        workspace
            .binding(captured_scope)
            .expect("captured Binding")
            .mux()
            .selected_session(),
        Some("child")
    );
    anyhow::ensure!(
        workspace
            .activate_target(
                SpaceId::from_persistence(i64::MAX),
                "foreign",
                None,
                &repaint
            )
            .is_err(),
        "a closed Binding target must fail"
    );
    assert_eq!(workspace.active_space_id(), parent_scope);
    assert_eq!(
        workspace.active.binding.mux().selected_session(),
        Some("parent")
    );
    assert_eq!(
        workspace
            .binding(captured_scope)
            .expect("captured Binding")
            .mux()
            .selected_session(),
        Some("child")
    );
    Ok(())
}

#[rstest]
fn remote_native_placement_reports_its_typed_error_without_starting_a_shell() -> Result<()> {
    let config = bootty_mux::MuxBindingConfig {
        remote: Some(
            bootty_config::config::WslRemoteConfig {
                distribution: bootty_config::config::WslDistribution::new("devbox").unwrap(),
            }
            .into(),
        ),
        ..Default::default()
    };
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )?;
    let error = terminal.sync_mux_anchor(&config, None).unwrap_err();
    assert_eq!(
        error.downcast_ref::<bootty_mux::workspace::RemoteSpaceUnavailable>(),
        Some(&bootty_mux::workspace::RemoteSpaceUnavailable {
            host: "devbox".to_owned()
        })
    );
    anyhow::ensure!(terminal.focused_pane_id().is_none());
    Ok(())
}

#[rstest]
fn failed_membership_write_preserves_native_topology_and_saved_work() {
    use bootty_mux::{
        controller::CommandSelection, native::NativeBackend, repository::WorkspaceRepository,
    };
    let directory = assert_fs::TempDir::new().unwrap();
    let config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..Default::default()
    };
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    bootty_test_support::native_mux::local(&config.config_path).unwrap();
    let mut workspace =
        WorkspaceRuntime::open(&config, "main", AppearanceVariant::Light, repaint.clone()).unwrap();
    let scope = workspace.active_space_id();
    bootty_test_support::native_mux::local(&config.config_path).unwrap();
    let native = NativeBackend::for_workspace(&config.config_path);
    let before = native.snapshot().unwrap();
    let (command, membership) = workspace
        .begin_session_create(
            scope,
            "new-session",
            directory.path().to_str().unwrap(),
            Vec::new(),
        )
        .unwrap();
    let connection =
        rusqlite::Connection::open(config.config_path.with_file_name("session-order.sqlite3"))
            .unwrap();
    connection.execute_batch("CREATE TRIGGER fixture_write_failure BEFORE INSERT ON workspace_sessions BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
    let pending = bootty_mux::executor::submit_authoritative_command_for_scope(
        &mut workspace,
        &repaint,
        scope,
        command,
        membership.map(Box::new),
        None,
        CommandSelection::Preserve,
    )
    .unwrap();
    let result = pending.result.recv().unwrap();
    assert!(
        matches!(result, Err(bootty_mux::controller::MuxCommandError::Failed(ref message)) if message.contains("fixture write failure")),
        "{result:?}"
    );
    assert_eq!(native.snapshot().unwrap(), before);
    assert!(workspace.binding(scope).unwrap().sessions().is_empty());
    let (_, saved) = WorkspaceRepository::open(&config.config_path).unwrap();
    assert!(saved.spaces()[0].binding().sessions().is_empty());
}
