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
use bootty_config::{ApplicationIdentity, DEVELOPMENT_NAMESPACE_ENV};
use bootty_host::ssh::SshRemote;
use bootty_mux::SshTarget;
use bootty_mux::terminal::{
    BackendPanePolicy, MuxPaneTarget, PaneStartRequest, ScopedMuxPaneTarget, TerminalRuntime,
};
use bootty_mux::tmux::{TmuxPanePolicy, local_server_args};
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

const CHILD_ENV: &str = "BOOTTY_TMUX_GPUI_ACCEPTANCE_CHILD";
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const START_MARKER: &str = "BOOTTY_TMUX_COLOR";
const INPUT_MARKER: &str = "BOOTTY_TMUX_INPUT=accepted";
const SIZE_MARKER: &str = "BOOTTY_TMUX_SIZE=30 100";
const REMOTE_START_MARKER: &str = "BOOTTY_REMOTE_TMUX_COLOR";
const REMOTE_INPUT_MARKER: &str = "BOOTTY_REMOTE_TMUX_INPUT=accepted";
const REMOTE_SIZE_MARKER: &str = "BOOTTY_REMOTE_TMUX_SIZE=30 100";

struct TmuxServerGuard {
    namespace: String,
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        let _ = kill_server(&self.namespace);
    }
}

#[rstest]
fn tmux_policy_publishes_real_frames_through_gpui() -> Result<()> {
    if env::var_os(CHILD_ENV).is_some() {
        return run_child_acceptance();
    }

    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("skipping tmux acceptance because tmux is not installed");
        return Ok(());
    }

    let directory = assert_fs::TempDir::new_in("/tmp").context("create isolated TMUX_TMPDIR")?;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .context("read wall clock")?
        .as_nanos();
    let discriminator = u64::try_from(nonce).context("test timestamp fits the namespace")?
        ^ u64::from(std::process::id());
    let namespace = format!("bootty-dev-{discriminator:016x}");
    let output = Command::new(env::current_exe().context("resolve acceptance test executable")?)
        .args([
            "--exact",
            "tmux_policy_publishes_real_frames_through_gpui",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("TMUX_TMPDIR", directory.path())
        .env(DEVELOPMENT_NAMESPACE_ENV, &namespace)
        .output()
        .context("run isolated tmux acceptance child")?;

    let cleanup = kill_server_with_tmpdir(&namespace, directory.path());
    assert_child_succeeded(&output)?;
    cleanup.context("clean up isolated tmux server")?;
    Ok(())
}

#[rstest]
fn remote_tmux_proxy_publishes_real_frames_through_gpui() -> Result<()> {
    let directory = assert_fs::TempDir::new_in("/tmp").context("create remote transport root")?;
    let argv_log = directory.path().join("argv.log");
    let transport = directory.path().join("ssh");
    write_remote_transport(&transport, &argv_log)?;

    let remote = SshRemote::new(SshTarget {
        host: "remote.test".to_owned(),
        user: Some("bootty".to_owned()),
        port: None,
        program: transport.display().to_string(),
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
    let mut policy = TmuxPanePolicy::new(Some(remote.into()));
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
        .context("remote tmux policy did not start an attach terminal")?;

    let initial = wait_for_frame(&mut *terminal, &repaint_rx, REMOTE_START_MARKER)?;
    anyhow::ensure!(
        initial
            .text_rows()
            .iter()
            .any(|row| row.contains("BOOTTY_REMOTE_TMUX_COLOR π🥟")),
        "remote transport lost deterministic Unicode text"
    );
    anyhow::ensure!(initial.cells.iter().any(|cell| {
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
    anyhow::ensure!(
        final_rows
            .iter()
            .any(|row| row.contains(REMOTE_INPUT_MARKER))
    );
    anyhow::ensure!(
        final_rows
            .iter()
            .any(|row| row.contains(REMOTE_SIZE_MARKER))
    );
    assert_eq!((final_frame.cols, final_frame.rows), (100, 30));

    let argv = fs::read_to_string(&argv_log).context("read remote transport argv")?;
    anyhow::ensure!(
        argv.lines().any(|arg| arg == "-t"),
        "missing SSH PTY flag: {argv}"
    );
    anyhow::ensure!(
        argv.lines().any(|arg| arg == "bootty@remote.test"),
        "missing SSH destination: {argv}"
    );
    anyhow::ensure!(
        argv.lines().any(|arg| arg.contains("remote-exec")),
        "attach did not use the remote daemon protocol: {argv}"
    );

    drop(terminal);
    policy.deactivate();
    draw_with_gpui(initial, final_frame);
    Ok(())
}

fn run_child_acceptance() -> Result<()> {
    ApplicationIdentity::Development
        .initialize_process()
        .context("initialize development identity")?;
    let namespace = ApplicationIdentity::for_process().namespace().to_owned();
    let _server = TmuxServerGuard { namespace };
    let backend = bootty_mux::tmux::TmuxBackend::for_identity(ApplicationIdentity::Development);
    let snapshot = backend
        .snapshot()
        .context("read topology before the first tmux session")?;
    anyhow::ensure!(
        snapshot.sessions.is_empty(),
        "expected an empty tmux server: {:?}",
        snapshot.sessions
    );
    let output = Command::new("tmux")
        .args(local_server_args(ApplicationIdentity::Development))
        // The isolated server must not load the host's tmux.conf: its hooks can spawn
        // daemons or wedge the client, which would test the machine instead of Bootty.
        .args(["-f", "/dev/null"])
        .args(["start-server", ";", "set-option", "-g", "exit-empty", "off"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot = backend
        .snapshot()
        .context("read running server without sessions")?;
    anyhow::ensure!(
        snapshot.sessions.is_empty(),
        "expected an empty tmux server: {:?}",
        snapshot.sessions
    );
    let session = format!("bootty-e2e-{}", std::process::id());
    let script = write_pane_script()?;
    start_server(&session, &script)?;

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
    let mut policy = TmuxPanePolicy::new(None);
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
        .context("tmux policy did not start an attach terminal")?;
    policy.sync_target(Some(&target), true);

    let initial = wait_for_frame(&mut *terminal, &repaint_rx, START_MARKER)?;
    let initial_rows = initial.text_rows();
    anyhow::ensure!(
        initial_rows
            .iter()
            .any(|row| row.contains("BOOTTY_TMUX_COLOR π🥟")),
        "tmux attach lost deterministic Unicode text: {initial_rows:?}"
    );
    let expected_color = RgbColor {
        r: 12,
        g: 34,
        b: 56,
    };
    anyhow::ensure!(
        initial.cells.iter().any(|cell| {
            cell.fg == Some(expected_color)
                && initial
                    .cell_text(cell)
                    .iter()
                    .any(|character| !character.is_whitespace())
        }),
        "tmux attach lost the 24-bit foreground color"
    );

    terminal.resize(resized_geometry)?;
    wait_for_pane_size(target.session_id(), 100, 30)?;
    terminal.write_input(b"accepted\n")?;
    let final_frame = wait_for_frame(&mut *terminal, &repaint_rx, SIZE_MARKER)?;
    let final_rows = final_frame.text_rows();
    anyhow::ensure!(
        final_rows.iter().any(|row| row.contains(INPUT_MARKER)),
        "terminal input did not reach the real tmux pane: {final_rows:?}"
    );
    anyhow::ensure!(
        final_rows.iter().any(|row| row.contains(SIZE_MARKER)),
        "terminal resize did not reach the real tmux pane: {final_rows:?}"
    );
    assert_eq!((final_frame.cols, final_frame.rows), (100, 30));

    drop(terminal);
    policy.deactivate();
    draw_with_gpui(initial, final_frame);
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

fn wait_for_frame(
    terminal: &mut dyn TerminalRuntime,
    repaint_rx: &mpsc::Receiver<()>,
    marker: &str,
) -> Result<Arc<RenderFrame>> {
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame deadline overflow")?;
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
    bail!("tmux attach did not publish {marker:?}: {last_rows:?}")
}

fn write_pane_script() -> Result<std::path::PathBuf> {
    let directory = env::var_os("TMUX_TMPDIR").context("child has no isolated TMUX_TMPDIR")?;
    let path = std::path::Path::new(&directory).join("pane.sh");
    fs::write(
        &path,
        concat!(
            "#!/bin/sh\n",
            "printf '\\033[38;2;12;34;56mBOOTTY_TMUX_COLOR π🥟\\033[0m\\n'\n",
            "IFS= read -r line\n",
            "printf 'BOOTTY_TMUX_INPUT=%s\\n' \"$line\"\n",
            "attempt=0\n",
            "while :; do\n",
            "  set -- $(stty size)\n",
            "  [ \"$1 $2\" = '30 100' ] && break\n",
            "  attempt=$((attempt + 1))\n",
            "  [ \"$attempt\" -ge 500 ] && break\n",
            "  sleep 0.01\n",
            "done\n",
            "printf 'BOOTTY_TMUX_SIZE=%s %s\\n' \"$1\" \"$2\"\n",
            "sleep 30\n",
        ),
    )
    .context("write tmux pane program")?;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions)?;
    Ok(path)
}

fn write_remote_transport(path: &std::path::Path, argv_log: &std::path::Path) -> Result<()> {
    fs::write(
        path,
        format!(
            concat!(
                "#!/bin/sh\n",
                "attach=false\n",
                "for arg in \"$@\"; do [ \"$arg\" = '-t' ] && attach=true; done\n",
                "$attach || exit 0\n",
                "printf '%s\\n' \"$@\" > '{}'\n",
                "printf '\\033[38;2;12;34;56mBOOTTY_REMOTE_TMUX_COLOR π🥟\\033[0m\\n'\n",
                "IFS= read -r line\n",
                "printf 'BOOTTY_REMOTE_TMUX_INPUT=%s\\n' \"$line\"\n",
                "attempt=0\n",
                "while :; do\n",
                "  set -- $(stty size)\n",
                "  [ \"$1 $2\" = '30 100' ] && break\n",
                "  attempt=$((attempt + 1))\n",
                "  [ \"$attempt\" -ge 500 ] && break\n",
                "  sleep 0.01\n",
                "done\n",
                "printf 'BOOTTY_REMOTE_TMUX_SIZE=%s %s\\n' \"$1\" \"$2\"\n",
            ),
            argv_log.display()
        ),
    )
    .context("write fake SSH transport")?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn start_server(session: &str, script: &std::path::Path) -> Result<()> {
    let output = Command::new("tmux")
        .args(local_server_args(ApplicationIdentity::for_process()))
        .args(["-f", "/dev/null"])
        .args(["new-session", "-d", "-x", "80", "-y", "24", "-s", session])
        .arg(script)
        .output()
        .context("start isolated tmux server")?;
    if !output.status.success() {
        bail!(
            "start isolated tmux server: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn wait_for_pane_size(session: &str, cols: u16, rows: u16) -> Result<()> {
    let expected = format!("{cols} {rows}");
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame deadline overflow")?;
    let mut actual = String::new();
    while Instant::now() < deadline {
        let output = Command::new("tmux")
            .args(local_server_args(ApplicationIdentity::for_process()))
            .args([
                "display-message",
                "-p",
                "-t",
                session,
                "#{pane_width} #{pane_height}",
            ])
            .output()
            .context("read isolated tmux pane size")?;
        if output.status.success() {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .clone_into(&mut actual);
            if actual == expected {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!("tmux pane did not resize to {expected}; last size was {actual}")
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
        "isolated tmux acceptance child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn kill_server(namespace: &str) -> Result<()> {
    let tmpdir = env::var_os("TMUX_TMPDIR").context("TMUX_TMPDIR is unset")?;
    kill_server_with_tmpdir(namespace, std::path::Path::new(&tmpdir))
}

fn kill_server_with_tmpdir(namespace: &str, tmpdir: &std::path::Path) -> Result<()> {
    let output = Command::new("tmux")
        .args(["-L", namespace, "kill-server"])
        .env("TMUX_TMPDIR", tmpdir)
        .output()
        .context("invoke tmux kill-server")?;
    if output.status.success()
        || String::from_utf8_lossy(&output.stderr).contains("no server running")
    {
        Ok(())
    } else {
        bail!(
            "kill isolated tmux server: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}
