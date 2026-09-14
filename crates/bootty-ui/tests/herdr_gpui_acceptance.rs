#![cfg(test)]
#![cfg(unix)]

use std::{
    env, fs,
    os::unix::fs::PermissionsExt as _,
    process::{Command, Output},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context as _, Result, bail};
use bootty_host::ssh::SshRemote;
use bootty_mux::SshTarget;
use bootty_mux::herdr::HerdrPanePolicy;
use bootty_mux::terminal::{
    BackendPanePolicy, MuxPaneTarget, PaneStartRequest, ScopedMuxPaneTarget, TerminalRuntime,
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
use libghostty_vt::style::RgbColor;
use pretty_assertions::assert_eq;
use rstest::rstest;

const CHILD_ENV: &str = "BOOTTY_HERDR_GPUI_ACCEPTANCE_CHILD";
const REMOTE_CHILD_ENV: &str = "BOOTTY_HERDR_REMOTE_GPUI_ACCEPTANCE_CHILD";
const REMOTE_ARGV_LOG_ENV: &str = "BOOTTY_HERDR_REMOTE_ARGV_LOG";
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const COLOR_MARKER: &str = "BOOTTY_HERDR_COLOR";
const BEFORE_MARKER: &str = "BOOTTY_HERDR_BEFORE=";
const REMOTE_COLOR_MARKER: &str = "BOOTTY_HERDR_REMOTE_COLOR";
const REMOTE_INPUT_MARKER: &str = "BOOTTY_HERDR_REMOTE_INPUT=accepted";
const REMOTE_SIZE_MARKER: &str = "BOOTTY_HERDR_REMOTE_SIZE=";

struct HerdrServerGuard {
    session: String,
}

impl Drop for HerdrServerGuard {
    fn drop(&mut self) {
        let _ = stop_session(&self.session);
    }
}

#[rstest]
fn herdr_policy_publishes_real_frames_through_gpui() -> Result<()> {
    if env::var_os(CHILD_ENV).is_some() {
        return run_child_acceptance();
    }

    if Command::new("herdr").arg("--version").output().is_err() {
        eprintln!("skipping Herdr acceptance because Herdr is not installed");
        return Ok(());
    }

    let directory = assert_fs::TempDir::new_in("/tmp").context("create isolated Herdr root")?;
    let config_home = directory.path().join("config");
    let state_home = directory.path().join("state");
    let runtime_dir = directory.path().join("run");
    let home = directory.path().join("home");
    fs::create_dir_all(config_home.join("herdr"))?;
    fs::create_dir_all(&state_home)?;
    fs::create_dir_all(&runtime_dir)?;
    fs::create_dir_all(&home)?;
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
    )?;

    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .context("read wall clock")?
        .as_nanos();
    let session = format!("bootty-e2e-{}-{nonce:x}", std::process::id());
    let output = Command::new(env::current_exe().context("resolve acceptance test executable")?)
        .args([
            "--exact",
            "herdr_policy_publishes_real_frames_through_gpui",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HOME", &home)
        .env("SHELL", "/bin/sh")
        .env("HERDR_DISABLE_SOUND", "1")
        .env("BOOTTY_HERDR_ACCEPTANCE_SESSION", &session)
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .output()
        .context("run isolated Herdr acceptance child")?;

    let cleanup =
        stop_session_with_environment(&session, &config_home, &state_home, &runtime_dir, &home);
    assert_child_succeeded(&output)?;
    cleanup.context("clean up isolated Herdr session")?;
    Ok(())
}

#[rstest]
fn remote_herdr_launch_publishes_real_frames_through_gpui() -> Result<()> {
    if env::var_os(REMOTE_CHILD_ENV).is_some() {
        return run_remote_child_acceptance();
    }

    let directory = assert_fs::TempDir::new_in("/tmp").context("create remote Herdr root")?;
    let transport = directory.path().join("herdr");
    let argv_log = directory.path().join("argv.log");
    write_remote_herdr_transport(&transport)?;
    let path = env::join_paths([
        directory.path(),
        std::path::Path::new("/usr/bin"),
        std::path::Path::new("/bin"),
    ])
    .context("build isolated remote Herdr PATH")?;
    let output = Command::new(env::current_exe().context("resolve acceptance test executable")?)
        .args([
            "--exact",
            "remote_herdr_launch_publishes_real_frames_through_gpui",
            "--nocapture",
        ])
        .env(REMOTE_CHILD_ENV, "1")
        .env(REMOTE_ARGV_LOG_ENV, &argv_log)
        .env("PATH", path)
        .env("SHELL", "/bin/sh")
        .env("ENV", "")
        .output()
        .context("run isolated remote Herdr acceptance child")?;

    assert_child_succeeded(&output)?;
    let argv = fs::read_to_string(argv_log).context("read remote Herdr argv")?;
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        [
            "--remote",
            "bootty@remote.test",
            "--session",
            "remote-session"
        ]
    );
    Ok(())
}

fn run_remote_child_acceptance() -> Result<()> {
    let remote = SshRemote::new(SshTarget {
        host: "remote.test".to_owned(),
        user: Some("bootty".to_owned()),
        port: None,
        program: "ssh".to_owned(),
        args: Vec::new(),
    });
    let target = ScopedMuxPaneTarget::from(MuxPaneTarget::Session {
        session_id: "remote-session".to_owned(),
        cwd: None,
    });
    let (repaint_tx, repaint_rx) = mpsc::channel();
    let repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(move || {
        let _ = repaint_tx.send(());
    });
    let initial_geometry = geometry(80, 24);
    let resized_geometry = geometry(100, 30);
    let config = TerminalSessionConfig::default();
    let mut policy = HerdrPanePolicy::new(Some(remote.into()));
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
        .context("remote Herdr policy did not start an attach terminal")?;

    let initial = wait_until(&mut *terminal, &repaint_rx, |frame| {
        frame
            .text_rows()
            .iter()
            .any(|row| row.contains(REMOTE_COLOR_MARKER))
    })?;
    let initial_rows = initial.text_rows();
    anyhow::ensure!(
        initial_rows
            .iter()
            .any(|row| row.contains("BOOTTY_HERDR_REMOTE_COLOR π🥟")),
        "remote Herdr attach lost deterministic Unicode text: {initial_rows:?}"
    );
    anyhow::ensure!(
        initial.cells.iter().any(|cell| {
            cell.fg
                == Some(RgbColor {
                    r: 12,
                    g: 34,
                    b: 56,
                })
        }),
        "remote Herdr attach lost the 24-bit foreground color"
    );

    terminal.write_input(b"accepted\n")?;
    wait_until(&mut *terminal, &repaint_rx, |frame| {
        frame
            .text_rows()
            .iter()
            .any(|row| row.contains(REMOTE_INPUT_MARKER))
    })?;
    terminal.resize(resized_geometry)?;
    terminal.write_input(b"resized\n")?;
    let (final_frame, size) = wait_for_size_frame(&mut *terminal, &repaint_rx, REMOTE_SIZE_MARKER)?;
    anyhow::ensure!(size == (30, 100), "remote shell size: {size:?}");
    anyhow::ensure!(
        (final_frame.cols, final_frame.rows) == (100, 30),
        "terminal frame size: {}x{}",
        final_frame.cols,
        final_frame.rows
    );

    drop(terminal);
    policy.deactivate();
    draw_with_gpui(initial, final_frame);
    Ok(())
}

fn write_remote_herdr_transport(path: &std::path::Path) -> Result<()> {
    fs::write(
        path,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' \"$@\" > \"$BOOTTY_HERDR_REMOTE_ARGV_LOG\"\n",
            "stty -echo\n",
            "printf '\\033[2J\\033[H\\033[38;2;12;34;56mBOOTTY_HERDR_REMOTE_COLOR π🥟\\033[0m'\n",
            "IFS= read -r input\n",
            "printf '\\033[5;1HBOOTTY_HERDR_REMOTE_INPUT=%s' \"$input\"\n",
            "IFS= read -r resized\n",
            "printf '\\033[6;1HBOOTTY_HERDR_REMOTE_SIZE='\n",
            "stty size\n",
            "while :; do sleep 1; done\n",
        ),
    )
    .context("write remote Herdr transport")?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).context("make remote Herdr transport executable")
}

fn run_child_acceptance() -> Result<()> {
    let session = env::var("BOOTTY_HERDR_ACCEPTANCE_SESSION")
        .context("child has no isolated Herdr session")?;
    let _server = HerdrServerGuard {
        session: session.clone(),
    };
    let target = ScopedMuxPaneTarget::from(MuxPaneTarget::Session {
        session_id: session,
        cwd: None,
    });
    let (repaint_tx, repaint_rx) = mpsc::channel();
    let repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(move || {
        let _ = repaint_tx.send(());
    });
    let initial_geometry = geometry(80, 24);
    let resized_geometry = geometry(100, 30);
    let config = TerminalSessionConfig::default();
    let mut policy = HerdrPanePolicy::new(None);
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
        .context("Herdr policy did not start an attach terminal")?;

    wait_for_nonempty_frame(&mut *terminal, &repaint_rx)?;
    // A first frame can precede Herdr applying the client's initial PTY size.
    wait_for_shell_size(&mut *terminal, &repaint_rx, "READY", |(rows, cols)| {
        rows > 0 && rows < initial_geometry.rows && cols > 0 && cols < initial_geometry.cols
    })?;
    terminal.write_input(
        concat!(
            "printf '\\033[38;2;12;34;56mBOOTTY_HERDR_COLOR π🥟\\033[0m\\n'; ",
            "printf 'BOOTTY_HERDR_BEFORE=%s\\n' \"$(stty size)\"\n",
        )
        .as_bytes(),
    )?;
    let (initial, before) = wait_for_size_frame(&mut *terminal, &repaint_rx, BEFORE_MARKER)?;
    assert_herdr_frame_content(&initial)?;
    terminal.resize(resized_geometry)?;
    let expected = resized_shell_size(before, initial_geometry, resized_geometry)?;
    let (final_frame, after) =
        wait_for_shell_size(&mut *terminal, &repaint_rx, "GROW", |size| size == expected)?;
    anyhow::ensure!(
        after == expected,
        "shell size: {after:?}; expected {expected:?}"
    );
    anyhow::ensure!(
        (final_frame.cols, final_frame.rows) == (100, 30),
        "terminal frame size: {}x{}",
        final_frame.cols,
        final_frame.rows
    );

    terminal.resize(initial_geometry)?;
    wait_for_shell_size(&mut *terminal, &repaint_rx, "SHRINK", |size| size == before)?;

    drop(terminal);
    policy.deactivate();
    draw_with_gpui(initial, final_frame);
    Ok(())
}

fn resized_shell_size(
    before: (u16, u16),
    initial: TerminalGeometry,
    resized: TerminalGeometry,
) -> Result<(u16, u16)> {
    Ok((
        before
            .0
            .checked_add(
                resized
                    .rows
                    .checked_sub(initial.rows)
                    .context("row growth")?,
            )
            .context("resized rows fit")?,
        before
            .1
            .checked_add(
                resized
                    .cols
                    .checked_sub(initial.cols)
                    .context("column growth")?,
            )
            .context("resized columns fit")?,
    ))
}

fn assert_herdr_frame_content(frame: &RenderFrame) -> Result<()> {
    let rows = frame.text_rows();
    anyhow::ensure!(
        rows.iter()
            .any(|row| row.contains("BOOTTY_HERDR_COLOR π🥟")),
        "Herdr attach lost deterministic Unicode text: {rows:?}"
    );
    let expected_color = RgbColor {
        r: 12,
        g: 34,
        b: 56,
    };
    anyhow::ensure!(
        // The marker may also appear in echoed input, and Herdr can prefix rows with chrome.
        rows.iter().enumerate().any(|(y, row)| {
            row.contains(COLOR_MARKER)
                && frame
                    .cells
                    .iter()
                    .any(|cell| usize::from(cell.y) == y && cell.fg == Some(expected_color))
        }),
        "Herdr attach lost the 24-bit foreground color: {rows:?}"
    );
    Ok(())
}

fn draw_with_gpui(initial: Arc<RenderFrame>, final_frame: Arc<RenderFrame>) {
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

fn wait_for_nonempty_frame(
    terminal: &mut dyn TerminalRuntime,
    repaint_rx: &mpsc::Receiver<()>,
) -> Result<Arc<RenderFrame>> {
    wait_until(terminal, repaint_rx, |frame| !frame.cells.is_empty())
}

fn wait_for_size_frame(
    terminal: &mut dyn TerminalRuntime,
    repaint_rx: &mpsc::Receiver<()>,
    marker: &str,
) -> Result<(Arc<RenderFrame>, (u16, u16))> {
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame deadline overflow")?;
    let mut last_rows = Vec::new();
    while Instant::now() < deadline {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        last_rows = frame.text_rows();
        if let Some(size) = parse_size(&last_rows, marker) {
            return Ok((frame, size));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _ = repaint_rx.recv_timeout(remaining.min(Duration::from_millis(25)));
    }
    bail!("Herdr attach did not publish a size after {marker:?}: {last_rows:?}")
}

fn wait_until(
    terminal: &mut dyn TerminalRuntime,
    repaint_rx: &mpsc::Receiver<()>,
    mut ready: impl FnMut(&RenderFrame) -> bool,
) -> Result<Arc<RenderFrame>> {
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame deadline overflow")?;
    while Instant::now() < deadline {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        if ready(&frame) {
            return Ok(frame);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _ = repaint_rx.recv_timeout(remaining.min(Duration::from_millis(25)));
    }
    bail!("timed out waiting for a Herdr frame")
}

fn wait_for_shell_size(
    terminal: &mut dyn TerminalRuntime,
    repaint_rx: &mpsc::Receiver<()>,
    phase: &str,
    ready: impl Fn((u16, u16)) -> bool,
) -> Result<(Arc<RenderFrame>, (u16, u16))> {
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame deadline overflow")?;
    let mut sequence = 0_u64;
    let mut observed = Vec::new();
    let mut last_rows = Vec::new();
    let mut marker = format!("{phase}_{sequence}=");
    terminal.write_input(format!("printf '{marker}%s\\n' \"$(stty size)\"\n").as_bytes())?;
    while Instant::now() < deadline {
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        last_rows = frame.text_rows();
        if let Some(size) = parse_size(&last_rows, &marker) {
            if observed.last() != Some(&size) {
                observed.push(size);
            }
            if ready(size) {
                eprintln!("Herdr {phase} shell sizes: {observed:?}");
                return Ok((frame, size));
            }
            sequence = sequence.checked_add(1).context("shell sequence overflow")?;
            marker = format!("{phase}_{sequence}=");
            terminal
                .write_input(format!("printf '{marker}%s\\n' \"$(stty size)\"\n").as_bytes())?;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _ = repaint_rx.recv_timeout(remaining.min(Duration::from_millis(25)));
    }
    bail!(
        "Herdr {phase} shell size did not converge: {observed:?}; pending={marker}; frame={last_rows:?}"
    )
}

fn parse_size(rows: &[String], marker: &str) -> Option<(u16, u16)> {
    rows.iter().find_map(|row| {
        let suffix = row.split_once(marker)?.1;
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
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return Ok(());
    }
    bail!(
        "isolated Herdr acceptance child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn stop_session(session: &str) -> Result<()> {
    let output = Command::new("herdr")
        .args(["session", "stop", session])
        .output()
        .context("invoke Herdr session stop")?;
    accept_stop_output(&output)
}

fn stop_session_with_environment(
    session: &str,
    config_home: &std::path::Path,
    state_home: &std::path::Path,
    runtime_dir: &std::path::Path,
    home: &std::path::Path,
) -> Result<()> {
    let output = Command::new("herdr")
        .args(["session", "stop", session])
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", state_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", home)
        .env("SHELL", "/bin/sh")
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .output()
        .context("invoke isolated Herdr session stop")?;
    accept_stop_output(&output)
}

fn accept_stop_output(output: &Output) -> Result<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success()
        || stderr.contains("is not running")
        || stderr.contains("cannot be reached")
    {
        Ok(())
    } else {
        bail!("stop isolated Herdr session: {}", stderr.trim())
    }
}
