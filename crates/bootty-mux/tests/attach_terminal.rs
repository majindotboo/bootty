#![cfg(unix)]

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bootty_mux::terminal::{
    AttachLaunch, MuxPaneTarget, PaneStartRequest, ScopedMuxPaneTarget, StartingNativeTerminal,
    TerminalRuntime, start_attach_terminal,
};
use bootty_terminal::frame_source::TerminalFrameSource;
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::terminal_session::TerminalSessionConfig;
use rstest::rstest;

const FRAME_MARKER: &str = "BOOTTY_ATTACH_FRAME";
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);

struct ResumeRepaints(Arc<(Mutex<bool>, Condvar)>);

impl Drop for ResumeRepaints {
    fn drop(&mut self) {
        let (resumed, wakeup) = &*self.0;
        *resumed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        wakeup.notify_all();
    }
}

#[rstest]
fn native_startup_and_later_resizes_queue_without_waiting_for_the_worker() -> Result<()> {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let resume = ResumeRepaints(Arc::clone(&gate));
    let (repaint_tx, repaint_rx) = mpsc::channel();
    let repaint = Arc::new(move || {
        let _ = repaint_tx.send(());
        let (resumed, wakeup) = &*gate;
        drop(
            wakeup.wait_while(
                resumed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                |resumed| !*resumed,
            ),
        );
    });
    let geometry = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    };
    let mut config = TerminalSessionConfig::default();
    config.launch.shell = Some("/bin/sh".to_owned());
    config.launch.args = vec![
        "-c".to_owned(),
        "printf 'READY\\n'; read -r line; printf 'PTY_SIZE:'; stty size; read -r line".to_owned(),
    ];
    config
        .launch
        .env
        .push(("PATH".to_owned(), "/usr/bin:/bin".to_owned()));
    let mut terminal =
        StartingNativeTerminal::spawn(geometry, 1.0, CellMetrics::new(10.0, 20.0), config, repaint);
    // Startup has delivered its runtime and the terminal worker is paused in its repaint callback.
    for _ in 0..2 {
        repaint_rx
            .recv_timeout(FRAME_TIMEOUT)
            .context("native startup repaint")?;
    }

    let resized = TerminalGeometry {
        cols: 83,
        rows: 29,
        ..geometry
    };
    let (resized_tx, resized_rx) = mpsc::channel();
    let resize = thread::spawn(move || {
        let result = (|| {
            terminal.resize(TerminalGeometry {
                cols: 100,
                ..geometry
            })?;
            terminal.resize(resized)?;
            Ok::<_, anyhow::Error>(terminal)
        })();
        let _ = resized_tx.send(result);
    });
    let queued = resized_rx.recv_timeout(FRAME_TIMEOUT);
    drop(resume);
    resize.join().expect("native resize caller");
    let mut terminal = queued.context("presentation resize waited for the paused worker")??;

    // The queued geometry still reaches the emulator and the PTY before subsequent input.
    terminal.write_input(b"\n")?;
    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame deadline")?;
    loop {
        let frame = terminal.extract_frame()?;
        if (frame.cols, frame.rows) == (resized.cols, resized.rows)
            && frame
                .text_rows()
                .iter()
                .any(|row| row.contains("PTY_SIZE:29 83"))
        {
            return Ok(());
        }
        repaint_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("queued resize did not reach the terminal frame and PTY")?;
    }
}

#[rstest]
#[case::local(false)]
#[case::remote(true)]
fn attach_launch_publishes_pane_bytes_and_wakes_the_renderer(#[case] remote: bool) -> Result<()> {
    let target = ScopedMuxPaneTarget::from(MuxPaneTarget::Session {
        session_id: "attach-test".to_owned(),
        cwd: None,
    });
    let (repaint_tx, repaint_rx) = mpsc::channel();
    let repaint_count = Arc::new(AtomicUsize::new(0));
    let repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static> = {
        let repaint_count = Arc::clone(&repaint_count);
        Arc::new(move || {
            repaint_count.fetch_add(1, Ordering::Relaxed);
            let _ = repaint_tx.send(());
        })
    };
    let geometry = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 10,
        cell_height: 20,
    };
    let mut terminal = start_attach_terminal(
        PaneStartRequest {
            target: &target,
            geometry,
            spawn_geometry: geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(10.0, 20.0),
            terminal_config: &TerminalSessionConfig::default(),
            repaint_wakeup: &repaint_wakeup,
        },
        // Mux adapters enter this shared path after constructing their
        // backend-specific command. A shell keeps the frame contract test local.
        AttachLaunch {
            program: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                format!("printf '{FRAME_MARKER} TERM=%s\\n' \"$TERM\""),
            ],
            env_remove: Vec::new(),
            env: Vec::new(),
            term_program: None,
            remote,
        },
    )?;

    let deadline = Instant::now()
        .checked_add(FRAME_TIMEOUT)
        .context("frame deadline")?;
    let mut last_rows = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        repaint_rx
            .recv_timeout(remaining)
            .context("attach terminal did not wake the renderer")?;
        terminal.drain_pty();
        let frame = terminal.extract_frame()?;
        last_rows = frame.text_rows();
        if last_rows.iter().any(|row| row.contains(FRAME_MARKER)) {
            anyhow::ensure!(
                repaint_count.load(Ordering::Relaxed) > 0,
                "published attach frame did not request a repaint"
            );
            if remote {
                anyhow::ensure!(
                    last_rows
                        .iter()
                        .any(|row| row.contains("TERM=xterm-256color")),
                    "remote attach did not use the portable terminal identity: {last_rows:?}"
                );
            }
            return Ok(());
        }
    }

    anyhow::bail!("attach terminal did not publish {FRAME_MARKER:?}: {last_rows:?}")
}
