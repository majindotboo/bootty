#![cfg(test)]
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
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

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const INPUT_MARKER: &str = "BOOTTY_NATIVE_INPUT=accepted";
const PANE_MARKER: &str = "BOOTTY_NATIVE_PANE=native-pane";
const SIZE_MARKER: &str = "BOOTTY_NATIVE_SIZE=30 100";

#[rstest]
fn native_policy_publishes_real_frames_through_gpui() -> Result<()> {
    let directory = assert_fs::TempDir::new().context("create native acceptance directory")?;
    let script = directory.path().join("pane.sh");
    write_pane_script(&script)?;

    let target = ScopedMuxPaneTarget::from(MuxPaneTarget::Pane {
        session_id: "native-session".to_owned(),
        pane_id: "native-pane".to_owned(),
        cwd: Some(directory.path().to_string_lossy().into_owned()),
    });
    let (repaint_tx, repaint_rx) = mpsc::channel();
    let repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(move || {
        let _ = repaint_tx.send(());
    });
    let initial_geometry = geometry(80, 24);
    let resized_geometry = geometry(100, 30);
    let mut config = TerminalSessionConfig::default();
    config.launch.shell = Some(script.to_string_lossy().into_owned());

    let mut policy = bootty_mux::native::NativePanePolicy;
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
        .context("native policy did not start a pane terminal")?;

    let initial = wait_for_frame(&mut *terminal, &repaint_rx, PANE_MARKER)?;
    let initial_rows = initial.text_rows();
    anyhow::ensure!(
        initial_rows
            .iter()
            .any(|row| row.contains("BOOTTY_NATIVE_COLOR π🥟")),
        "native PTY lost deterministic Unicode text: {initial_rows:?}"
    );
    anyhow::ensure!(
        initial_rows.iter().any(|row| row.contains(PANE_MARKER)),
        "native pane identity did not reach the child: {initial_rows:?}"
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
        "native PTY lost the 24-bit foreground color"
    );

    terminal.resize(resized_geometry)?;
    terminal.write_input(b"accepted\n")?;
    let final_frame = wait_for_frame(&mut *terminal, &repaint_rx, SIZE_MARKER)?;
    let final_rows = final_frame.text_rows();
    anyhow::ensure!(
        final_rows.iter().any(|row| row.contains(INPUT_MARKER)),
        "terminal input did not reach the native pane: {final_rows:?}"
    );
    anyhow::ensure!(
        final_rows.iter().any(|row| row.contains(SIZE_MARKER)),
        "terminal resize did not reach the native pane: {final_rows:?}"
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
    bail!("native pane did not publish {marker:?}: {last_rows:?}")
}

fn write_pane_script(path: &std::path::Path) -> Result<()> {
    fs::write(
        path,
        concat!(
            "#!/bin/sh\n",
            "printf '\\033[38;2;12;34;56mBOOTTY_NATIVE_COLOR π🥟\\033[0m\\n'\n",
            "printf 'BOOTTY_NATIVE_PANE=%s\\n' \"$BOOTTY_PANE\"\n",
            "IFS= read -r line\n",
            "printf 'BOOTTY_NATIVE_INPUT=%s\\n' \"$line\"\n",
            "attempt=0\n",
            "while :; do\n",
            "  set -- $(stty size)\n",
            "  [ \"$1 $2\" = '30 100' ] && break\n",
            "  attempt=$((attempt + 1))\n",
            "  [ \"$attempt\" -ge 500 ] && break\n",
            "  sleep 0.01\n",
            "done\n",
            "printf 'BOOTTY_NATIVE_SIZE=%s %s\\n' \"$1\" \"$2\"\n",
            "sleep 30\n",
        ),
    )
    .context("write native pane program")?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

const fn geometry(cols: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry {
        cols,
        rows,
        cell_width: 10,
        cell_height: 20,
    }
}
