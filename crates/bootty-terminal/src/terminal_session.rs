use crate::shell_prompt::{PromptSnapshot, ShellPrompt};
use crate::terminal_capture::{CaptureOptions, TerminalCapture};
use crate::terminal_search::TerminalSearchOptions;
use num_traits::ToPrimitive as _;
use std::{
    fmt::{self, Display, Formatter},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use crate::benchmark_trace::{BenchmarkTrace, TraceValue};
pub use crate::pty_backlog::DrainStats;
use crate::pty_backlog::{PtyBacklog, drain_pty_backlog, drain_pty_backlog_with_limits};
use anyhow::{Context, Result};
use portable_pty::{MasterPty, PtySize};

use crate::geometry::{CellMetrics, TerminalGeometry};
use crate::{
    terminal_engine::{
        TERMINAL_TERM, TerminalColorConfig, TerminalCopyModeAction, TerminalCopyModeOutcome,
        TerminalCursorConfig, TerminalEngine, TerminalFeatureConfig, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
        TerminalSideEffectEvent,
    },
    terminal_frame::{CursorSnapshot, RenderFrame},
    terminal_input_model::{KeyInput, MacosOptionAsAlt, MouseInput},
    terminal_side_effect::deliver_terminal_side_effects,
};

const INPUT_FAST_PATH_DRAIN_BYTES: usize = 64 * 1024;
const INPUT_FAST_PATH_DRAIN_CHUNKS: usize = 8;
const INPUT_FAST_PATH_DRAIN_TIME_US: u128 = 2_000;
// Keep collection bounded across ticks, not just within one tick. Otherwise
// draining the bounded reader channel into this queue defeats PTY backpressure.
// Keep at most about 1 MiB across reader and worker so a finished producer
// does not leave many frames of invisible output. One read may cross the watermark.
const PTY_READ_BYTES: usize = 8 * 1024;
const MAX_PENDING_PTY_BYTES: usize = 512 * 1024;
const MAX_COLLECT_CHUNKS_PER_TICK: usize = 256;
const MAX_READER_QUEUE_CHUNKS: usize = MAX_PENDING_PTY_BYTES / PTY_READ_BYTES;
pub use crate::terminal_launch::{BOOTTY_SHELL_ENV, configured_user_shell};
pub(crate) const WORKER_READY_FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const WORKER_BACKLOG_FRAME_INTERVAL: Duration = Duration::from_millis(64);
/// Pending PTY bytes past which output counts as a flood rather than an interactive redraw, and
/// publishing backs off to [`WORKER_BACKLOG_FRAME_INTERVAL`] to keep the drain moving. A full-screen
/// repaint of a large grid runs tens of kilobytes, so this sits well above one.
pub(crate) const WORKER_FLOOD_BACKLOG_BYTES: usize = 256 * 1024;
pub(crate) const WORKER_IDLE_WAIT: Duration = Duration::from_millis(16);
pub(crate) const WORKER_SETTLED_FRAME_DELAY: Duration = Duration::from_millis(16);
/// Output must stay quiet this long before a frame publishes. Apps split one logical redraw across
///
/// write(2) calls a few hundred microseconds apart: tmux moves the cursor, then opens its
/// synchronized-output block; pi-tui closes its block, then positions the cursor. Publishing inside
/// that gap shows the cursor parked mid-screen, or hidden, until the next frame.
pub const WORKER_OUTPUT_QUIET: Duration = Duration::from_millis(2);
/// The most the quiet window may delay a publish that is otherwise due, so a producer that never
/// goes quiet still gets frames.
pub const WORKER_OUTPUT_HOLD_MAX: Duration = Duration::from_millis(5);
/// How long a purely output-driven cursor move must persist before it becomes visible. Apps park
///
/// their cursor on a scratch cell mid-redraw and move it home a few milliseconds later in a
/// separate write; holding the visible cursor across that gap keeps it from flickering to the
/// scratch position. Keystroke echo commits the cursor immediately, so typing is never delayed.
pub const CURSOR_COMMIT_DELAY: Duration = Duration::from_millis(8);
pub(crate) const SYNC_OUTPUT_MAX_SUPPRESS: Duration = Duration::from_secs(1);
const WORKER_RESPONSE_TIMEOUT: Duration = Duration::from_millis(50);
const WORKER_RESPONSE_COMPLETION_TIMEOUT: Duration = Duration::from_millis(100);
const REQUEST_PENDING: u8 = 0;
const REQUEST_RUNNING: u8 = 1;
const REQUEST_CANCELLED: u8 = 2;

#[derive(Clone, Debug, Default)]
pub struct TerminalSessionConfig {
    pub launch: SessionLaunchConfig,
    pub colors: TerminalColorConfig,
    pub cursor: TerminalCursorConfig,
    pub features: TerminalFeatureConfig,
    pub max_scrollback: usize,
    pub macos_option_as_alt: MacosOptionAsAlt,
    pub side_effect_tx: Option<Sender<TerminalSideEffectEvent>>,
    pub side_effect_pane_id: Option<String>,
    /// Receives protocol-encoded Super key bytes when the attach backend must bypass its own key
    /// parser. Other encoded input continues through the attached PTY.
    pub super_key_input_tx: Option<Sender<Vec<u8>>>,
    pub benchmark_trace: Option<BenchmarkTrace>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLaunchConfig {
    pub shell_integration: bool,
    pub shell: Option<String>,
    pub args: Vec<String>,
    pub working_directory: Option<PathBuf>,
    /// The mux pane this terminal is the front end for, exported as `BOOTTY_PANE`. Only backends
    /// that spawn the pane's own PTY know it, so it stays unset for a tmux attach, where tmux
    /// exports the same id as `$TMUX_PANE`.
    pub pane_id: Option<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub term: String,
    pub colorterm: String,
    pub term_program: Option<String>,
}

impl Default for SessionLaunchConfig {
    fn default() -> Self {
        Self {
            shell_integration: false,
            shell: None,
            args: Vec::new(),
            working_directory: None,
            pane_id: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            term: TERMINAL_TERM.to_owned(),
            colorterm: "truecolor".to_owned(),
            term_program: None,
        }
    }
}

pub struct TerminalSession {
    command_tx: Sender<TerminalCommand>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    worker_health: Arc<WorkerHealth>,
    current_working_directory: Arc<Mutex<Option<String>>>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    child: crate::terminal_launch::OwnedChild,
    tty_name: Option<String>,
}

type RepaintWakeup = Arc<dyn Fn() + Send + Sync + 'static>;

pub(crate) struct PublishedFrame {
    latest: Mutex<Arc<RenderFrame>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalWorkerFailure {
    operation: &'static str,
    source: String,
}

impl Display for TerminalWorkerFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "terminal worker {}: {}",
            self.operation, self.source
        )
    }
}

#[derive(Debug, Default)]
struct WorkerHealth {
    latest: Mutex<Option<TerminalWorkerFailure>>,
}

impl WorkerHealth {
    fn record(&self, operation: &'static str, source: impl Display) {
        let mut latest = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *latest = Some(TerminalWorkerFailure {
            operation,
            source: source.to_string(),
        });
    }

    fn take(&self) -> Result<Option<TerminalWorkerFailure>> {
        self.latest
            .lock()
            .map(|mut latest| latest.take())
            .map_err(|_| anyhow::anyhow!("terminal worker health lock poisoned"))
    }
}

impl PublishedFrame {
    pub(crate) fn new() -> Self {
        Self {
            latest: Mutex::new(Arc::new(RenderFrame::default())),
        }
    }

    pub(crate) fn load(&self) -> Result<Arc<RenderFrame>> {
        self.latest
            .lock()
            .map(|frame| Arc::clone(&frame))
            .map_err(|_| anyhow::anyhow!("terminal render frame lock poisoned"))
    }

    pub(crate) fn publish(&self, frame: &RenderFrame) -> Result<()> {
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal render frame lock poisoned"))?;
        *latest = Arc::new(frame.clone());
        drop(latest);
        Ok(())
    }

    pub(crate) fn publish_with_cursor(
        &self,
        frame: &RenderFrame,
        cursor: Option<CursorSnapshot>,
    ) -> Result<()> {
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal render frame lock poisoned"))?;
        let mut cloned = frame.clone();
        cloned.cursor = cursor;
        *latest = Arc::new(cloned);
        drop(latest);
        Ok(())
    }
}

type SelectionFormatResponse = std::result::Result<Option<Vec<u8>>, String>;
type MouseTrackingResponse = std::result::Result<bool, String>;
type SearchViewportResponse = std::result::Result<bool, String>;
type CopyModeActiveResponse = std::result::Result<bool, String>;
type CopyModeActionResponse = std::result::Result<TerminalCopyModeOutcome, String>;

/// Worker-side half of a single-response request, claimed with [`WorkerRequest::try_claim`] before
/// the work runs so a caller that already timed out is not served.
pub struct WorkerRequest<T> {
    state: Arc<AtomicU8>,
    sender: Sender<T>,
}

/// Caller-side half of a single-response request.
pub struct PendingWorkerResponse<T> {
    state: Arc<AtomicU8>,
    receiver: Receiver<T>,
}

/// Creates a request/response pair for one worker round trip.
#[must_use]
pub fn worker_request<T>() -> (WorkerRequest<T>, PendingWorkerResponse<T>) {
    let state = Arc::new(AtomicU8::new(REQUEST_PENDING));
    let (sender, receiver) = mpsc::channel();
    (
        WorkerRequest {
            state: Arc::clone(&state),
            sender,
        },
        PendingWorkerResponse { state, receiver },
    )
}

impl<T> WorkerRequest<T> {
    /// Takes ownership of the request. Returns `false` when the caller already gave up.
    #[must_use]
    pub fn try_claim(&self) -> bool {
        self.state
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Delivers the response to the waiting caller.
    pub fn send(self, response: T) {
        if self.state.load(Ordering::Acquire) == REQUEST_RUNNING {
            let _ = self.sender.send(response);
        }
    }
}

impl<T> PendingWorkerResponse<T> {
    /// Waits for the worker response, naming `operation` in any error.
    ///
    /// # Errors
    /// Returns an error if the worker disconnects or exceeds the response deadline.
    /// A completion timeout means the claimed operation may already have run.
    pub fn receive(self, operation: &'static str) -> Result<T> {
        match self.receiver.recv_timeout(WORKER_RESPONSE_TIMEOUT) {
            Ok(response) => Ok(response),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                "terminal worker stopped before {operation}"
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if self
                    .state
                    .compare_exchange(
                        REQUEST_PENDING,
                        REQUEST_CANCELLED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return Err(anyhow::anyhow!(
                        "terminal worker timed out before {operation}"
                    ));
                }

                match self
                    .receiver
                    .recv_timeout(WORKER_RESPONSE_COMPLETION_TIMEOUT)
                {
                    Ok(response) => Ok(response),
                    Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                        "terminal worker stopped before {operation}"
                    )),
                    Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow::anyhow!(
                        "terminal worker completion unknown after {operation}; the operation may have completed"
                    )),
                }
            }
        }
    }
}

enum TerminalCommand {
    PtyReady,
    DisplayScale {
        display_scale: f32,
        pty_size: PtySize,
    },
    RenderCellMetrics {
        cell: CellMetrics,
        pty_size: PtySize,
    },
    ApplyLiveConfig(TerminalLiveConfig),
    Resize {
        geometry: TerminalGeometry,
        pty_size: PtySize,
        done: Option<WorkerRequest<()>>,
    },
    Key(KeyInput),
    Focus(bool),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
    Paste(String),
    RawInput(Vec<u8>),
    MouseViewportScroll {
        delta: isize,
    },
    MouseViewportScrollTo {
        offset: usize,
    },
    EnterCopyMode,
    SelectionBegin(TerminalSelectionEvent),
    SelectionUpdate(TerminalSelectionEvent),
    SelectionEnd(Option<TerminalSelectionEvent>),
    Prompt {
        text: Option<(u64, String, bool)>,
        done: WorkerRequest<std::result::Result<PromptSnapshot, String>>,
    },
    Capture {
        options: CaptureOptions,
        done: WorkerRequest<std::result::Result<TerminalCapture, String>>,
    },
    FormatSelection {
        format: TerminalSelectionFormat,
        done: WorkerRequest<SelectionFormatResponse>,
    },
    CopyModeActive(WorkerRequest<CopyModeActiveResponse>),
    CopyModeAction {
        action: TerminalCopyModeAction,
        done: WorkerRequest<CopyModeActionResponse>,
    },
    SearchViewport {
        options: TerminalSearchOptions,
        query: String,
        direction: TerminalSearchDirection,
        done: WorkerRequest<SearchViewportResponse>,
    },
    IsMouseTracking(WorkerRequest<MouseTrackingResponse>),
    DiscardPendingOutput(WorkerRequest<()>),
}
impl TerminalSession {
    ///
    /// # Errors
    /// Returns an error if the shell PTY, process, I/O handles, or terminal worker cannot be started.
    pub fn new(geometry: TerminalGeometry) -> Result<Self> {
        Self::new_with_repaint_wakeup(geometry, Arc::new(|| {}))
    }

    ///
    /// # Errors
    /// Returns an error if the shell PTY, process, I/O handles, or terminal worker cannot be started.
    pub fn new_with_repaint_wakeup(
        geometry: TerminalGeometry,
        repaint_wakeup: RepaintWakeup,
    ) -> Result<Self> {
        Self::new_with_config(geometry, TerminalSessionConfig::default(), repaint_wakeup)
    }

    ///
    /// # Errors
    /// Returns an error if the shell PTY, process, I/O handles, or terminal worker cannot be started.
    pub fn new_with_config(
        geometry: TerminalGeometry,
        config: TerminalSessionConfig,
        repaint_wakeup: RepaintWakeup,
    ) -> Result<Self> {
        Self::new_with_config_and_host_metrics(
            geometry,
            1.0,
            CellMetrics::new(
                geometry.cell_width.to_f32().unwrap_or(f32::MAX),
                geometry.cell_height.to_f32().unwrap_or(f32::MAX),
            ),
            config,
            repaint_wakeup,
        )
    }

    ///
    /// # Errors
    /// Returns an error if the shell PTY, process, I/O handles, or terminal worker cannot be started.
    pub fn new_with_config_and_host_metrics(
        geometry: TerminalGeometry,
        display_scale: f32,
        render_cell: CellMetrics,
        config: TerminalSessionConfig,
        repaint_wakeup: RepaintWakeup,
    ) -> Result<Self> {
        let pty_size = physical_pty_size(geometry, render_cell, display_scale);
        let (pty_master, child, tty_name) =
            crate::terminal_launch::spawn(pty_size, &config.launch)?.into_parts();
        let mut reader = pty_master.try_clone_reader()?;
        let pty_writer = Arc::new(Mutex::new(pty_master.take_writer()?));
        let (pty_tx, pty_rx) = mpsc::sync_channel(MAX_READER_QUEUE_CHUNKS);
        let (command_tx, command_rx) = mpsc::channel();
        let pty_wakeup = command_tx.clone();
        let pty_wakeup_pending = Arc::new(AtomicBool::new(false));
        let reader_wakeup_pending = Arc::clone(&pty_wakeup_pending);
        thread::spawn(move || {
            let mut buf = [0_u8; PTY_READ_BYTES];
            while let Ok(n) = reader.read(&mut buf) {
                let Some(bytes) = buf.get(..n) else {
                    break;
                };
                if bytes.is_empty() || pty_tx.send(bytes.to_vec()).is_err() {
                    break;
                }
                if !reader_wakeup_pending.swap(true, Ordering::AcqRel)
                    && pty_wakeup.send(TerminalCommand::PtyReady).is_err()
                {
                    break;
                }
            }
            // EOF must wake an idle worker after the byte channel has closed.
            drop(pty_tx);
            let _ = pty_wakeup.send(TerminalCommand::PtyReady);
        });
        let latest_frame = Arc::new(PublishedFrame::new());
        let latest_drain = Arc::new(Mutex::new(DrainStats::default()));
        let pending_pty_len = Arc::new(AtomicUsize::new(0));
        let worker_health = Arc::new(WorkerHealth::default());
        let current_working_directory = Arc::new(Mutex::new(None));
        let benchmark_trace = match config.benchmark_trace.clone() {
            Some(trace) => Some(trace),
            None => BenchmarkTrace::from_env().context("open benchmark trace")?,
        };
        spawn_terminal_worker(TerminalWorkerConfig {
            geometry,
            display_scale,
            render_cell,
            pty_size,
            colors: config.colors,
            cursor: config.cursor,
            features: config.features,
            max_scrollback: config.max_scrollback,
            macos_option_as_alt: config.macos_option_as_alt,
            pty_master,
            pty_rx,
            pty_wakeup_pending,
            pty_writer,
            command_rx,
            latest_frame: latest_frame.clone(),
            latest_drain: latest_drain.clone(),
            pending_pty_len: pending_pty_len.clone(),
            worker_health: Arc::clone(&worker_health),
            current_working_directory: current_working_directory.clone(),
            repaint_wakeup,
            side_effect_tx: config.side_effect_tx,
            side_effect_pane_id: config.side_effect_pane_id,
            super_key_input_tx: config.super_key_input_tx,
            benchmark_trace,
        })?;

        Ok(Self {
            command_tx,
            latest_frame,
            latest_drain,
            pending_pty_len,
            worker_health,
            current_working_directory,
            geometry,
            display_scale,
            render_cell,
            child,
            tty_name,
        })
    }

    #[must_use]
    pub const fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    ///
    /// # Errors
    /// Returns an error if resizing fails, the worker disconnects, or the resize acknowledgement times out.
    pub fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        // Even an unchanged request must settle earlier queued geometry before returning.
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::Resize {
            geometry,
            pty_size: physical_pty_size(geometry, self.render_cell, self.display_scale),
            done: Some(done),
        })?;
        response.receive("resizing")?;
        self.check_worker_error()?;
        self.geometry = geometry;

        Ok(())
    }

    /// Queue a resize without waiting for the worker to publish it.
    ///
    /// # Errors
    /// Returns an error for an empty grid, a failed worker, or a closed command channel.
    pub fn queue_resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        anyhow::ensure!(
            geometry.cols > 0 && geometry.rows > 0,
            "terminal grid must be nonempty"
        );
        if geometry == self.geometry {
            return Ok(());
        }
        self.send_command(TerminalCommand::Resize {
            geometry,
            pty_size: physical_pty_size(geometry, self.render_cell, self.display_scale),
            done: None,
        })?;
        self.geometry = geometry;
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        if (self.display_scale - display_scale).abs() <= f32::EPSILON {
            return Ok(());
        }
        self.send_command(TerminalCommand::DisplayScale {
            display_scale,
            pty_size: physical_pty_size(self.geometry, self.render_cell, display_scale),
        })?;
        self.display_scale = display_scale;
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        if self.render_cell == cell {
            return Ok(());
        }
        self.send_command(TerminalCommand::RenderCellMetrics {
            cell,
            pty_size: physical_pty_size(self.geometry, cell, self.display_scale),
        })?;
        self.render_cell = cell;
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        // The worker applies the aggregate in colors, cursor, features order.
        self.send_command(TerminalCommand::ApplyLiveConfig(config))
    }

    pub fn drain_pty(&mut self) -> DrainStats {
        let Ok(mut stats) = self.latest_drain.lock() else {
            return DrainStats::default();
        };
        let drained = *stats;
        *stats = DrainStats::default();
        drained
    }

    #[must_use]
    pub fn pending_pty_len(&self) -> usize {
        self.pending_pty_len.load(Ordering::Relaxed)
    }

    ///
    /// # Errors
    /// Returns an error if the worker failed or the shell process cannot be polled.
    pub fn child_exited(&mut self) -> Result<bool> {
        self.check_worker_error()?;
        self.child.exited()
    }

    #[must_use]
    pub fn tty_name(&self) -> Option<&str> {
        self.tty_name.as_deref()
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn write_input(&self, bytes: &[u8]) -> Result<()> {
        self.send_command(TerminalCommand::RawInput(bytes.to_vec()))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn write_paste(&mut self, text: &str) -> Result<()> {
        self.send_command(TerminalCommand::Paste(text.to_owned()))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.send_command(TerminalCommand::Key(input))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.send_command(TerminalCommand::Focus(gained))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.send_command(TerminalCommand::Mouse(input))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.send_command(TerminalCommand::MouseWheel {
            input,
            scroll_delta,
        })
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.send_command(TerminalCommand::MouseViewportScroll { delta })
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn scroll_viewport_to(&mut self, offset: usize) -> Result<()> {
        self.send_command(TerminalCommand::MouseViewportScrollTo { offset })
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn enter_copy_mode(&mut self) -> Result<()> {
        self.send_command(TerminalCommand::EnterCopyMode)
    }

    ///
    /// # Errors
    /// Returns an error if the worker fails, times out, disconnects, or rejects the requested terminal operation.
    pub fn copy_mode_active(&mut self) -> Result<bool> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::CopyModeActive(done))?;
        response
            .receive("reporting copy mode")?
            .map_err(anyhow::Error::msg)
    }

    ///
    /// # Errors
    /// Returns an error if the worker fails, times out, disconnects, or rejects the requested terminal operation.
    pub fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::CopyModeAction { action, done })?;
        response
            .receive("handling copy mode action")?
            .map_err(anyhow::Error::msg)
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(TerminalCommand::SelectionBegin(event))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(TerminalCommand::SelectionUpdate(event))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.send_command(TerminalCommand::SelectionEnd(event))
    }

    ///
    /// # Errors
    /// Returns an error if the worker has failed or its command channel is closed.
    pub fn prompt(
        &mut self,
        text: Option<(u64, String, bool)>,
    ) -> Result<PendingWorkerResponse<std::result::Result<PromptSnapshot, String>>> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::Prompt { text, done })?;
        Ok(response)
    }

    ///
    /// # Errors
    /// Returns an error for invalid capture limits, a failed worker, or a closed command channel.
    pub fn capture(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        options.validate().map_err(anyhow::Error::msg)?;
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::Capture { options, done })?;
        Ok(response)
    }

    ///
    /// # Errors
    /// Returns an error if the worker fails, times out, disconnects, or rejects the requested terminal operation.
    pub fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::FormatSelection { format, done })?;
        response
            .receive("formatting selection")?
            .map_err(anyhow::Error::msg)
    }

    ///
    /// # Errors
    /// Returns an error if the worker fails, times out, disconnects, or rejects the requested terminal operation.
    pub fn search_viewport(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<bool> {
        self.search_viewport_with_options(query, direction, TerminalSearchOptions::default())
    }

    ///
    /// # Errors
    /// Returns an error if the worker fails, times out, disconnects, or rejects the requested terminal operation.
    pub fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<bool> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::SearchViewport {
            options,
            query: query.to_owned(),
            direction,
            done,
        })?;
        response
            .receive("searching scrollback")?
            .map_err(anyhow::Error::msg)
    }

    ///
    /// # Errors
    /// Returns an error if the worker fails, times out, disconnects, or rejects the requested terminal operation.
    pub fn is_mouse_tracking(&mut self) -> Result<bool> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::IsMouseTracking(done))?;
        response
            .receive("reporting mouse tracking")?
            .map_err(anyhow::Error::msg)
    }

    #[must_use]
    pub fn current_working_directory(&self) -> Option<String> {
        self.current_working_directory
            .lock()
            .ok()
            .and_then(|cwd| cwd.clone())
    }

    ///
    /// # Errors
    /// Returns an error if the worker fails, disconnects, or does not acknowledge the discard in time.
    pub fn discard_pending_output(&mut self) -> Result<()> {
        let (done, response) = worker_request();
        self.send_command(TerminalCommand::DiscardPendingOutput(done))?;
        response.receive("discarding output")
    }

    ///
    /// # Errors
    /// Returns an error if the worker failed or the published frame lock is poisoned.
    pub fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.check_worker_error()?;
        self.latest_frame.load()
    }

    fn send_command(&self, command: TerminalCommand) -> Result<()> {
        self.check_worker_error()?;
        self.command_tx
            .send(command)
            .map_err(|_| anyhow::anyhow!("terminal worker stopped"))
    }

    fn check_worker_error(&self) -> Result<()> {
        if let Some(failure) = self.worker_health.take()? {
            anyhow::bail!(failure);
        }
        Ok(())
    }
}

struct TerminalWorkerConfig {
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    pty_size: PtySize,
    colors: TerminalColorConfig,
    cursor: TerminalCursorConfig,
    features: TerminalFeatureConfig,
    max_scrollback: usize,
    macos_option_as_alt: MacosOptionAsAlt,
    pty_master: Box<dyn MasterPty + Send>,
    pty_rx: Receiver<Vec<u8>>,
    pty_wakeup_pending: Arc<AtomicBool>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    command_rx: Receiver<TerminalCommand>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    worker_health: Arc<WorkerHealth>,
    current_working_directory: Arc<Mutex<Option<String>>>,
    repaint_wakeup: RepaintWakeup,
    side_effect_tx: Option<Sender<TerminalSideEffectEvent>>,
    side_effect_pane_id: Option<String>,
    super_key_input_tx: Option<Sender<Vec<u8>>>,
    benchmark_trace: Option<BenchmarkTrace>,
}

fn physical_pty_size(
    geometry: TerminalGeometry,
    render_cell: CellMetrics,
    display_scale: f32,
) -> PtySize {
    let (cell_width, cell_height) = render_cell.physical_size(display_scale);
    PtySize {
        rows: geometry.rows,
        cols: geometry.cols,
        pixel_width: u16::try_from(u32::from(geometry.cols).saturating_mul(cell_width))
            .unwrap_or(u16::MAX),
        pixel_height: u16::try_from(u32::from(geometry.rows).saturating_mul(cell_height))
            .unwrap_or(u16::MAX),
    }
}

fn spawn_terminal_worker(config: TerminalWorkerConfig) -> Result<()> {
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut engine = match TerminalEngine::new_with_terminal_options(
            config.geometry,
            config.colors,
            config.cursor,
            config.features,
            config.max_scrollback,
            config.macos_option_as_alt,
        ) {
            Ok(engine) => engine,
            Err(error) => {
                let _ = startup_tx.send(Err(error.to_string()));
                return;
            }
        };
        engine.set_display_scale(config.display_scale);
        engine.set_render_cell_metrics(config.render_cell);
        let callback_writer = config.pty_writer.clone();
        let callback_health = Arc::clone(&config.worker_health);
        if let Err(error) = engine.on_pty_write(move |_terminal, bytes| {
            write_pty(&callback_writer, bytes, &callback_health);
        }) {
            let _ = startup_tx.send(Err(error.to_string()));
            return;
        }
        let _ = startup_tx.send(Ok(()));
        let mut worker = TerminalWorker {
            shell_prompt: ShellPrompt::default(),
            engine,
            pty_master: config.pty_master,
            pty_size: config.pty_size,
            pty_rx: config.pty_rx,
            pty_wakeup_pending: config.pty_wakeup_pending,
            pty_writer: config.pty_writer,
            command_rx: config.command_rx,
            latest_frame: config.latest_frame,
            latest_drain: config.latest_drain,
            pending_pty_len: config.pending_pty_len,
            worker_health: config.worker_health,
            current_working_directory: config.current_working_directory,
            repaint_wakeup: config.repaint_wakeup,
            side_effect_tx: config.side_effect_tx,
            side_effect_pane_id: config.side_effect_pane_id,
            super_key_input_tx: config.super_key_input_tx,
            benchmark_trace: config.benchmark_trace,
            output_buf: Vec::with_capacity(1024),
            pending_pty: PtyBacklog::with_capacity(MAX_COLLECT_CHUNKS_PER_TICK),
            pending_resize_ack: None,
            last_frame_publish: Instant::now()
                .checked_sub(WORKER_READY_FRAME_INTERVAL)
                .unwrap_or_else(Instant::now),
            has_unpublished_frame: false,
            sync_output_since: None,
            sync_output_batch_pending: false,
            deferred_sync_publish: false,
            last_terminal_change: None,
            settle: OutputSettle::default(),
            last_hold: PublishHold::None,
            cursor_hold: CursorHold::default(),
            force_next_frame_publish: false,
            command_disconnected: false,
            pending_command: None,
            pty_disconnected: false,
        };
        worker.trace_event(
            "worker_start",
            &[
                ("cols", TraceValue::U64(u64::from(config.geometry.cols))),
                ("rows", TraceValue::U64(u64::from(config.geometry.rows))),
            ],
        );
        worker.run();
    });

    startup_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("terminal worker failed to start"))?
        .map_err(|error| anyhow::anyhow!(error))
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "Transport lifetimes and frame publication flags vary independently."
)]
struct TerminalWorker {
    shell_prompt: ShellPrompt,
    engine: TerminalEngine,
    pty_master: Box<dyn MasterPty + Send>,
    pty_size: PtySize,
    pty_rx: Receiver<Vec<u8>>,
    pty_wakeup_pending: Arc<AtomicBool>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    command_rx: Receiver<TerminalCommand>,
    pending_command: Option<TerminalCommand>,
    pending_resize_ack: Option<WorkerRequest<()>>,
    latest_frame: Arc<PublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_pty_len: Arc<AtomicUsize>,
    worker_health: Arc<WorkerHealth>,
    current_working_directory: Arc<Mutex<Option<String>>>,
    repaint_wakeup: RepaintWakeup,
    side_effect_tx: Option<Sender<TerminalSideEffectEvent>>,
    side_effect_pane_id: Option<String>,
    super_key_input_tx: Option<Sender<Vec<u8>>>,
    output_buf: Vec<u8>,
    pending_pty: PtyBacklog,
    last_frame_publish: Instant,
    has_unpublished_frame: bool,
    sync_output_since: Option<Instant>,
    sync_output_batch_pending: bool,
    deferred_sync_publish: bool,
    last_terminal_change: Option<Instant>,
    settle: OutputSettle,
    last_hold: PublishHold,
    cursor_hold: CursorHold,
    force_next_frame_publish: bool,
    command_disconnected: bool,
    pty_disconnected: bool,
    benchmark_trace: Option<BenchmarkTrace>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WorkerCommandStats {
    did_work: bool,
    terminal_changed: bool,
    commands: usize,
}

impl TerminalWorker {
    fn run(&mut self) {
        loop {
            let command_stats = self.process_commands();
            let mut did_work = command_stats.did_work;
            let mut terminal_changed = command_stats.terminal_changed;
            did_work |= self.collect_pty();
            let stats = self.drain_pty();
            terminal_changed |= stats.bytes > 0;
            did_work |= stats.bytes > 0;
            if terminal_changed {
                self.mark_unpublished_frame();
            }
            // After the drain, not before: a wakeup for bytes that trail a completed sync batch
            // must land in the frame this publishes, not the one after it.
            self.publish_deferred_sync_frame();

            if did_work {
                self.publish_drain(stats);
            }
            if self.should_publish_frame() {
                self.publish_frame();
                self.last_frame_publish = Instant::now();
                if !did_work {
                    continue;
                }
            }
            if !did_work {
                if self.should_stop() {
                    break;
                }
                let wait = if self.has_unpublished_frame && self.last_hold == PublishHold::Settling
                {
                    WORKER_OUTPUT_QUIET
                } else if self.cursor_hold.pending() {
                    CURSOR_COMMIT_DELAY
                } else {
                    WORKER_IDLE_WAIT
                };
                let command = if self.has_unpublished_frame || self.cursor_hold.pending() {
                    self.command_rx.recv_timeout(wait)
                } else {
                    self.command_rx
                        .recv()
                        .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                };
                match command {
                    Ok(command) => self.pending_command = Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.command_disconnected = true;
                    }
                }
            }
        }
        self.trace_event("worker_stop", &[]);
    }

    const fn should_stop(&self) -> bool {
        self.command_disconnected && self.pty_disconnected && self.pending_pty.is_empty()
    }

    fn should_publish_frame(&mut self) -> bool {
        let hold = if self.sync_output_suppressed() {
            // The quiet window measures its own delay, so it restarts after a sync block.
            self.settle.reset();
            PublishHold::SyncOutput
        } else if self.settling() {
            PublishHold::Settling
        } else {
            PublishHold::None
        };
        self.last_hold = hold;
        should_publish_frame_after_work(
            self.has_unpublished_frame,
            self.force_next_frame_publish,
            hold,
            self.pending_pty.len(),
            self.last_terminal_change
                .map_or(Duration::ZERO, |instant| instant.elapsed()),
            self.last_frame_publish.elapsed(),
        ) || self.cursor_hold.commit_due(Instant::now())
    }

    fn settling(&mut self) -> bool {
        self.settle.holds(
            self.last_terminal_change
                .map_or(Duration::ZERO, |instant| instant.elapsed()),
        )
    }

    fn sync_output_suppressed(&mut self) -> bool {
        let active = synchronized_output_state(&self.engine, &self.worker_health);
        let elapsed = if active {
            self.sync_output_since
                .get_or_insert_with(Instant::now)
                .elapsed()
        } else {
            self.sync_output_since = None;
            Duration::ZERO
        };
        let observed = std::mem::take(&mut self.sync_output_batch_pending);
        if observed && !active {
            self.deferred_sync_publish = true;
        }
        sync_output_suppresses_publish(active, observed, elapsed)
    }

    fn publish_deferred_sync_frame(&mut self) {
        if !self.deferred_sync_publish || !self.has_unpublished_frame {
            return;
        }
        if synchronized_output_state(&self.engine, &self.worker_health) || self.settling() {
            return;
        }
        self.publish_frame();
        self.last_frame_publish = Instant::now();
    }

    fn mark_unpublished_frame(&mut self) {
        self.has_unpublished_frame = true;
        self.last_terminal_change = Some(Instant::now());
    }

    const fn mark_input_fast_path(&mut self) {
        self.force_next_frame_publish = true;
    }

    fn process_commands(&mut self) -> WorkerCommandStats {
        let mut stats = WorkerCommandStats::default();
        loop {
            let command = if let Some(command) = self.pending_command.take() {
                command
            } else {
                match self.command_rx.try_recv() {
                    Ok(command) => command,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.command_disconnected = true;
                        break;
                    }
                }
            };
            stats.did_work = true;
            stats.commands = stats.commands.saturating_add(1);
            self.process_command(command, &mut stats);
        }
        if stats.commands > 0 {
            self.trace_event(
                "input_commands",
                &[
                    ("commands", TraceValue::Usize(stats.commands)),
                    ("terminal_changed", TraceValue::Bool(stats.terminal_changed)),
                ],
            );
        }
        stats
    }

    fn process_command(&mut self, command: TerminalCommand, stats: &mut WorkerCommandStats) {
        match command {
            TerminalCommand::PtyReady => {}
            TerminalCommand::DisplayScale {
                display_scale,
                pty_size,
            } => {
                self.engine.set_display_scale(display_scale);
                self.resize_pty(pty_size);
                stats.terminal_changed = true;
            }
            TerminalCommand::RenderCellMetrics { cell, pty_size } => {
                self.engine.set_render_cell_metrics(cell);
                self.resize_pty(pty_size);
                stats.terminal_changed = true;
            }
            TerminalCommand::Resize {
                geometry,
                pty_size,
                done,
            } => self.resize_command(geometry, pty_size, done, stats),
            TerminalCommand::ApplyLiveConfig(config) => {
                match self.engine.apply_live_config(config) {
                    Ok(()) => stats.terminal_changed = true,
                    Err(error) => self.worker_health.record("apply_live_config", error),
                }
            }
            TerminalCommand::Key(input) => self.key_command(input, stats),
            TerminalCommand::Focus(gained) => self.focus_command(gained),
            TerminalCommand::Mouse(input) => self.mouse_command(input),
            TerminalCommand::MouseWheel {
                input,
                scroll_delta,
            } => self.mouse_wheel_command(input, scroll_delta, stats),
            TerminalCommand::Paste(text) => self.paste_command(&text, stats),
            TerminalCommand::DiscardPendingOutput(done) => {
                if !done.try_claim() {
                    return;
                }
                self.discard_pending_output_queue();
                done.send(());
            }
            TerminalCommand::RawInput(bytes) => self.raw_input_command(&bytes, stats),
            TerminalCommand::MouseViewportScroll { delta } => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_delta(delta);
                stats.terminal_changed = true;
            }
            TerminalCommand::MouseViewportScrollTo { offset } => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_to(offset);
                stats.terminal_changed = true;
            }
            TerminalCommand::EnterCopyMode => {
                self.mark_input_fast_path();
                match self.engine.enter_copy_mode() {
                    Ok(()) => stats.terminal_changed = true,
                    Err(error) => self.worker_health.record("enter_copy_mode", error),
                }
            }
            TerminalCommand::SelectionBegin(event) => self.selection_begin_command(event, stats),
            TerminalCommand::SelectionUpdate(event) => self.selection_update_command(event, stats),
            TerminalCommand::SelectionEnd(event) => self.selection_end_command(event, stats),
            TerminalCommand::Prompt { text, done } => self.prompt_command(text, done, stats),
            TerminalCommand::Capture { options, done } => self.capture_command(options, done),
            TerminalCommand::FormatSelection { format, done } => {
                self.format_selection_command(format, done);
            }
            TerminalCommand::CopyModeActive(done) => {
                if done.try_claim() {
                    done.send(Ok(self.engine.copy_mode_active()));
                }
            }
            TerminalCommand::CopyModeAction { action, done } => {
                self.copy_mode_command(action, done, stats);
            }
            TerminalCommand::SearchViewport {
                options,
                query,
                direction,
                done,
            } => self.search_command(options, &query, direction, done),
            TerminalCommand::IsMouseTracking(done) => {
                if !done.try_claim() {
                    return;
                }
                let response = self
                    .engine
                    .is_mouse_tracking()
                    .map_err(|error| error.to_string());
                done.send(response);
            }
        }
    }

    fn selection_begin_command(
        &mut self,
        event: TerminalSelectionEvent,
        stats: &mut WorkerCommandStats,
    ) {
        self.mark_input_fast_path();
        match self.engine.begin_selection(event) {
            Ok(()) => stats.terminal_changed = true,
            Err(error) => self.worker_health.record("selection_begin", error),
        }
    }

    fn selection_update_command(
        &mut self,
        event: TerminalSelectionEvent,
        stats: &mut WorkerCommandStats,
    ) {
        self.mark_input_fast_path();
        match self.engine.update_selection(event) {
            Ok(()) => stats.terminal_changed = true,
            Err(error) => self.worker_health.record("selection_update", error),
        }
    }

    fn selection_end_command(
        &mut self,
        event: Option<TerminalSelectionEvent>,
        stats: &mut WorkerCommandStats,
    ) {
        self.mark_input_fast_path();
        match self.engine.end_selection(event) {
            Ok(()) => stats.terminal_changed = true,
            Err(error) => self.worker_health.record("selection_end", error),
        }
    }

    fn capture_command(
        &self,
        options: CaptureOptions,
        done: WorkerRequest<std::result::Result<TerminalCapture, String>>,
    ) {
        if done.try_claim() {
            done.send(
                self.engine
                    .capture(options)
                    .map_err(|error| error.to_string()),
            );
        }
    }

    fn format_selection_command(
        &self,
        format: TerminalSelectionFormat,
        done: WorkerRequest<SelectionFormatResponse>,
    ) {
        if !done.try_claim() {
            return;
        }
        let response = self
            .engine
            .format_selection(format)
            .map_err(|error| error.to_string());
        done.send(response);
    }
    fn resize_command(
        &mut self,
        geometry: TerminalGeometry,
        pty_size: PtySize,
        done: Option<WorkerRequest<()>>,
        stats: &mut WorkerCommandStats,
    ) {
        if let Some(done) = done.as_ref()
            && !done.try_claim()
        {
            return;
        }
        let result = self.resize(geometry, pty_size);
        if let Err(error) = result {
            self.worker_health.record("resize", error);
            if let Some(done) = done {
                done.send(());
            }
        } else {
            stats.terminal_changed = true;
            self.pending_resize_ack = done;
        }
    }

    fn key_command(&mut self, input: KeyInput, stats: &mut WorkerCommandStats) {
        self.shell_prompt.input(matches!(
            input.key,
            crate::terminal_input_model::TerminalKey::Enter
                | crate::terminal_input_model::TerminalKey::NumpadEnter
        ));
        self.mark_input_fast_path();
        self.engine.scroll_viewport_bottom();
        stats.terminal_changed = true;
        match self.engine.encode_key_to_vec(input, &mut self.output_buf) {
            Ok(()) => self.write_key_output(input),
            Err(error) => self.worker_health.record("encode_key", error),
        }
    }

    fn focus_command(&mut self, gained: bool) {
        self.mark_input_fast_path();
        match self
            .engine
            .encode_focus_to_vec(gained, &mut self.output_buf)
        {
            Ok(()) => self.write_output_buf(),
            Err(error) => self.worker_health.record("encode_focus", error),
        }
    }

    fn mouse_command(&mut self, input: MouseInput) {
        if self.engine.is_mouse_tracking().unwrap_or(false) {
            self.shell_prompt.input(false);
        }
        match self.engine.encode_mouse_to_vec(input, &mut self.output_buf) {
            // Motion over a pane with no mouse tracking sends nothing, so it must not
            // force a publish either: that would bypass the quiet window on every move.
            Ok(()) if self.output_buf.is_empty() => {}
            Ok(()) => {
                self.mark_input_fast_path();
                self.write_output_buf();
            }
            Err(error) => self.worker_health.record("encode_mouse", error),
        }
    }

    fn mouse_wheel_command(
        &mut self,
        input: MouseInput,
        scroll_delta: isize,
        stats: &mut WorkerCommandStats,
    ) {
        match self.engine.is_mouse_tracking() {
            Ok(true) => {
                self.mark_input_fast_path();
                match self.engine.encode_mouse_wheel_to_vec(
                    input,
                    scroll_delta.unsigned_abs().max(1),
                    &mut self.output_buf,
                ) {
                    Ok(()) => self.write_output_buf(),
                    Err(error) => self.worker_health.record("encode_mouse_wheel", error),
                }
            }
            Ok(false) if scroll_delta != 0 => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_delta(scroll_delta);
                stats.terminal_changed = true;
            }
            Ok(false) => {}
            Err(error) => self.worker_health.record("mouse_tracking_for_wheel", error),
        }
    }

    fn paste_command(&mut self, text: &str, stats: &mut WorkerCommandStats) {
        self.shell_prompt.input(false);
        self.mark_input_fast_path();
        self.engine.scroll_viewport_bottom();
        stats.terminal_changed = true;
        match self.engine.encode_paste_to_vec(text, &mut self.output_buf) {
            Ok(()) => self.write_output_buf(),
            Err(error) => self.worker_health.record("encode_paste", error),
        }
    }

    fn raw_input_command(&mut self, bytes: &[u8], stats: &mut WorkerCommandStats) {
        for byte in bytes {
            self.shell_prompt.input(matches!(*byte, b'\r' | b'\n'));
        }
        self.mark_input_fast_path();
        self.engine.scroll_viewport_bottom();
        stats.terminal_changed = true;
        write_pty(&self.pty_writer, bytes, &self.worker_health);
    }

    fn prompt_command(
        &mut self,
        text: Option<(u64, String, bool)>,
        done: WorkerRequest<std::result::Result<PromptSnapshot, String>>,
        stats: &mut WorkerCommandStats,
    ) {
        if !done.try_claim() {
            return;
        }
        // Consume already arrived output before evaluating the prompt lease.
        self.collect_pty();
        let drained = self.drain_pty();
        stats.terminal_changed |= drained.bytes > 0;
        self.publish_drain(drained);
        let allows = !self.pty_disconnected
            && self.pending_pty.is_empty()
            && self.engine.allows_prompt_editing();
        let result = (|| -> std::result::Result<PromptSnapshot, String> {
            if let Some((revision, text, submit)) = text {
                if text.is_empty()
                    || text.len() > 16 * 1024
                    || text
                        .chars()
                        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
                {
                    return Err(
                                    "Command must contain 1–16384 bytes with only LF and tab control characters"
                                        .to_owned(),
                                );
                }
                self.shell_prompt.claim(revision, allows)?;
                self.engine
                    .encode_paste_to_vec(&text, &mut self.output_buf)
                    .map_err(|error| error.to_string())?;
                if submit {
                    let mut enter = Vec::new();
                    self.engine
                        .encode_key_to_vec(
                            crate::terminal_input_model::KeyInput {
                                key: crate::terminal_input_model::TerminalKey::Enter,
                                mods: crate::terminal_input_model::KeyMods::default(),
                                repeat: false,
                                utf8: None,
                                unshifted: None,
                            },
                            &mut enter,
                        )
                        .map_err(|error| error.to_string())?;
                    self.output_buf.extend(enter);
                }
                let mut writer = self.pty_writer.lock().map_err(|error| error.to_string())?;
                writer
                    .write_all(&self.output_buf)
                    .and_then(|()| writer.flush())
                    .map_err(|error| error.to_string())?;
                drop(writer);
                self.engine.scroll_viewport_bottom();
                stats.terminal_changed = true;
            }
            Ok(self
                .shell_prompt
                .snapshot(self.engine.current_working_directory().to_owned(), allows))
        })();
        done.send(result);
    }

    fn copy_mode_command(
        &mut self,
        action: TerminalCopyModeAction,
        done: WorkerRequest<CopyModeActionResponse>,
        stats: &mut WorkerCommandStats,
    ) {
        if !done.try_claim() {
            return;
        }
        self.mark_input_fast_path();
        let response = self
            .engine
            .handle_copy_mode_action(action)
            .map_err(|error| error.to_string());
        stats.terminal_changed = true;
        done.send(response);
    }

    fn search_command(
        &mut self,
        options: TerminalSearchOptions,
        query: &str,
        direction: TerminalSearchDirection,
        done: WorkerRequest<SearchViewportResponse>,
    ) {
        if !done.try_claim() {
            return;
        }
        let response = self
            .engine
            .search_viewport_with_options(query, direction, options)
            .and_then(|found| {
                let frame = self.engine.extract_frame()?;
                self.latest_frame.publish(frame)?;
                self.cursor_hold.reset();
                Ok(found)
            })
            .map_err(|error| error.to_string());
        if response.is_ok() {
            self.force_next_frame_publish = false;
            self.has_unpublished_frame = false;
            (self.repaint_wakeup)();
        }
        done.send(response);
    }

    fn resize_pty(&mut self, pty_size: PtySize) {
        if pty_size == self.pty_size {
            return;
        }
        match self.pty_master.resize(pty_size) {
            Ok(()) => self.pty_size = pty_size,
            Err(error) => self.worker_health.record("resize PTY pixels", error),
        }
    }

    fn resize(&mut self, geometry: TerminalGeometry, pty_size: PtySize) -> Result<()> {
        let previous = self.engine.geometry();
        let previous_pty_size = self.pty_size;
        self.pty_master.resize(pty_size)?;
        if let Err(error) = self.engine.resize(geometry) {
            let engine_rollback = self.engine.resize(previous).err();
            let pty_rollback = self.pty_master.resize(previous_pty_size).err();
            if engine_rollback.is_none() && pty_rollback.is_none() {
                return Err(error);
            }

            let details = [
                engine_rollback.map(|error| format!("engine rollback: {error}")),
                pty_rollback.map(|error| format!("PTY rollback: {error}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            return Err(error.context(format!("resize rollback failed: {details}")));
        }
        self.pty_size = pty_size;
        Ok(())
    }

    fn discard_pending_output_queue(&mut self) {
        self.pending_pty.clear();
        loop {
            match self.pty_rx.try_recv() {
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pty_disconnected = true;
                    break;
                }
            }
        }
        self.pending_pty_len.store(0, Ordering::Relaxed);
        self.has_unpublished_frame = false;
        self.sync_output_batch_pending = false;
        self.deferred_sync_publish = false;
        self.last_terminal_change = None;
        self.cursor_hold.reset();
    }

    fn collect_pty(&mut self) -> bool {
        // Reset before collecting: a concurrent read either belongs to this
        // batch or leaves a notification for the next one, including before sleep.
        self.pty_wakeup_pending.store(false, Ordering::Release);
        let mut did_work = false;
        let mut collected_bytes = 0_usize;
        let mut collected_chunks = 0_usize;
        while collected_chunks < MAX_COLLECT_CHUNKS_PER_TICK
            && self.pending_pty.len() < MAX_PENDING_PTY_BYTES
        {
            let bytes = match self.pty_rx.try_recv() {
                Ok(bytes) => bytes,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pty_disconnected = true;
                    break;
                }
            };
            let bytes_len = bytes.len();
            did_work = true;
            collected_bytes = collected_bytes.saturating_add(bytes_len);
            collected_chunks = collected_chunks.saturating_add(1);
            self.pending_pty.push_back(bytes);
            self.trace_event(
                "pty_read",
                &[
                    ("bytes", TraceValue::Usize(bytes_len)),
                    (
                        "pending_pty_bytes",
                        TraceValue::Usize(self.pending_pty.len()),
                    ),
                ],
            );
        }
        if did_work {
            self.pending_pty_len
                .store(self.pending_pty.len(), Ordering::Relaxed);
            self.trace_event(
                "pty_collect_done",
                &[
                    ("bytes", TraceValue::Usize(collected_bytes)),
                    ("chunks", TraceValue::Usize(collected_chunks)),
                    (
                        "pending_pty_bytes",
                        TraceValue::Usize(self.pending_pty.len()),
                    ),
                ],
            );
        }
        did_work
    }

    fn drain_pty(&mut self) -> DrainStats {
        let pending_before = self.pending_pty.len();
        if pending_before > 0 {
            self.trace_event(
                "parse_start",
                &[("pending_pty_bytes", TraceValue::Usize(pending_before))],
            );
        }
        let engine = &mut self.engine;
        let worker_health = Arc::clone(&self.worker_health);
        let mut observed_sync_output = synchronized_output_state(engine, &worker_health);
        let mut write = |bytes: &[u8]| {
            engine.write_vt(bytes);
            observed_sync_output |= engine.take_synchronized_output_observed();
            observed_sync_output |= synchronized_output_state(engine, &worker_health);
        };
        let stats = if self.force_next_frame_publish {
            drain_pty_backlog_with_limits(
                &mut self.pending_pty,
                INPUT_FAST_PATH_DRAIN_BYTES,
                INPUT_FAST_PATH_DRAIN_CHUNKS,
                INPUT_FAST_PATH_DRAIN_TIME_US,
                &mut write,
            )
        } else {
            drain_pty_backlog(&mut self.pending_pty, &mut write)
        };
        self.sync_output_batch_pending |= observed_sync_output;
        if stats.bytes > 0 {
            self.publish_current_working_directory();
            self.trace_event(
                "parse_done",
                &[
                    ("bytes", TraceValue::Usize(stats.bytes)),
                    ("chunks", TraceValue::Usize(stats.chunks)),
                    ("elapsed_us", TraceValue::U64(stats.elapsed_us)),
                    (
                        "pending_pty_bytes",
                        TraceValue::Usize(self.pending_pty.len()),
                    ),
                ],
            );
        }
        self.forward_side_effects();

        if self.pending_pty.len() != pending_before {
            self.pending_pty_len
                .store(self.pending_pty.len(), Ordering::Relaxed);
        }
        stats
    }

    fn publish_current_working_directory(&self) {
        let cwd = self.engine.current_working_directory();
        let next = (!cwd.is_empty()).then(|| cwd.to_owned());
        if let Ok(mut current) = self.current_working_directory.lock()
            && *current != next
        {
            *current = next;
        }
    }

    fn forward_side_effects(&mut self) {
        let effects = self.engine.drain_side_effects();
        for effect in &effects {
            match effect {
                crate::terminal_side_effect::TerminalSideEffect::ShellLifecycle(event) => {
                    self.shell_prompt.lifecycle(*event);
                }
                crate::terminal_side_effect::TerminalSideEffect::ShellPrompt(report) => {
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    self.shell_prompt.report(
                        report,
                        self.engine.current_working_directory(),
                        timestamp,
                    );
                }
                _ => {}
            }
        }
        deliver_terminal_side_effects(&mut self.side_effect_tx, &self.side_effect_pane_id, effects);
    }

    fn publish_drain(&self, stats: DrainStats) {
        if let Ok(mut latest) = self.latest_drain.lock() {
            latest.chunks = latest.chunks.saturating_add(stats.chunks);
            latest.bytes = latest.bytes.saturating_add(stats.bytes);
            latest.elapsed_us = latest.elapsed_us.saturating_add(stats.elapsed_us);
        }
    }

    fn publish_frame(&mut self) {
        let trace = self.benchmark_trace.clone();
        let extract_start = Instant::now();
        let frame = match self.engine.extract_frame() {
            Ok(frame) => frame,
            Err(error) => {
                self.worker_health.record("extract_frame", error);
                self.acknowledge_resize();
                return;
            }
        };
        let extract_elapsed_us =
            u64::try_from(extract_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        if let Some(trace) = &trace {
            trace.emit(
                "frame_submitted",
                &[
                    ("cols", TraceValue::U64(u64::from(frame.cols))),
                    ("rows", TraceValue::U64(u64::from(frame.rows))),
                    ("cells", TraceValue::Usize(frame.stats.cells)),
                    ("chars", TraceValue::Usize(frame.stats.chars)),
                    ("dirty_rows", TraceValue::Usize(frame.stats.dirty_rows)),
                    ("extract_us", TraceValue::U64(extract_elapsed_us)),
                    (
                        "render_state_update_us",
                        TraceValue::U64(frame.stats.render_state_update_us),
                    ),
                    (
                        "frame_extraction_us",
                        TraceValue::U64(frame.stats.extraction_us),
                    ),
                    (
                        "image_placements",
                        TraceValue::Usize(frame.images.placements.len()),
                    ),
                    (
                        "virtual_placements",
                        TraceValue::Usize(frame.images.virtual_placements.len()),
                    ),
                ],
            );
        }
        let shown = self.cursor_hold.resolve(frame.cursor, Instant::now());
        if let Err(error) = self.latest_frame.publish_with_cursor(frame, shown) {
            self.worker_health.record("publish_frame", error);
            self.acknowledge_resize();
            return;
        }
        if let Some(trace) = &trace {
            trace.emit(
                "frame_presented",
                &[("presenter", TraceValue::Str("published_frame"))],
            );
        }
        self.force_next_frame_publish = false;
        self.has_unpublished_frame = false;
        self.deferred_sync_publish = false;
        self.settle.reset();
        self.acknowledge_resize();
        (self.repaint_wakeup)();
    }

    fn acknowledge_resize(&mut self) {
        if let Some(done) = self.pending_resize_ack.take() {
            done.send(());
        }
    }

    fn trace_event(&self, event: &str, fields: &[(&str, TraceValue<'_>)]) {
        if let Some(trace) = &self.benchmark_trace {
            trace.emit(event, fields);
        }
    }

    fn write_output_buf(&self) {
        if !self.output_buf.is_empty() {
            write_pty(&self.pty_writer, &self.output_buf, &self.worker_health);
        }
    }

    fn write_key_output(&self, input: KeyInput) {
        if self.output_buf.is_empty() {
            return;
        }
        if input.mods.command
            && let Some(tx) = &self.super_key_input_tx
            && tx.send(self.output_buf.clone()).is_ok()
        {
            return;
        }
        self.write_output_buf();
    }
}

fn synchronized_output_state(engine: &TerminalEngine, health: &WorkerHealth) -> bool {
    match engine.is_synchronized_output() {
        Ok(active) => active,
        Err(error) => {
            health.record("synchronized_output", error);
            false
        }
    }
}

fn write_pty(writer: &Arc<Mutex<Box<dyn Write + Send>>>, bytes: &[u8], health: &WorkerHealth) {
    let Ok(mut writer) = writer.lock() else {
        health.record("pty_write_lock", "writer lock poisoned");
        return;
    };
    if let Err(error) = writer.write_all(bytes) {
        health.record("pty_write_all", error);
        return;
    }
    if let Err(error) = writer.flush() {
        health.record("pty_flush", error);
    }
}

// DEC mode 2026 (synchronized output): applications wrap multi-step redraws
// in BSU/ESU so intermediate states (e.g. a cleared screen before a tmux
// layout repaint) never reach the display. The grace period bounds a client
// that sets the mode and dies without clearing it.
#[must_use]
pub fn sync_output_suppresses_publish(
    sync_output_active: bool,
    sync_output_observed_in_batch: bool,
    elapsed_since_sync_start: Duration,
) -> bool {
    sync_output_observed_in_batch
        || (sync_output_active && elapsed_since_sync_start < SYNC_OUTPUT_MAX_SUPPRESS)
}

/// Why a frame that is otherwise ready is being held back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishHold {
    None,
    /// DEC 2026 batch still open: nothing publishes, not even forced input echo.
    SyncOutput,
    /// Output is still arriving; only the hold cap overrides this.
    Settling,
}

/// Tracks how long the quiet window has been holding a frame. Reset on publish.
#[derive(Debug, Default)]
pub struct OutputSettle {
    since: Option<Instant>,
}

impl OutputSettle {
    /// Whether to keep holding; the hold clock starts the first time this says yes.
    pub fn holds(&mut self, since_last_terminal_change: Duration) -> bool {
        let hold = output_settling(
            since_last_terminal_change,
            self.since
                .map_or(Duration::ZERO, |instant| instant.elapsed()),
        );
        if hold {
            self.since.get_or_insert_with(Instant::now);
        }
        hold
    }

    pub const fn reset(&mut self) {
        self.since = None;
    }
}

/// Whether output is still settling: not yet quiet for [`WORKER_OUTPUT_QUIET`], unless the hold
/// has already lasted [`WORKER_OUTPUT_HOLD_MAX`].
#[must_use]
pub fn output_settling(since_last_terminal_change: Duration, held_for: Duration) -> bool {
    since_last_terminal_change < WORKER_OUTPUT_QUIET && held_for < WORKER_OUTPUT_HOLD_MAX
}

/// Holds the visible cursor position across the brief window an app leaves it parked on a scratch
/// cell mid-redraw. Content publishes immediately; only the cursor waits.
///
/// A move that stays on the committed row is shown at once: that is the caret advancing as you
/// type, and it must never lag. A move to a different row is held until it survives
/// [`CURSOR_COMMIT_DELAY`] of output; an app that parks its cursor on a status row and moves it
/// home a few milliseconds later never shows the park. Judging by row rather than by whether a
/// keystroke is in flight is what keeps a keystroke from committing a status park that happens to
/// be the live cursor at that instant. Visibility changes commit immediately.
#[derive(Debug, Default)]
pub struct CursorHold {
    initialized: bool,
    committed: Option<CursorSnapshot>,
    pending: Option<(CursorSnapshot, Instant)>,
}

impl CursorHold {
    /// The cursor to publish for this frame, updating internal state.
    pub fn resolve(
        &mut self,
        live: Option<CursorSnapshot>,
        now: Instant,
    ) -> Option<CursorSnapshot> {
        if !self.initialized {
            self.initialized = true;
            self.committed = live;
            self.pending = None;
            return live;
        }
        match (self.committed, live) {
            (Some(committed), Some(live)) if committed.y != live.y => {
                match self.pending {
                    Some((pending, since))
                        if pending.y == live.y
                            && now.duration_since(since) >= CURSOR_COMMIT_DELAY =>
                    {
                        self.committed = Some(live);
                        self.pending = None;
                        Some(live)
                    }
                    Some((pending, since)) if pending.y == live.y => {
                        // The move to this row is still settling; keep the original timestamp but
                        // refresh the position so a same-row shift while waiting is not lost.
                        self.pending = Some((live, since));
                        self.committed
                    }
                    _ => {
                        self.pending = Some((live, now));
                        self.committed
                    }
                }
            }
            // Same row (the caret advancing as you type), a visibility change, or both hidden:
            // nothing jumps to an unrelated row, so commit immediately.
            _ => {
                self.committed = live;
                self.pending = None;
                live
            }
        }
    }

    /// Whether a held move has now survived long enough to publish on its own.
    #[must_use]
    pub fn commit_due(&self, now: Instant) -> bool {
        matches!(self.pending, Some((_, since)) if now.duration_since(since) >= CURSOR_COMMIT_DELAY)
    }

    /// Whether a move is currently held, so the worker should wake to commit it.
    #[must_use]
    pub const fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[must_use]
pub fn should_publish_frame_after_work(
    unpublished_frame: bool,
    force_next_frame_publish: bool,
    hold: PublishHold,
    pending_pty_bytes: usize,
    elapsed_since_last_terminal_change: Duration,
    elapsed_since_last_publish: Duration,
) -> bool {
    if !unpublished_frame {
        return false;
    }
    if hold != PublishHold::None {
        // Input echo skips the frame pacing below, not the quiet window: a keystroke's first
        // bytes back are often the cursor move ahead of the app's redraw.
        return false;
    }
    if force_next_frame_publish {
        return true;
    }
    if pending_pty_bytes > 0 {
        // A TUI repainting as it scrolls keeps a little output pending at all times, which pinned
        // publishing to the backlog interval and left content updating at ~15fps under a window
        // painting at 120. That interval is there to keep a flood (`cat` of a large file) from
        // starving the drain, so it applies once the backlog is actually flood-sized.
        let interval = if pending_pty_bytes >= WORKER_FLOOD_BACKLOG_BYTES {
            WORKER_BACKLOG_FRAME_INTERVAL
        } else {
            WORKER_READY_FRAME_INTERVAL
        };
        return elapsed_since_last_publish >= interval;
    }
    elapsed_since_last_publish >= WORKER_READY_FRAME_INTERVAL
        || elapsed_since_last_terminal_change >= WORKER_SETTLED_FRAME_DELAY
}
