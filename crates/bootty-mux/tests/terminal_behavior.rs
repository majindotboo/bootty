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

/// A pane that prints `text` once and then stays open, so a frame names the server it ran on.
struct Announcing {
    text: Option<Vec<u8>>,
    closed: std::sync::mpsc::Receiver<()>,
}
impl std::io::Read for Announcing {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if let Some(text) = self.text.take() {
            let length = text.len().min(bytes.len());
            bytes[..length].copy_from_slice(&text[..length]);
            return Ok(length);
        }
        let _ = self.closed.recv();
        Ok(0)
    }
}
struct Closer(std::sync::mpsc::Sender<()>);
impl bootty_terminal::terminal_transport::TerminalControl for Closer {
    fn close(&self) {
        let _ = self.0.send(());
    }
    fn resize(&self, _: bootty_terminal::terminal_transport::TerminalSize) -> Result<()> {
        Ok(())
    }
    fn exited(&self) -> Result<bool> {
        Ok(false)
    }
}

/// An in-process server whose panes announce `name`, with one session open in it.
fn announcing_server(
    path: &std::path::Path,
    name: &'static str,
) -> Result<(
    bootty_mux::native::NativeBackend,
    bootty_mux::snapshot::MuxPaneAnchor,
    String,
)> {
    bootty_test_support::native_mux::configure(
        path,
        Arc::new(move |_, _| {
            let (tx, rx) = std::sync::mpsc::channel();
            Ok(bootty_terminal::terminal_transport::TerminalTransport {
                reader: bootty_terminal::terminal_output::TerminalReader::Bytes(Box::new(
                    Announcing {
                        text: Some(format!("{name}\r\n").into_bytes()),
                        closed: rx,
                    },
                )),
                writer: Box::new(std::io::sink()),
                control: Arc::new(Closer(tx)),
                tty_name: None,
            })
        }),
    )?;
    let backend = bootty_mux::native::NativeBackend::for_workspace(path);
    backend.execute(bootty_mux::command::MuxCommand::CreateProjectSession {
        session_id: name.into(),
        cwd: path.to_string_lossy().into_owned(),
        tag: bootty_mux::snapshot::MuxSessionTag::default(),
        argv: None,
    })?;
    let session = backend
        .snapshot()?
        .sessions
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{name} has no session"))?;
    let window = session
        .active_window_id
        .ok_or_else(|| anyhow::anyhow!("{name} has no window"))?;
    Ok((backend, session.anchor, window))
}

fn wait_for_text(
    terminal: &mut bootty_mux::terminal::ActiveTerminal,
    repaints: &std::sync::mpsc::Receiver<()>,
    text: &str,
) -> Result<()> {
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(5))
        .ok_or_else(|| anyhow::anyhow!("deadline overflow"))?;
    loop {
        let rows = terminal.extract_frame()?.text_rows();
        if rows.iter().any(|row| row.contains(text)) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        anyhow::ensure!(
            repaints.recv_timeout(remaining).is_ok(),
            "the focused pane never showed {text:?}: {rows:?}"
        );
    }
}

#[rstest]
fn each_space_shows_its_panes_from_its_own_server() -> Result<()> {
    let (first, second) = (assert_fs::TempDir::new()?, assert_fs::TempDir::new()?);
    let (local, local_pane, local_window) = announcing_server(first.path(), "server-a")?;
    let (remote, remote_pane, remote_window) = announcing_server(second.path(), "server-b")?;
    let (repaint, repaints) = std::sync::mpsc::channel();
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(move || {
            let _ = repaint.send(());
        }),
    )?;

    // The workspace keeps one terminal owner and hands it to whichever Space is active, so each
    // Space's panes must start on that Space's server.
    terminal.sync_scoped_native_window(
        SpaceId::from_persistence(1),
        &local,
        std::slice::from_ref(&local_pane),
        Some(&local_pane),
        Some(&local_window),
    );
    wait_for_text(&mut terminal, &repaints, "server-a")?;
    terminal.sync_scoped_native_window(
        SpaceId::from_persistence(2),
        &remote,
        std::slice::from_ref(&remote_pane),
        Some(&remote_pane),
        Some(&remote_window),
    );
    wait_for_text(&mut terminal, &repaints, "server-b")
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
    let error = terminal
        .sync_mux_anchor(&bootty_mux::native::NativeBackend::new(), &config, None)
        .unwrap_err();
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
