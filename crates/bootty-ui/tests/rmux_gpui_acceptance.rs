#![cfg(test)]
#![cfg(unix)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    process::{Command, Output},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail, ensure};
use bootty_config::ApplicationIdentity;
use bootty_host::ssh::{REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION, SshRemote};
use bootty_mux::rmux::{RemoteRmuxRequest, RmuxBackend, RmuxPanePolicy, endpoint_path_for};
use bootty_mux::{
    SshTarget,
    command::MuxCommand,
    snapshot::{MuxSessionTag, new_session_identity},
    terminal::{
        BackendPanePolicy, MuxPaneTarget, PaneLayoutResizeRequest, PaneStartRequest,
        ScopedMuxPaneTarget, TerminalRuntime,
    },
};
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface};
use bootty_terminal::terminal_frame::RenderFrame;
use bootty_terminal::terminal_session::TerminalSessionConfig;
use bootty_ui::{
    gpui::{GpuiTerminalAdapter, TerminalRenderMetrics},
    paint_plan::CursorBlinkPhase,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use gpui_kit::{TestAppContext, point, px, size};
use libghostty_vt::{render::Dirty, style::RgbColor};
use pretty_assertions::assert_eq;
use rstest::rstest;
use tokio::runtime::{Builder, Runtime};

const CHILD_ENV: &str = "BOOTTY_RMUX_GPUI_ACCEPTANCE_CHILD";
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const COLOR_MARKER: &str = "BOOTTY_RMUX_COLOR";
const READY_MARKER: &str = "BOOTTY_RMUX_READY";
const SIZE_MARKER: &str = "BOOTTY_RMUX_SIZE=";
const INPUT_MARKER: &str = "BOOTTY_RMUX_INPUT=accepted";
const SECOND_INPUT_MARKER: &str = "BOOTTY_RMUX_SECOND=again";
const REMOTE_COLOR_MARKER: &str = "BOOTTY_RMUX_REMOTE";
const REMOTE_INPUT_MARKER: &str = "BOOTTY_RMUX_REMOTE_INPUT=accepted";
const REMOTE_SIZE_MARKER: &str = "BOOTTY_RMUX_REMOTE_SIZE=30 100";

struct EmbeddedDaemon {
    runtime: Runtime,
    handle: Option<rmux_server::ServerHandle>,
}

impl EmbeddedDaemon {
    fn start() -> Result<Self> {
        let endpoint = endpoint_path_for(ApplicationIdentity::Production)?;
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .thread_name("bootty-rmux-gpui-acceptance")
            .build()
            .context("create embedded rmux runtime")?;
        let handle = runtime
            .block_on(
                rmux_server::ServerDaemon::new(rmux_server::DaemonConfig::new(endpoint)).bind(),
            )
            .context("bind isolated embedded rmux daemon")?;
        Ok(Self {
            runtime,
            handle: Some(handle),
        })
    }

    fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        self.runtime
            .block_on(handle.shutdown())
            .context("shut down isolated embedded rmux daemon")
    }
}

impl Drop for EmbeddedDaemon {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

#[rstest]
fn rmux_policy_publishes_real_frames_through_gpui() -> Result<()> {
    if env::var_os(CHILD_ENV).is_some() {
        return run_child_acceptance();
    }

    let directory = assert_fs::TempDir::new_in("/tmp").context("create isolated rmux root")?;
    let output = Command::new(env::current_exe().context("resolve acceptance test executable")?)
        .args([
            "--exact",
            "rmux_policy_publishes_real_frames_through_gpui",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("RMUX_TMPDIR", directory.path())
        .env("BOOTTY_APPLICATION_IDENTITY", "bootty")
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
        .env("ENV", "")
        .output()
        .context("run isolated rmux acceptance child")?;

    assert_child_succeeded(&output)
}

#[rstest]
fn remote_rmux_proxy_publishes_real_frames_through_gpui() -> Result<()> {
    let directory = assert_fs::TempDir::new_in("/tmp").context("create remote rmux root")?;
    let transport = directory.path().join("ssh");
    let argv_log = directory.path().join("argv.log");
    let input_log = directory.path().join("input.log");
    let input_ready = directory.path().join("input-ready");
    let resize_ready = directory.path().join("resize-ready");
    let remote = SshRemote::new(SshTarget {
        host: "remote.test".to_owned(),
        user: Some("bootty".to_owned()),
        port: None,
        program: transport.display().to_string(),
        args: Vec::new(),
    });
    let session = "remote-session";
    let pane = "remote-pane";
    let stream_line = remote_rmux_line(
        &remote,
        &RemoteRmuxRequest::PaneStream {
            session: session.to_owned(),
            pane: pane.to_owned(),
        },
    )?;
    let input_line = remote_rmux_line(
        &remote,
        &RemoteRmuxRequest::PaneInput {
            session: session.to_owned(),
            pane: pane.to_owned(),
        },
    )?;
    write_remote_transport(
        &transport,
        &argv_log,
        &input_log,
        &input_ready,
        &resize_ready,
        &stream_line,
        &input_line,
    )?;

    let target = ScopedMuxPaneTarget::from(MuxPaneTarget::Pane {
        session_id: session.to_owned(),
        pane_id: pane.to_owned(),
        cwd: None,
    });
    let (repaint_tx, repaint_rx) = mpsc::channel();
    let repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(move || {
        let _ = repaint_tx.send(());
    });
    let initial_geometry = geometry(80, 24);
    let resized_geometry = geometry(100, 30);
    let config = TerminalSessionConfig::default();
    let mut policy = RmuxPanePolicy::new(Some(remote.into()));
    let mut terminal = policy
        .start_terminal(PaneStartRequest {
            target: &target,
            geometry: initial_geometry,
            spawn_geometry: initial_geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(10.0, 20.0),
            terminal_config: &config,
            repaint_wakeup: &repaint_wakeup,
        })?
        .context("remote rmux policy did not start a pane terminal")?;

    let initial = wait_for_frame(&mut *terminal, &repaint_rx, REMOTE_COLOR_MARKER)?;
    let initial_rows = initial.text_rows();
    ensure!(
        initial_rows
            .iter()
            .any(|row| row.contains("BOOTTY_RMUX_REMOTE π🥟")),
        "remote rmux stream lost deterministic Unicode text: {initial_rows:?}"
    );
    ensure!(initial.cells.iter().any(|cell| {
        cell.fg
            == Some(RgbColor {
                r: 12,
                g: 34,
                b: 56,
            })
    }));

    terminal.resize(resized_geometry)?;
    terminal.write_input(b"accepted\n")?;
    let final_frame = wait_for_frame(&mut *terminal, &repaint_rx, REMOTE_SIZE_MARKER)?;
    let final_rows = final_frame.text_rows();
    ensure!(
        final_rows
            .iter()
            .any(|row| row.contains(REMOTE_INPUT_MARKER)),
        "remote rmux input marker was not rendered: {final_rows:?}"
    );
    ensure!(
        final_rows
            .iter()
            .any(|row| row.contains(REMOTE_SIZE_MARKER)),
        "remote rmux size marker was not rendered: {final_rows:?}"
    );
    ensure!(
        (final_frame.cols, final_frame.rows) == (100, 30),
        "remote rmux frame has wrong dimensions: {:?}",
        (final_frame.cols, final_frame.rows)
    );
    let input = fs::read_to_string(&input_log)?;
    ensure!(
        input.trim() == "YWNjZXB0ZWQK",
        "remote rmux input was not transported byte-for-byte: {input:?}"
    );

    let argv = fs::read_to_string(&argv_log).context("read remote rmux argv")?;
    ensure!(
        argv.lines().any(|arg| arg == "bootty@remote.test"),
        "missing SSH destination: {argv}"
    );
    ensure!(
        argv.lines().any(|arg| arg.contains("remote-exec")),
        "rmux pane did not use the remote daemon protocol: {argv}"
    );

    drop(terminal);
    policy.deactivate();
    draw_remote_with_gpui(initial, final_frame);
    Ok(())
}

fn run_child_acceptance() -> Result<()> {
    ApplicationIdentity::Production
        .initialize_process()
        .context("initialize production identity")?;
    let daemon = EmbeddedDaemon::start()?;
    let session_id = format!("bootty-rmux-gpui-{}", std::process::id());
    let mut backend = RmuxBackend::new();
    backend.execute(MuxCommand::CreateProjectSession {
        session_id: session_id.clone(),
        cwd: env::temp_dir().to_string_lossy().into_owned(),
        tag: MuxSessionTag {
            identity: Some(new_session_identity()),
            space: None,
        },
    })?;

    let snapshot = backend.snapshot()?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .context("created rmux session is missing")?;
    let window = session
        .windows
        .first()
        .context("created rmux window is missing")?;
    let pane = window
        .panes
        .first()
        .context("created rmux pane is missing")?;
    let window_id = window.id.clone();
    let target = ScopedMuxPaneTarget::from(MuxPaneTarget::from(pane.clone()));
    let (repaint_tx, repaint_rx) = mpsc::channel();
    let repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(move || {
        let _ = repaint_tx.send(());
    });
    let initial_geometry = geometry(80, 24);
    let resized_geometry = geometry(100, 30);
    let config = TerminalSessionConfig::default();
    let mut policy = RmuxPanePolicy::new(None);
    policy.set_layout_window(Some(&window_id));
    let mut terminal = policy
        .start_terminal(PaneStartRequest {
            target: &target,
            geometry: initial_geometry,
            spawn_geometry: initial_geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(10.0, 20.0),
            terminal_config: &config,
            repaint_wakeup: &repaint_wakeup,
        })?
        .context("rmux policy did not start a native terminal")?;

    let script_path = std::path::Path::new(
        &env::var_os("RMUX_TMPDIR").context("isolated child has no RMUX_TMPDIR")?,
    )
    .join("gpui-acceptance.sh");
    fs::write(&script_path, child_acceptance_script())
        .context("write deterministic rmux pane script")?;
    terminal.write_input(format!("/bin/sh {}\r", script_path.display()).as_bytes())?;
    let ready = wait_for_frame(&mut *terminal, &repaint_rx, READY_MARKER)?;
    ensure!(
        (ready.cols, ready.rows) == (80, 24),
        "rmux initial frame has wrong dimensions: {:?}",
        (ready.cols, ready.rows)
    );
    assert_frame_content(&ready)?;

    let base = resize_child_terminal(
        &mut policy,
        &window_id,
        &mut *terminal,
        resized_geometry,
        &repaint_wakeup,
        &repaint_rx,
    )?;

    terminal.write_input(b"accepted\r")?;
    let updated = wait_for_frame(&mut *terminal, &repaint_rx, INPUT_MARKER)?;
    terminal.write_input(b"again\r")?;
    let final_frame = wait_for_frame(&mut *terminal, &repaint_rx, SECOND_INPUT_MARKER)?;
    assert_child_frame_progress(&base, &updated, &final_frame)?;

    draw_with_gpui(base, updated, final_frame);

    drop(terminal);
    policy.deactivate();
    ditch_child_session(&mut backend, &session_id)?;
    fs::remove_file(&script_path).context("remove deterministic rmux pane script")?;
    daemon.shutdown()
}

const fn child_acceptance_script() -> &'static str {
    concat!(
        "#!/bin/sh\n",
        "stty -echo\n",
        "printf '\\033[2J\\033[H\\033[38;2;12;34;56mBOOTTY_RMUX_COLOR π🥟\\033[0m'\n",
        "printf '\\033[3;1HBOOTTY_RMUX_READY\\n'\n",
        "IFS= read -r resized\n",
        "printf '\\033[5;1HBOOTTY_RMUX_SIZE='\n",
        "stty size\n",
        "IFS= read -r answer\n",
        "printf '\\033[10;1HBOOTTY_RMUX_INPUT=%s' \"$answer\"\n",
        "IFS= read -r second\n",
        "printf '\\033[11;1HBOOTTY_RMUX_SECOND=%s' \"$second\"\n",
    )
}

fn ditch_child_session(backend: &mut RmuxBackend, session_id: &str) -> Result<()> {
    backend.execute(MuxCommand::DitchSession {
        session_id: session_id.to_owned(),
    })?;
    ensure!(
        !backend
            .snapshot()?
            .sessions
            .iter()
            .any(|session| session.id == session_id),
        "isolated rmux session survived teardown"
    );
    Ok(())
}

fn resize_child_terminal(
    policy: &mut RmuxPanePolicy,
    window_id: &str,
    terminal: &mut dyn TerminalRuntime,
    geometry: TerminalGeometry,
    repaint_wakeup: &Arc<dyn Fn() + Send + Sync + 'static>,
    repaint_rx: &mpsc::Receiver<()>,
) -> Result<Arc<RenderFrame>> {
    wait_for_layout_resize(policy, window_id, geometry, repaint_wakeup, repaint_rx)?;
    terminal.resize(geometry)?;
    terminal.write_input(b"resized\r")?;
    let frame = wait_for_size_frame(terminal, repaint_rx)?;
    let rows = frame.text_rows();
    ensure!(
        parse_size(&rows) == Some((30, 100)),
        "rmux size marker has wrong dimensions: {rows:?}"
    );
    ensure!(
        (frame.cols, frame.rows) == (100, 30),
        "rmux resized frame has wrong dimensions: {:?}",
        (frame.cols, frame.rows)
    );
    Ok(frame)
}

fn assert_child_frame_progress(
    base: &RenderFrame,
    updated: &RenderFrame,
    final_frame: &RenderFrame,
) -> Result<()> {
    let updated_rows = updated.text_rows();
    ensure!(
        updated_rows.iter().any(|row| row.contains(INPUT_MARKER)),
        "terminal input did not reach the real rmux pane: {updated_rows:?}"
    );
    ensure!(
        (updated.cols, updated.rows) == (base.cols, base.rows),
        "rmux input frame dimensions changed: {:?} vs {:?}",
        (updated.cols, updated.rows),
        (base.cols, base.rows)
    );
    let dirty_rows = updated.row_dirty.iter().filter(|dirty| **dirty).count();
    ensure!(
        updated.dirty != Dirty::Full,
        "rmux input update repainted the full frame"
    );
    ensure!(
        dirty_rows > 0 && dirty_rows < usize::from(updated.rows),
        "rmux update was not localized: dirty={:?}, rows={:?}",
        updated.dirty,
        updated.row_dirty
    );

    let final_dirty_rows = final_frame.row_dirty.iter().filter(|dirty| **dirty).count();
    ensure!(
        final_frame.dirty != Dirty::Full,
        "rmux second input update repainted the full frame"
    );
    ensure!(
        final_dirty_rows > 0 && final_dirty_rows < usize::from(final_frame.rows),
        "rmux second update was not localized: dirty={:?}, rows={:?}",
        final_frame.dirty,
        final_frame.row_dirty
    );
    Ok(())
}

fn remote_rmux_line(remote: &SshRemote, request: &RemoteRmuxRequest) -> Result<String> {
    let (_, args) = remote.proxy_command(
        REMOTE_DAEMON_PROGRAM,
        &["remote-rmux".to_owned(), request.encode()?],
    )?;
    args.last()
        .cloned()
        .context("remote rmux command has no remote command line")
}

fn write_remote_transport(
    path: &Path,
    argv_log: &Path,
    input_log: &Path,
    input_ready: &Path,
    resize_ready: &Path,
    stream_line: &str,
    input_line: &str,
) -> Result<()> {
    // The encoded payloads identify the long-lived output and input requests. Every other proxied
    // rmux request is a one-shot resize, so the shim can acknowledge it without knowing rmux.
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "for arg in \"$@\"; do\n",
            "  printf '%s\\n' \"$arg\" >> {argv_log}\n",
            "  last=$arg\n",
            "done\n",
            "case \"$last\" in\n",
            "  *\" remote-ping\") printf '%s:%s\\n' {protocol} {version}; exit 0 ;;\n",
            "esac\n",
            "if [ \"$last\" = {stream_line} ]; then\n",
            "  printf '%s\\n' '{{\"Rebase\":\"G1syShtbSBtbMzg7MjsxMjszNDs1Nm1CT09UVFlfUk1VWF9SRU1PVEUgz4Dwn6WfG1swbQ\"}}'\n",
            "  while [ ! -f {input_ready} ]; do sleep 0.01; done\n",
            "  printf '%s\\n' '{{\"Bytes\":\"G1s1OzFIQk9PVFRZX1JNVVhfUkVNT1RFX0lOUFVUPWFjY2VwdGVk\"}}'\n",
            "  while [ ! -f {resize_ready} ]; do sleep 0.01; done\n",
            "  printf '%s\\n' '{{\"Bytes\":\"G1s2OzFIQk9PVFRZX1JNVVhfUkVNT1RFX1NJWkU9MzAgMTAw\"}}'\n",
            "  while :; do sleep 1; done\n",
            "fi\n",
            "if [ \"$last\" = {input_line} ]; then\n",
            "  if IFS= read -r input; then\n",
            "    printf '%s\\n' \"$input\" > {input_log}\n",
            "    : > {input_ready}\n",
            "  fi\n",
            "  while IFS= read -r input; do :; done\n",
            "  exit 0\n",
            "fi\n",
            ": > {resize_ready}\n",
        ),
        argv_log = shell_quote(&argv_log.to_string_lossy()),
        protocol = shell_quote(REMOTE_DAEMON_PROTOCOL_VERSION),
        version = shell_quote(env!("CARGO_PKG_VERSION")),
        stream_line = shell_quote(stream_line),
        input_line = shell_quote(input_line),
        input_log = shell_quote(&input_log.to_string_lossy()),
        input_ready = shell_quote(&input_ready.to_string_lossy()),
        resize_ready = shell_quote(&resize_ready.to_string_lossy()),
    );
    fs::write(path, script).context("write remote rmux transport")?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).context("make remote rmux transport executable")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn assert_frame_content(frame: &RenderFrame) -> Result<()> {
    let rows = frame.text_rows();
    ensure!(
        rows.iter().any(|row| row.contains("BOOTTY_RMUX_COLOR π🥟")),
        "rmux native terminal lost deterministic Unicode text: {rows:?}"
    );
    let color_row = rows
        .iter()
        .position(|row| row.contains(COLOR_MARKER))
        .context("rmux color marker has no rendered row")?;
    let expected_color = RgbColor {
        r: 12,
        g: 34,
        b: 56,
    };
    ensure!(
        frame
            .cells
            .iter()
            .any(|cell| usize::from(cell.y) == color_row && cell.fg == Some(expected_color)),
        "rmux native terminal lost the 24-bit foreground color"
    );
    Ok(())
}

fn draw_remote_with_gpui(initial: Arc<RenderFrame>, final_frame: Arc<RenderFrame>) {
    let mut cx = TestAppContext::single();
    let cx = cx.add_empty_window();
    let metrics = TerminalRenderMetrics::default();
    let mut adapter = GpuiTerminalAdapter::default();
    adapter.set_render_metrics(Some(metrics.clone()));
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());

    for frame in [initial, final_frame] {
        let surface = TerminalSurface::for_logical_size(
            f32::from(frame.cols) * 10.0,
            f32::from(frame.rows) * 20.0,
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::default(),
        );
        cx.draw(
            point(px(0.0), px(0.0)),
            size(px(surface.rect.width()), px(surface.rect.height())),
            |_, _| {
                adapter.element(
                    surface,
                    &frame,
                    14.0,
                    surface.cell.height,
                    1.0,
                    &contract,
                    CursorBlinkPhase::visible(),
                    true,
                    "",
                )
            },
        );
    }

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.scene_builds, 2);
    assert_eq!(snapshot.prepaint_calls, 2);
    assert_eq!(snapshot.paint_calls, 2);
    assert!(snapshot.glyph_primitives > 0);
}

fn draw_with_gpui(
    base: Arc<RenderFrame>,
    updated: Arc<RenderFrame>,
    final_frame: Arc<RenderFrame>,
) {
    let mut cx = TestAppContext::single();
    let cx = cx.add_empty_window();
    let metrics = TerminalRenderMetrics::default();
    let mut adapter = GpuiTerminalAdapter::default();
    adapter.set_render_metrics(Some(metrics.clone()));
    let contract =
        TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default());
    let surface = TerminalSurface::for_logical_size(
        f32::from(base.cols) * 10.0,
        f32::from(base.rows) * 20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );

    for frame in [base, updated, final_frame] {
        cx.draw(
            point(px(0.0), px(0.0)),
            size(px(surface.rect.width()), px(surface.rect.height())),
            |_, _| {
                let actual = adapter.element(
                    surface,
                    &frame,
                    14.0,
                    surface.cell.height,
                    1.0,
                    &contract,
                    CursorBlinkPhase::visible(),
                    true,
                    "",
                );
                // Observing selected PTY publications can skip the damage's base frame.
                let expected = GpuiTerminalAdapter::default().element(
                    surface,
                    &frame,
                    14.0,
                    surface.cell.height,
                    1.0,
                    &contract,
                    CursorBlinkPhase::visible(),
                    true,
                    "",
                );
                assert_eq!(actual.frame(), expected.frame());
                assert_eq!(actual.glyph_sprite_rects(), expected.glyph_sprite_rects());
                actual
            },
        );
    }

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.scene_builds, 3);
    assert_eq!(snapshot.prepaint_calls, 3);
    assert_eq!(snapshot.paint_calls, 3);
    assert!(snapshot.glyph_primitives > 0);
}

fn wait_for_layout_resize(
    policy: &mut RmuxPanePolicy,
    window_id: &str,
    geometry: TerminalGeometry,
    repaint_wakeup: &Arc<dyn Fn() + Send + Sync + 'static>,
    repaint_rx: &mpsc::Receiver<()>,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame timeout overflows instant")?;
    while Instant::now() < deadline {
        if policy.resize_layout_window(PaneLayoutResizeRequest {
            window_id: Some(window_id),
            cols: geometry.cols,
            rows: geometry.rows,
            repaint_wakeup,
        })? {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _ = repaint_rx.recv_timeout(remaining.min(Duration::from_millis(25)));
    }
    bail!(
        "rmux layout window did not resize to {}x{}",
        geometry.cols,
        geometry.rows
    )
}

fn wait_for_frame(
    terminal: &mut dyn TerminalRuntime,
    repaint_rx: &mpsc::Receiver<()>,
    marker: &str,
) -> Result<Arc<RenderFrame>> {
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame timeout overflows instant")?;
    let mut last_rows = Vec::new();
    while Instant::now() < deadline {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        last_rows = frame.text_rows();
        if last_rows.iter().any(|row| row.contains(marker)) {
            return Ok(frame);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _ = repaint_rx.recv_timeout(remaining.min(Duration::from_millis(25)));
    }
    bail!("rmux native terminal did not publish {marker:?}: {last_rows:?}")
}

fn wait_for_size_frame(
    terminal: &mut dyn TerminalRuntime,
    repaint_rx: &mpsc::Receiver<()>,
) -> Result<Arc<RenderFrame>> {
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame timeout overflows instant")?;
    let mut last_rows = Vec::new();
    while Instant::now() < deadline {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        last_rows = frame.text_rows();
        if parse_size(&last_rows).is_some() {
            return Ok(frame);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _ = repaint_rx.recv_timeout(remaining.min(Duration::from_millis(25)));
    }
    bail!("rmux attach did not publish a complete size: {last_rows:?}")
}

fn parse_size(rows: &[String]) -> Option<(u16, u16)> {
    rows.iter().find_map(|row| {
        let suffix = row.split_once(SIZE_MARKER)?.1;
        let mut fields = suffix.split_whitespace();
        let rows = fields.next()?.parse().ok()?;
        let cols = fields.next()?.parse().ok()?;
        Some((rows, cols))
    })
}

const fn geometry(cols: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry {
        cols,
        rows,
        cell_width: 10,
        cell_height: 20,
    }
}

fn assert_child_succeeded(output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "isolated rmux acceptance child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
