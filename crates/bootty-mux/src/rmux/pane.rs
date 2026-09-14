use bootty_terminal::terminal_search::TerminalSearchOptions;
use bootty_terminal::{
    terminal_capture::{CaptureOptions, TerminalCapture},
    terminal_session::PendingWorkerResponse,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    DrainStats, OutputBacklog, TerminalSessionConfig, drain_output_backlog,
    drain_output_backlog_with_limits,
    frame_source::TerminalFrameSource,
    terminal_session::{
        CURSOR_COMMIT_DELAY, CursorHold, OutputSettle, PublishHold, WORKER_OUTPUT_QUIET,
        WorkerRequest, should_publish_frame_after_work, sync_output_suppresses_publish,
        worker_request,
    },
};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalEngine, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
        TerminalSideEffectEvent,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
    terminal_side_effect::deliver_terminal_side_effects,
};
use rmux_sdk::TerminalSizeSpec;
use tokio::sync::mpsc as tokio_mpsc;

use super::bridge::{rmux_missing_target_text, rmux_stale_target_text};
use super::pane_io::{RmuxPaneEvent, RmuxPaneIo, RmuxPaneTarget, open_rmux_pane_io};
use super::remote::{open_remote_rmux_pane_io, resize_remote_rmux_window};
use bootty_host::remote::RemoteHost;

use crate::terminal::{
    BackendPanePolicy, MuxPaneTarget, PaneLayoutResizeRequest, PaneStartRequest,
    ScopedMuxPaneTarget, TerminalRuntime,
};

const RMUX_MAX_PENDING_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const RMUX_INPUT_FAST_PATH_DRAIN_BYTES: usize = 64 * 1024;
const RMUX_INPUT_FAST_PATH_DRAIN_CHUNKS: usize = 8;
const RMUX_INPUT_FAST_PATH_DRAIN_TIME_US: u128 = 2_000;
const RMUX_MAX_COLLECT_BYTES_PER_TICK: usize = 4 * 1024 * 1024;
const RMUX_MAX_COLLECT_CHUNKS_PER_TICK: usize = 256;
const RMUX_PENDING_FRAME_WAIT: Duration = Duration::from_millis(8);
const RMUX_INITIAL_FRAME_AGE: Duration = Duration::from_millis(16);

struct RmuxNativeTerminal {
    command_tx: tokio_mpsc::UnboundedSender<RmuxTerminalCommand>,
    latest_frame: Arc<RmuxPublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output_len: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    error_rx: mpsc::Receiver<String>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    needs_initial_resize: bool,
}

struct RmuxPublishedFrame {
    latest: Mutex<Arc<RenderFrame>>,
}

impl RmuxPublishedFrame {
    fn new() -> Self {
        Self {
            latest: Mutex::new(Arc::new(RenderFrame::default())),
        }
    }

    fn load(&self) -> Result<Arc<RenderFrame>> {
        self.latest
            .lock()
            .map(|latest| Arc::clone(&latest))
            .map_err(|_| anyhow::anyhow!("rmux frame cache lock poisoned"))
    }

    fn publish(&self, frame: RenderFrame) -> Result<()> {
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("rmux frame cache lock poisoned"))?;
        *latest = Arc::new(frame);
        drop(latest);
        Ok(())
    }
}

enum RmuxTerminalCommand {
    DisplayScale(f32),
    RenderCellMetrics(CellMetrics),
    Resize(TerminalGeometry),
    ForceResize,
    ApplyLiveConfig(TerminalLiveConfig),
    Key(KeyInput),
    Focus(bool),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
    Paste(String),
    InputBytes(Vec<u8>),
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
    Capture {
        options: CaptureOptions,
        done: WorkerRequest<std::result::Result<TerminalCapture, String>>,
    },
    FormatSelection {
        format: TerminalSelectionFormat,
        done: WorkerRequest<std::result::Result<Option<Vec<u8>>, String>>,
    },
    CopyModeActive {
        done: WorkerRequest<std::result::Result<bool, String>>,
    },
    CopyModeAction {
        action: TerminalCopyModeAction,
        done: WorkerRequest<std::result::Result<TerminalCopyModeOutcome, String>>,
    },
    SearchViewport {
        options: TerminalSearchOptions,
        query: String,
        direction: TerminalSearchDirection,
        done: WorkerRequest<std::result::Result<bool, String>>,
    },
    IsMouseTracking {
        done: WorkerRequest<std::result::Result<bool, String>>,
    },
    DiscardPendingOutput {
        done: WorkerRequest<std::result::Result<(), String>>,
    },
    Stop,
}

struct RmuxWorkerConfig {
    pane_io: RmuxPaneIo,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    terminal_config: TerminalSessionConfig,
    command_rx: tokio_mpsc::UnboundedReceiver<RmuxTerminalCommand>,
    latest_frame: Arc<RmuxPublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output_len: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    error_tx: mpsc::Sender<String>,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    waiting_initial_remote_frame: bool,
}
struct RmuxWorkerClosedGuard(Arc<AtomicBool>);

impl Drop for RmuxWorkerClosedGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "publication, synchronized output, and channel lifetime flags are independent"
)]
struct RmuxWorker {
    pane_io: RmuxPaneIo,
    geometry: TerminalGeometry,
    engine: TerminalEngine,
    command_rx: tokio_mpsc::UnboundedReceiver<RmuxTerminalCommand>,
    pending_command: Option<RmuxTerminalCommand>,
    pending_event: Option<RmuxPaneEvent>,
    input_results_closed: bool,
    latest_frame: Arc<RmuxPublishedFrame>,
    latest_drain: Arc<Mutex<DrainStats>>,
    pending_output: OutputBacklog,
    pending_output_len: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    error_tx: mpsc::Sender<String>,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    side_effect_tx: Option<mpsc::Sender<TerminalSideEffectEvent>>,
    side_effect_pane_id: Option<String>,
    output_buf: Vec<u8>,
    last_frame_publish: Instant,
    has_unpublished_frame: bool,
    force_next_frame_publish: bool,
    sync_output_since: Option<Instant>,
    sync_output_batch_pending: bool,
    deferred_sync_publish: bool,
    last_terminal_change: Option<Instant>,
    settle: OutputSettle,
    last_hold: PublishHold,
    cursor_hold: CursorHold,
    waiting_initial_remote_frame: bool,
    command_disconnected: bool,
    output_closed: bool,
}

impl RmuxNativeTerminal {
    fn new(
        target: &MuxPaneTarget,
        remote: Option<&RemoteHost>,
        geometry: TerminalGeometry,
        display_scale: f32,
        render_cell: CellMetrics,
        config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<Self> {
        let pane_target = RmuxPaneTarget::new(
            target.session_id().to_owned(),
            match target {
                MuxPaneTarget::Pane { pane_id, .. } => Some(pane_id.clone()),
                MuxPaneTarget::Session { .. } => None,
            },
        );
        // A remote pane uses the other host's embedded Bootty rmux protocol.
        let pane_io = match remote {
            Some(remote) => open_remote_rmux_pane_io(remote, &pane_target)?,
            None => open_rmux_pane_io(pane_target)?,
        };
        let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
        let (error_tx, error_rx) = mpsc::channel();
        let latest_frame = Arc::new(RmuxPublishedFrame::new());
        let latest_drain = Arc::new(Mutex::new(DrainStats::default()));
        let pending_output_len = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        spawn_rmux_terminal_worker(RmuxWorkerConfig {
            pane_io,
            geometry,
            display_scale,
            render_cell,
            terminal_config: config,
            command_rx,
            latest_frame: Arc::clone(&latest_frame),
            latest_drain: Arc::clone(&latest_drain),
            pending_output_len: Arc::clone(&pending_output_len),
            closed: Arc::clone(&closed),
            error_tx,
            repaint_wakeup,
            waiting_initial_remote_frame: true,
        })?;
        Ok(Self {
            command_tx,
            latest_frame,
            latest_drain,
            pending_output_len,
            closed,
            error_rx,
            geometry,
            display_scale,
            render_cell,
            needs_initial_resize: true,
        })
    }

    fn send_command(&self, command: RmuxTerminalCommand) -> Result<()> {
        self.check_worker_error()?;
        // Topology reconciliation can deliver focus, layout, or input updates after close.
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        if self.command_tx.send(command).is_err() {
            // The worker may have closed between the check and the send. Report its actual
            // failure first; only a known closure makes the disconnected queue harmless.
            self.check_worker_error()?;
            anyhow::ensure!(
                self.closed.load(Ordering::Relaxed),
                "rmux terminal worker stopped"
            );
        }
        Ok(())
    }

    fn request<T>(
        &self,
        operation: &'static str,
        build: impl FnOnce(WorkerRequest<std::result::Result<T, String>>) -> RmuxTerminalCommand,
    ) -> Result<T> {
        self.check_worker_error()?;
        let (done, response) = worker_request();
        self.command_tx
            .send(build(done))
            .map_err(|_| anyhow::anyhow!("rmux terminal worker stopped"))?;
        response
            .receive(operation)?
            .map_err(|error| anyhow::anyhow!(error))
    }

    fn check_worker_error(&self) -> Result<()> {
        let mut error = None;
        while let Ok(next) = self.error_rx.try_recv() {
            error = Some(next);
        }
        if let Some(error) = error {
            anyhow::bail!(error);
        }
        Ok(())
    }

    fn take_drain_stats(&self) -> DrainStats {
        let Ok(mut stats) = self.latest_drain.lock() else {
            return DrainStats::default();
        };
        let drained = *stats;
        *stats = DrainStats::default();
        drained
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RmuxWindowResizeRequest {
    window_id: String,
    cols: u16,
    rows: u16,
}

struct RmuxWindowResizeWorker {
    tx: mpsc::Sender<RmuxWindowResizeRequest>,
    result_rx: mpsc::Receiver<(RmuxWindowResizeRequest, std::result::Result<(), String>)>,
}

/// How long to wait before re-driving a resize at a window the daemon will not
/// resolve. Re-driving on every paint would enumerate every session at frame
/// rate, and giving up entirely would leave a window that comes back listed at
/// the wrong size forever, so back off to this instead.
const UNRESOLVED_WINDOW_RESIZE_RETRY: Duration = Duration::from_millis(500);

pub struct RmuxPanePolicy {
    remote: Option<RemoteHost>,
    window_sizes: BTreeMap<String, (u16, u16)>,
    unresolved_window_resize_at: BTreeMap<String, Instant>,
    resize_worker: Option<RmuxWindowResizeWorker>,
}

impl RmuxPanePolicy {
    #[must_use]
    pub const fn new(remote: Option<RemoteHost>) -> Self {
        Self {
            remote,
            window_sizes: BTreeMap::new(),
            unresolved_window_resize_at: BTreeMap::new(),
            resize_worker: None,
        }
    }

    fn ensure_resize_worker(&mut self, repaint_wakeup: &Arc<dyn Fn() + Send + Sync + 'static>) {
        if self.resize_worker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<RmuxWindowResizeRequest>();
        let (result_tx, result_rx) = mpsc::channel();
        let repaint = Arc::clone(repaint_wakeup);
        let remote = self.remote.clone();
        thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                // Coalesce within each window; another visible window must not lose its resize.
                let mut pending = BTreeMap::from([(request.window_id.clone(), request)]);
                for request in rx.try_iter() {
                    pending.insert(request.window_id.clone(), request);
                }
                for request in pending.into_values() {
                    let result = match remote.as_ref() {
                        Some(remote) => resize_remote_rmux_window(
                            remote,
                            &request.window_id,
                            request.cols,
                            request.rows,
                        ),
                        None => super::backend::resize_bootty_rmux_window(
                            &request.window_id,
                            request.cols,
                            request.rows,
                        ),
                    }
                    .map_err(|error| error.to_string());
                    let _ = result_tx.send((request, result));
                    repaint();
                }
            }
        });
        self.resize_worker = Some(RmuxWindowResizeWorker { tx, result_rx });
    }

    fn drain_resize_results(&mut self) -> Result<bool> {
        let mut completed = false;
        let mut errors = BTreeMap::new();
        if let Some(worker) = &self.resize_worker {
            for (request, result) in worker.result_rx.try_iter() {
                let current = self.window_sizes.get(&request.window_id)
                    == Some(&(request.cols, request.rows));
                match result {
                    Ok(()) => {
                        completed = true;
                        errors.remove(&request.window_id);
                        self.unresolved_window_resize_at.remove(&request.window_id);
                    }
                    Err(error) if current => {
                        if rmux_stale_target_text(&error) {
                            self.unresolved_window_resize_at
                                .insert(request.window_id.clone(), Instant::now());
                            self.window_sizes.remove(&request.window_id);
                        } else if !rmux_missing_target_text(&error) {
                            self.window_sizes.remove(&request.window_id);
                            errors.insert(request.window_id, error);
                        }
                    }
                    Err(_) => {}
                }
            }
        }
        if let Some((_, error)) = errors.into_iter().next() {
            anyhow::bail!(error);
        }
        Ok(completed)
    }
}

impl BackendPanePolicy for RmuxPanePolicy {
    fn remote_target(&self) -> Option<crate::RemoteTarget> {
        self.remote.as_ref().map(RemoteHost::target)
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        let mut config = request.terminal_config.clone();
        config.side_effect_pane_id = request.target.side_effect_pane_id();
        Ok(Some(Box::new(RmuxNativeTerminal::new(
            request.target.mux_target(),
            self.remote.as_ref(),
            request.spawn_geometry,
            request.display_scale,
            request.render_cell,
            config,
            Arc::clone(request.repaint_wakeup),
        )?)))
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        let completed = self.drain_resize_results()?;
        let Some(window_id) = request.window_id else {
            return Ok(completed);
        };
        let requested = (request.cols, request.rows);
        if self.window_sizes.get(window_id) == Some(&requested)
            || self
                .unresolved_window_resize_at
                .get(window_id)
                .is_some_and(|at| at.elapsed() < UNRESOLVED_WINDOW_RESIZE_RETRY)
        {
            return Ok(completed);
        }
        self.ensure_resize_worker(request.repaint_wakeup);
        let Some(worker) = &self.resize_worker else {
            anyhow::bail!("rmux window resize worker did not start");
        };
        worker
            .tx
            .send(RmuxWindowResizeRequest {
                window_id: window_id.to_owned(),
                cols: request.cols,
                rows: request.rows,
            })
            .map_err(|_| anyhow::anyhow!("rmux window resize worker stopped"))?;
        self.window_sizes.insert(window_id.to_owned(), requested);
        Ok(completed)
    }

    fn deactivate(&mut self) {}
}

impl TerminalFrameSource for RmuxNativeTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        if (self.display_scale - display_scale).abs() <= f32::EPSILON {
            return Ok(());
        }
        self.display_scale = display_scale;
        self.send_command(RmuxTerminalCommand::DisplayScale(display_scale))
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        if self.render_cell == cell {
            return Ok(());
        }
        self.render_cell = cell;
        self.send_command(RmuxTerminalCommand::RenderCellMetrics(cell))
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.needs_initial_resize || self.geometry != geometry {
            self.geometry = geometry;
            self.needs_initial_resize = false;
            self.send_command(RmuxTerminalCommand::Resize(self.geometry))?;
        }
        self.check_worker_error()
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.check_worker_error()?;
        self.latest_frame.load()
    }
}

impl TerminalRuntime for RmuxNativeTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        // Keep worker errors for extract_frame, child_exited, or the next command to report.
        self.take_drain_stats()
    }

    fn pending_pty_len(&self) -> usize {
        self.pending_output_len.load(Ordering::Relaxed)
    }

    fn child_exited(&mut self) -> Result<bool> {
        self.check_worker_error()?;
        Ok(self.closed.load(Ordering::Relaxed))
    }

    fn tty_name(&self) -> Option<&str> {
        None
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        self.request("discarding output", |done| {
            RmuxTerminalCommand::DiscardPendingOutput { done }
        })
    }

    fn force_resize(&mut self) -> Result<()> {
        self.send_command(RmuxTerminalCommand::ForceResize)
    }

    fn capture(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        options.validate().map_err(anyhow::Error::msg)?;
        self.check_worker_error()?;
        let (done, response) = worker_request();
        self.command_tx
            .send(RmuxTerminalCommand::Capture { options, done })
            .map_err(|_| anyhow::anyhow!("rmux terminal worker stopped"))?;
        Ok(response)
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.request("formatting selection", |done| {
            RmuxTerminalCommand::FormatSelection { format, done }
        })
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.send_command(RmuxTerminalCommand::ApplyLiveConfig(config))
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.request("reporting mouse tracking", |done| {
            RmuxTerminalCommand::IsMouseTracking { done }
        })
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.send_command(RmuxTerminalCommand::MouseViewportScroll { delta })
    }

    fn scroll_viewport_to(&mut self, offset: usize) -> Result<()> {
        self.send_command(RmuxTerminalCommand::MouseViewportScrollTo { offset })
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.send_command(RmuxTerminalCommand::EnterCopyMode)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        self.request("reporting copy mode", |done| {
            RmuxTerminalCommand::CopyModeActive { done }
        })
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.request("handling copy mode action", |done| {
            RmuxTerminalCommand::CopyModeAction { action, done }
        })
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        self.search_viewport_with_options(query, direction, TerminalSearchOptions::default())
    }

    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<bool> {
        self.request("searching scrollback", |done| {
            RmuxTerminalCommand::SearchViewport {
                options,
                query: query.to_owned(),
                direction,
                done,
            }
        })
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(RmuxTerminalCommand::SelectionBegin(event))
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.send_command(RmuxTerminalCommand::SelectionUpdate(event))
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.send_command(RmuxTerminalCommand::SelectionEnd(event))
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            Ok(())
        } else {
            self.send_command(RmuxTerminalCommand::InputBytes(bytes.to_vec()))
        }
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Paste(text.to_owned()))
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Key(input))
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Focus(gained))
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.send_command(RmuxTerminalCommand::Mouse(input))
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.send_command(RmuxTerminalCommand::MouseWheel {
            input,
            scroll_delta,
        })
    }
}

impl Drop for RmuxNativeTerminal {
    fn drop(&mut self) {
        let _ = self.command_tx.send(RmuxTerminalCommand::Stop);
    }
}

fn spawn_rmux_terminal_worker(config: RmuxWorkerConfig) -> Result<()> {
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _closed_guard = RmuxWorkerClosedGuard(Arc::clone(&config.closed));
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = startup_tx.send(Err(error.to_string()));
                return;
            }
        };
        let mut engine = match TerminalEngine::new_with_terminal_options(
            config.geometry,
            config.terminal_config.colors,
            config.terminal_config.cursor,
            config.terminal_config.features,
            config.terminal_config.max_scrollback,
            config.terminal_config.macos_option_as_alt,
        ) {
            Ok(engine) => engine,
            Err(error) => {
                let _ = startup_tx.send(Err(error.to_string()));
                return;
            }
        };
        engine.set_display_scale(config.display_scale);
        engine.set_render_cell_metrics(config.render_cell);
        let callback_input = config.pane_io.input_tx.clone();
        if let Err(error) = engine.on_pty_write(move |_terminal, bytes| {
            // RMUX answers most terminal capability queries beside the pane PTY. Bootty alone
            // knows its configured palette and physical cell size, so return only replies RMUX
            // cannot own instead of duplicating delayed CSI replies into the shell.
            if is_osc_default_color_response(bytes)
                || is_xtwinops_pixel_size_response(bytes)
                || is_kitty_graphics_response(bytes)
                || is_sgr_pixel_mouse_mode_response(bytes)
            {
                let _ = callback_input.send(bytes.to_vec());
            }
        }) {
            let _ = startup_tx.send(Err(error.to_string()));
            return;
        }
        let worker = RmuxWorker {
            pane_io: config.pane_io,
            geometry: config.geometry,
            engine,
            command_rx: config.command_rx,
            latest_frame: config.latest_frame,
            latest_drain: config.latest_drain,
            pending_output: OutputBacklog::with_capacity(RMUX_MAX_COLLECT_CHUNKS_PER_TICK),
            pending_output_len: config.pending_output_len,
            closed: config.closed,
            error_tx: config.error_tx,
            repaint_wakeup: config.repaint_wakeup,
            side_effect_tx: config.terminal_config.side_effect_tx,
            side_effect_pane_id: config.terminal_config.side_effect_pane_id,
            output_buf: Vec::with_capacity(1024),
            last_frame_publish: Instant::now()
                .checked_sub(RMUX_INITIAL_FRAME_AGE)
                .unwrap_or_else(Instant::now),
            has_unpublished_frame: false,
            force_next_frame_publish: false,
            sync_output_since: None,
            sync_output_batch_pending: false,
            deferred_sync_publish: false,
            last_terminal_change: None,
            settle: OutputSettle::default(),
            last_hold: PublishHold::None,
            cursor_hold: CursorHold::default(),
            waiting_initial_remote_frame: config.waiting_initial_remote_frame,
            command_disconnected: false,
            pending_command: None,
            pending_event: None,
            input_results_closed: false,
            output_closed: false,
        };
        let _ = startup_tx.send(Ok(()));
        worker.run(&runtime);
    });

    startup_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("rmux terminal worker failed to start"))?
        .map_err(|error| anyhow::anyhow!(error))
}

fn is_osc_default_color_response(bytes: &[u8]) -> bool {
    [b"\x1b]10;rgb:", b"\x1b]11;rgb:", b"\x1b]12;rgb:"]
        .iter()
        .any(|prefix| bytes.starts_with(*prefix))
}

fn is_xtwinops_pixel_size_response(bytes: &[u8]) -> bool {
    [b"\x1b[4;".as_slice(), b"\x1b[6;".as_slice()]
        .iter()
        .any(|prefix| bytes.starts_with(prefix))
        && bytes.ends_with(b"t")
}

fn is_kitty_graphics_response(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b_Gi=") && bytes.ends_with(b"\x1b\\")
}

fn is_sgr_pixel_mouse_mode_response(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[?1016;") && bytes.ends_with(b"$y")
}

enum RmuxWake {
    Command(Option<RmuxTerminalCommand>),
    Output(Option<RmuxPaneEvent>),
    InputResult(Option<std::result::Result<(), String>>),
    Deadline,
}

async fn wait_for_rmux_work(
    commands: &mut tokio_mpsc::UnboundedReceiver<RmuxTerminalCommand>,
    pane_io: &mut RmuxPaneIo,
    output_closed: bool,
    input_results_closed: bool,
    delay: Option<Duration>,
) -> RmuxWake {
    tokio::select! {
        command = commands.recv() => RmuxWake::Command(command),
        event = pane_io.output_rx.recv(), if !output_closed => RmuxWake::Output(event),
        result = pane_io.result_rx.recv(), if !input_results_closed => RmuxWake::InputResult(result),
        () = async {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => RmuxWake::Deadline,
    }
}

impl RmuxWorker {
    fn run(mut self, runtime: &tokio::runtime::Runtime) {
        loop {
            let (mut did_work, mut terminal_changed) = self.process_commands();
            did_work |= self.collect_pane_output();
            let stats = self.drain_pending_output();
            terminal_changed |= stats.bytes > 0;
            did_work |= stats.bytes > 0;
            self.drain_input_results();
            self.forward_side_effects();

            if terminal_changed {
                self.mark_unpublished_frame();
            }
            // After the drain, not before: bytes trailing a completed sync batch belong in the
            // frame this publishes, not the one after it.
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
            if did_work {
                continue;
            }
            if self.should_stop() {
                break;
            }
            self.wait_for_work(runtime);
        }
    }

    fn wait_for_work(&mut self, runtime: &tokio::runtime::Runtime) {
        // Only incomplete publication has a deadline. Idle workers sleep until a channel wakes
        // them; output and command arrivals share the same cancellation-safe async wait.
        let delay = if self.has_unpublished_frame && self.last_hold == PublishHold::Settling {
            Some(WORKER_OUTPUT_QUIET)
        } else if self.cursor_hold.pending() {
            Some(CURSOR_COMMIT_DELAY)
        } else if self.has_unpublished_frame {
            Some(RMUX_PENDING_FRAME_WAIT)
        } else {
            None
        };
        // The VT engine stays on this OS thread; only channel waiting enters the runtime.
        match runtime.block_on(wait_for_rmux_work(
            &mut self.command_rx,
            &mut self.pane_io,
            self.output_closed,
            self.input_results_closed,
            delay,
        )) {
            RmuxWake::Command(command) => {
                self.pending_command = command;
                self.command_disconnected = self.pending_command.is_none();
            }
            RmuxWake::Output(event) => {
                self.pending_event = event;
                if self.pending_event.is_none() {
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                }
            }
            RmuxWake::InputResult(result) => match result {
                Some(Err(error)) => self.send_error(&error),
                Some(Ok(())) => {}
                None => self.input_results_closed = true,
            },
            RmuxWake::Deadline => {}
        }
    }

    fn process_commands(&mut self) -> (bool, bool) {
        let mut did_work = false;
        let mut terminal_changed = false;
        loop {
            let command = if let Some(command) = self.pending_command.take() {
                command
            } else {
                match self.command_rx.try_recv() {
                    Ok(command) => command,
                    Err(tokio_mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio_mpsc::error::TryRecvError::Disconnected) => {
                        self.command_disconnected = true;
                        break;
                    }
                }
            };
            did_work = true;
            terminal_changed |= self.apply_command(command);
            if self.command_disconnected {
                break;
            }
        }
        (did_work, terminal_changed)
    }

    fn apply_command(&mut self, command: RmuxTerminalCommand) -> bool {
        let mut terminal_changed = false;
        match command {
            RmuxTerminalCommand::DisplayScale(display_scale) => {
                self.engine.set_display_scale(display_scale);
                self.mark_unpublished_frame();
            }
            RmuxTerminalCommand::RenderCellMetrics(cell) => {
                self.engine.set_render_cell_metrics(cell);
                self.mark_unpublished_frame();
            }
            RmuxTerminalCommand::Resize(geometry) => {
                self.force_next_frame_publish = true;
                self.geometry = geometry;
                self.queue_resize(geometry);
                terminal_changed = self.engine.resize(geometry).is_ok();
            }
            RmuxTerminalCommand::ForceResize => {
                self.force_next_frame_publish = true;
                self.queue_resize(self.geometry);
                terminal_changed = true;
            }
            RmuxTerminalCommand::ApplyLiveConfig(config) => {
                match self.engine.apply_live_config(config) {
                    Ok(()) => terminal_changed = true,
                    Err(error) => self.send_error(&error),
                }
            }
            command @ (RmuxTerminalCommand::Key(_)
            | RmuxTerminalCommand::Focus(_)
            | RmuxTerminalCommand::Mouse(_)
            | RmuxTerminalCommand::MouseWheel { .. }
            | RmuxTerminalCommand::Paste(_)
            | RmuxTerminalCommand::InputBytes(_)
            | RmuxTerminalCommand::MouseViewportScroll { .. }
            | RmuxTerminalCommand::MouseViewportScrollTo { .. }) => {
                terminal_changed = self.apply_input_command(command);
            }
            RmuxTerminalCommand::EnterCopyMode => {
                terminal_changed |= self.apply_terminal_change(TerminalEngine::enter_copy_mode);
            }
            RmuxTerminalCommand::SelectionBegin(event) => {
                terminal_changed |=
                    self.apply_terminal_change(|engine| engine.begin_selection(event));
            }
            RmuxTerminalCommand::SelectionUpdate(event) => {
                terminal_changed |=
                    self.apply_terminal_change(|engine| engine.update_selection(event));
            }
            RmuxTerminalCommand::SelectionEnd(event) => {
                terminal_changed |=
                    self.apply_terminal_change(|engine| engine.end_selection(event));
            }
            RmuxTerminalCommand::Capture { options, done } => {
                self.respond(done, |worker| worker.engine.capture(options));
            }
            RmuxTerminalCommand::FormatSelection { format, done } => {
                self.respond(done, |worker| worker.engine.format_selection(format));
            }
            RmuxTerminalCommand::CopyModeActive { done } => {
                self.respond(done, |worker| Ok(worker.engine.copy_mode_active()));
            }
            RmuxTerminalCommand::CopyModeAction { action, done } => {
                terminal_changed = self.respond(done, |worker| {
                    worker.mark_input_fast_path();
                    worker.engine.handle_copy_mode_action(action)
                });
            }
            RmuxTerminalCommand::SearchViewport {
                options,
                query,
                direction,
                done,
            } => {
                terminal_changed = self.respond(done, |worker| {
                    worker.mark_input_fast_path();
                    worker
                        .engine
                        .search_viewport_with_options(&query, direction, options)
                });
            }
            RmuxTerminalCommand::IsMouseTracking { done } => {
                self.respond(done, |worker| worker.engine.is_mouse_tracking());
            }
            RmuxTerminalCommand::DiscardPendingOutput { done } => {
                self.respond(done, |worker| {
                    worker.discard_pending_output();
                    Ok(())
                });
            }
            RmuxTerminalCommand::Stop => {
                self.command_disconnected = true;
            }
        }
        terminal_changed
    }

    fn apply_input_command(&mut self, command: RmuxTerminalCommand) -> bool {
        let mut terminal_changed = false;
        match command {
            RmuxTerminalCommand::MouseViewportScroll { delta } => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_delta(delta);
                terminal_changed = true;
            }
            RmuxTerminalCommand::MouseViewportScrollTo { offset } => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_to(offset);
                terminal_changed = true;
            }
            RmuxTerminalCommand::Key(input) => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_bottom();
                terminal_changed = true;
                self.encode_output(|engine, out| engine.encode_key_to_vec(input, out));
            }
            RmuxTerminalCommand::Focus(gained) => {
                self.mark_input_fast_path();
                self.encode_output(|engine, out| engine.encode_focus_to_vec(gained, out));
            }
            RmuxTerminalCommand::Mouse(input) => {
                self.mark_input_fast_path();
                self.encode_output(|engine, out| engine.encode_mouse_to_vec(input, out));
            }
            RmuxTerminalCommand::MouseWheel {
                input,
                scroll_delta,
            } => match self.engine.is_mouse_tracking() {
                Ok(true) => {
                    self.mark_input_fast_path();
                    self.encode_output(|engine, out| {
                        engine.encode_mouse_wheel_to_vec(
                            input,
                            scroll_delta.unsigned_abs().max(1),
                            out,
                        )
                    });
                }
                Ok(false) if scroll_delta != 0 => {
                    self.mark_input_fast_path();
                    self.engine.scroll_viewport_delta(scroll_delta);
                    terminal_changed = true;
                }
                Ok(false) => {}
                Err(error) => self.send_error(&error),
            },
            RmuxTerminalCommand::Paste(text) => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_bottom();
                terminal_changed = true;
                self.encode_output(|engine, out| engine.encode_paste_to_vec(&text, out));
            }
            RmuxTerminalCommand::InputBytes(bytes) => {
                self.mark_input_fast_path();
                self.engine.scroll_viewport_bottom();
                terminal_changed = true;
                self.queue_input(&bytes);
            }
            _ => {}
        }
        terminal_changed
    }

    fn discard_pending_output(&mut self) {
        self.pending_output.clear();
        self.pending_output_len.store(0, Ordering::Relaxed);
        self.has_unpublished_frame = false;
        self.sync_output_batch_pending = false;
        self.deferred_sync_publish = false;
    }

    fn collect_pane_output(&mut self) -> bool {
        let mut did_work = false;
        let mut collected_bytes = 0;
        let mut collected_chunks = 0;
        while collected_chunks < RMUX_MAX_COLLECT_CHUNKS_PER_TICK
            && collected_bytes < RMUX_MAX_COLLECT_BYTES_PER_TICK
            && self.total_pending_output_len() < RMUX_MAX_PENDING_OUTPUT_BYTES
        {
            let event = match self
                .pending_event
                .take()
                .map_or_else(|| self.pane_io.output_rx.try_recv(), Ok)
            {
                Ok(event) => event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
            };
            did_work = true;
            match event {
                RmuxPaneEvent::Rebase(keyframe) => {
                    // A rebase supersedes every byte queued from the previous
                    // epoch. Feed its reset-and-reconstruct keyframe as one
                    // authoritative emulator transition before accepting the
                    // following epoch's bytes.
                    self.pending_output.clear();
                    self.update_pending_output_len();
                    self.sync_output_batch_pending = false;
                    self.deferred_sync_publish = false;
                    self.cursor_hold.reset();
                    collected_chunks = collected_chunks.saturating_add(1);
                    collected_bytes = collected_bytes.saturating_add(keyframe.len());
                    self.engine.write_vt_without_pty_responses(&keyframe);
                    self.engine.scroll_viewport_bottom();
                    self.waiting_initial_remote_frame = false;
                    self.force_next_frame_publish = true;
                    self.mark_unpublished_frame();
                }
                RmuxPaneEvent::Bytes(bytes) => {
                    collected_chunks = collected_chunks.saturating_add(1);
                    collected_bytes = collected_bytes.saturating_add(bytes.len());
                    self.pending_output.push_back(bytes);
                    self.update_pending_output_len();
                }
                RmuxPaneEvent::ProcessExited => {}
                RmuxPaneEvent::End(error) => {
                    if let Some(reason) = error {
                        self.send_error(&format!("rmux pane output ended: {reason}"));
                    }
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
                RmuxPaneEvent::Error(error) => {
                    self.send_error(&error);
                    self.output_closed = true;
                    self.closed.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
        did_work
    }

    const fn total_pending_output_len(&self) -> usize {
        self.pending_output.len()
    }

    fn update_pending_output_len(&self) {
        self.pending_output_len
            .store(self.total_pending_output_len(), Ordering::Relaxed);
    }

    fn drain_pending_output(&mut self) -> DrainStats {
        let engine = &mut self.engine;
        let mut observed_sync_output = engine.is_synchronized_output().unwrap_or(false);
        let mut write = |bytes: &[u8]| {
            engine.write_vt(bytes);
            observed_sync_output |= engine.take_synchronized_output_observed();
            observed_sync_output |= engine.is_synchronized_output().unwrap_or(false);
        };
        let stats = if self.force_next_frame_publish {
            drain_output_backlog_with_limits(
                &mut self.pending_output,
                RMUX_INPUT_FAST_PATH_DRAIN_BYTES,
                RMUX_INPUT_FAST_PATH_DRAIN_CHUNKS,
                RMUX_INPUT_FAST_PATH_DRAIN_TIME_US,
                &mut write,
            )
        } else {
            drain_output_backlog(&mut self.pending_output, &mut write)
        };
        self.sync_output_batch_pending |= observed_sync_output;
        if stats.bytes > 0 {
            self.update_pending_output_len();
        }
        stats
    }

    fn drain_input_results(&mut self) {
        while let Ok(result) = self.pane_io.result_rx.try_recv() {
            if let Err(error) = result {
                self.send_error(&error);
            }
        }
    }

    fn forward_side_effects(&mut self) {
        deliver_terminal_side_effects(
            &mut self.side_effect_tx,
            &self.side_effect_pane_id,
            self.engine.drain_side_effects(),
        );
    }

    fn publish_drain(&self, stats: DrainStats) {
        if let Ok(mut latest) = self.latest_drain.lock() {
            latest.chunks = latest.chunks.saturating_add(stats.chunks);
            latest.bytes = latest.bytes.saturating_add(stats.bytes);
            latest.elapsed_us = latest.elapsed_us.saturating_add(stats.elapsed_us);
        }
    }

    fn should_publish_frame(&mut self) -> bool {
        if self.waiting_initial_remote_frame {
            return false;
        }
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
            self.total_pending_output_len(),
            self.last_terminal_change
                .map_or(Duration::ZERO, |instant| instant.elapsed()),
            self.last_frame_publish.elapsed(),
        ) || self.cursor_hold.commit_due(Instant::now())
    }

    fn sync_output_suppressed(&mut self) -> bool {
        let active = self.engine.is_synchronized_output().unwrap_or(false);
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

    fn settling(&mut self) -> bool {
        self.settle.holds(
            self.last_terminal_change
                .map_or(Duration::ZERO, |instant| instant.elapsed()),
        )
    }

    fn publish_deferred_sync_frame(&mut self) {
        if !self.deferred_sync_publish || !self.has_unpublished_frame {
            return;
        }
        if self.engine.is_synchronized_output().unwrap_or(false) || self.settling() {
            return;
        }
        self.publish_frame();
        self.last_frame_publish = Instant::now();
    }

    fn publish_frame(&mut self) {
        let Ok(frame) = self.engine.extract_frame() else {
            return;
        };
        let shown = self.cursor_hold.resolve(frame.cursor, Instant::now());
        let mut published = frame.clone();
        published.cursor = shown;
        if self.latest_frame.publish(published).is_ok() {
            self.force_next_frame_publish = false;
            self.has_unpublished_frame = false;
            self.deferred_sync_publish = false;
            self.settle.reset();
            (self.repaint_wakeup)();
        }
    }

    fn mark_unpublished_frame(&mut self) {
        self.has_unpublished_frame = true;
        self.last_terminal_change = Some(Instant::now());
    }

    const fn mark_input_fast_path(&mut self) {
        self.waiting_initial_remote_frame = false;
        self.force_next_frame_publish = true;
    }

    const fn should_stop(&self) -> bool {
        self.command_disconnected || (self.output_closed && self.total_pending_output_len() == 0)
    }

    fn queue_resize(&mut self, geometry: TerminalGeometry) {
        if self
            .pane_io
            .resize_tx
            .send(TerminalSizeSpec::new(geometry.cols, geometry.rows))
            .is_err()
        {
            // The pane stream can close before a queued layout resize reaches
            // this worker. That is normal pane teardown, not a terminal error.
            self.output_closed = true;
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    fn queue_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.pane_io.input_tx.send(bytes.to_vec()).is_err() {
            // Input can race pane/session close in the same way as resize.
            self.output_closed = true;
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    fn write_output_buf(&mut self) {
        if self.output_buf.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.output_buf);
        self.queue_input(&bytes);
    }

    fn encode_output(
        &mut self,
        encode: impl FnOnce(&mut TerminalEngine, &mut Vec<u8>) -> Result<()>,
    ) {
        if encode(&mut self.engine, &mut self.output_buf).is_ok() {
            self.write_output_buf();
        }
    }

    fn apply_terminal_change(
        &mut self,
        change: impl FnOnce(&mut TerminalEngine) -> Result<()>,
    ) -> bool {
        self.mark_input_fast_path();
        change(&mut self.engine).is_ok()
    }

    fn respond<T>(
        &mut self,
        done: WorkerRequest<std::result::Result<T, String>>,
        operation: impl FnOnce(&mut Self) -> Result<T>,
    ) -> bool {
        if !done.try_claim() {
            return false;
        }
        done.send(operation(self).map_err(|error| error.to_string()));
        true
    }

    fn send_error(&self, error: &impl std::fmt::Display) {
        let _ = self.error_tx.send(error.to_string());
    }
}
