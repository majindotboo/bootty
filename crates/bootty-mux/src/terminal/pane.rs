use bootty_terminal::terminal_search::TerminalSearchOptions;
use bootty_terminal::{
    terminal_capture::{CaptureOptions, TerminalCapture},
    terminal_session::PendingWorkerResponse,
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::terminal_frame::RenderFrame;
use derive_more::{Deref, DerefMut};

use crate::{MuxBackendKind, MuxBindingConfig};
use bootty_terminal::{
    DrainStats, TerminalSession, TerminalSessionConfig, frame_source::TerminalFrameSource,
};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_input_model::{KeyInput, MouseInput},
};

use crate::{
    controller::SpaceId,
    provider::{MuxBackendRegistry, PaneBehavior, PaneTopology},
    snapshot::MuxPaneAnchor,
};

#[derive(Clone, Copy)]
pub struct PaneStartRequest<'a> {
    pub target: &'a ScopedMuxPaneTarget,
    pub geometry: TerminalGeometry,
    pub spawn_geometry: TerminalGeometry,
    pub display_scale: f32,
    pub render_cell: CellMetrics,
    pub terminal_config: &'a TerminalSessionConfig,
    pub repaint_wakeup: &'a Arc<dyn Fn() + Send + Sync + 'static>,
}

pub struct PaneLayoutResizeRequest<'a> {
    pub window_id: Option<&'a str>,
    pub cols: u16,
    pub rows: u16,
    pub repaint_wakeup: &'a Arc<dyn Fn() + Send + Sync + 'static>,
}

pub trait BackendPanePolicy: Send {
    fn remote_target(&self) -> Option<crate::RemoteTarget>;
    /// # Errors
    /// Returns executable resolution, PTY, connection, or terminal startup errors.
    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>>;
    fn sync_target(&mut self, target: Option<&ScopedMuxPaneTarget>, hide_tmux_status: bool);
    fn set_layout_window(&mut self, window_id: Option<&str>);
    /// # Errors
    /// Returns backend resize or terminal transport errors.
    fn resize_layout_window(&mut self, request: PaneLayoutResizeRequest<'_>) -> Result<bool>;
    /// Drain failures produced by policy-owned background work.
    fn poll_async_errors(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn deactivate(&mut self);
}

const NATIVE_RESTART_MIN_DELAY: Duration = Duration::from_millis(250);
const NATIVE_RESTART_MAX_DELAY: Duration = Duration::from_secs(4);
const NATIVE_RESTART_QUIET_INTERVAL: Duration = Duration::from_secs(10);

struct NativeRuntimeRestart {
    failures: u32,
    failed_at: Option<Instant>,
    healthy_since: Option<Instant>,
    error_reported: bool,
}

impl NativeRuntimeRestart {
    const fn new(now: Instant) -> Self {
        Self {
            failures: 1,
            failed_at: Some(now),
            healthy_since: None,
            error_reported: false,
        }
    }

    const fn schedule_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.failed_at = Some(now);
        self.healthy_since = None;
        self.error_reported = false;
    }
}

fn native_restart_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(4);
    NATIVE_RESTART_MIN_DELAY
        .saturating_mul(1_u32 << exponent)
        .min(NATIVE_RESTART_MAX_DELAY)
}

#[derive(Deref, DerefMut)]
pub struct BackendPaneTerminal {
    registry: Arc<MuxBackendRegistry>,
    policy_kind: MuxBackendKind,
    policy: Box<dyn BackendPanePolicy>,
    behavior: PaneBehavior,
    active_target: Option<ScopedMuxPaneTarget>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    terminal_config: TerminalSessionConfig,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    native_terminals: HashMap<ScopedMuxPaneTarget, Box<dyn TerminalRuntime>>,
    /// The active native window's panes (focused + the parked siblings rendered alongside it). Empty
    /// for non-native backends, which render a single attach surface.
    native_window_targets: Vec<ScopedMuxPaneTarget>,
    native_window_spawn_geometry: Option<TerminalGeometry>,
    native_window_id: Option<String>,
    native_window_scope: Option<SpaceId>,
    native_runtime_restarts: HashMap<ScopedMuxPaneTarget, NativeRuntimeRestart>,
    /// Set when a runtime is swapped into the slot, cleared by the render resize that follows it.
    terminal_awaits_resize: bool,
    #[deref]
    #[deref_mut]
    terminal: Box<dyn TerminalRuntime>,
}
fn idle_terminal() -> Box<dyn TerminalRuntime> {
    Box::new(IdleTerminalRuntime {
        frame: Arc::new(RenderFrame::default()),
    })
}

pub trait TerminalRuntime: TerminalFrameSource + Send {
    fn drain_pty(&mut self) -> DrainStats;
    fn pending_pty_len(&self) -> usize;
    /// # Errors
    /// Returns an error if the process or terminal worker cannot report its status.
    fn child_exited(&mut self) -> Result<bool>;
    fn tty_name(&self) -> Option<&str>;
    /// # Errors
    /// Returns terminal worker or engine errors while discarding pending output.
    fn discard_pending_output(&mut self) -> Result<()>;
    /// # Errors
    /// Returns backend resize or terminal transport errors.
    fn force_resize(&mut self) -> Result<()>;
    /// # Errors
    /// Returns an error if prompt inspection is unsupported or the worker request cannot be queued.
    fn prompt(
        &mut self,
        _text: Option<(u64, String, bool)>,
    ) -> Result<
        PendingWorkerResponse<
            std::result::Result<bootty_terminal::shell_prompt::PromptSnapshot, String>,
        >,
    > {
        anyhow::bail!("This backend cannot exclusively lease a shell prompt")
    }
    /// # Errors
    /// Returns an error if capture is unsupported or its worker request cannot be queued.
    fn capture(
        &mut self,
        _options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        anyhow::bail!("This terminal has no capture runtime")
    }
    /// # Errors
    /// Returns terminal worker or selection formatting errors.
    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>>;
    /// # Errors
    /// Returns terminal worker or process inspection errors.
    fn current_working_directory(&mut self) -> Result<Option<String>>;
    /// Apply colors, cursor, and terminal features as one runtime update.
    ///
    /// A runtime that is not ready yet must retain the aggregate and apply it when it starts.
    /// # Errors
    /// Returns the first terminal configuration error after attempting every live runtime.
    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()>;
    /// # Errors
    /// Returns terminal worker or mouse-mode inspection errors.
    fn is_mouse_tracking(&mut self) -> Result<bool>;
    /// # Errors
    /// Returns terminal worker or viewport update errors.
    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()>;
    /// # Errors
    /// Returns terminal worker or viewport update errors.
    fn scroll_viewport_to(&mut self, offset: usize) -> Result<()>;
    /// # Errors
    /// Returns terminal worker or copy-mode operation errors.
    fn enter_copy_mode(&mut self) -> Result<()>;
    /// # Errors
    /// Returns terminal worker or copy-mode operation errors.
    fn copy_mode_active(&mut self) -> Result<bool>;
    /// # Errors
    /// Returns terminal worker or copy-mode operation errors.
    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome>;
    /// # Errors
    /// Returns terminal worker or search execution errors.
    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool>;
    /// # Errors
    /// Returns terminal worker or search execution errors.
    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<bool> {
        anyhow::ensure!(
            options == TerminalSearchOptions::default(),
            "search options are unavailable for this terminal"
        );
        self.search_viewport(query, direction)
    }

    /// # Errors
    /// Returns terminal worker or selection update errors.
    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()>;
    /// # Errors
    /// Returns terminal worker or selection update errors.
    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()>;
    /// # Errors
    /// Returns terminal worker or selection update errors.
    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()>;
    /// # Errors
    /// Returns input encoding, terminal worker, or transport errors.
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    /// # Errors
    /// Returns input encoding, terminal worker, or transport errors.
    fn write_paste(&mut self, text: &str) -> Result<()>;
    /// # Errors
    /// Returns input encoding, terminal worker, or transport errors.
    fn encode_key(&mut self, input: KeyInput) -> Result<()>;
    /// # Errors
    /// Returns input encoding, terminal worker, or transport errors.
    fn encode_focus(&mut self, gained: bool) -> Result<()>;
    /// # Errors
    /// Returns input encoding, terminal worker, or transport errors.
    fn encode_mouse(&mut self, input: MouseInput) -> Result<()>;
    /// # Errors
    /// Returns input encoding, terminal worker, or transport errors.
    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()>;
}
struct IdleTerminalRuntime {
    frame: Arc<RenderFrame>,
}

impl TerminalFrameSource for IdleTerminalRuntime {
    fn set_display_scale(&mut self, _display_scale: f32) -> Result<()> {
        Ok(())
    }

    fn set_render_cell_metrics(&mut self, _cell: CellMetrics) -> Result<()> {
        Ok(())
    }

    fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::clone(&self.frame))
    }
}

impl TerminalRuntime for IdleTerminalRuntime {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn tty_name(&self) -> Option<&str> {
        None
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        Ok(())
    }

    fn force_resize(&mut self) -> Result<()> {
        Ok(())
    }

    fn format_selection(&mut self, _format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    // Idle slots have no terminal state. The live update is intentionally a no-op.
    fn apply_live_config(&mut self, _config: TerminalLiveConfig) -> Result<()> {
        Ok(())
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn scroll_viewport_delta(&mut self, _delta: isize) -> Result<()> {
        Ok(())
    }

    fn scroll_viewport_to(&mut self, _offset: usize) -> Result<()> {
        Ok(())
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        Ok(())
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn handle_copy_mode_action(
        &mut self,
        _action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        Ok(TerminalCopyModeOutcome::default())
    }

    fn search_viewport(
        &mut self,
        _query: &str,
        _direction: TerminalSearchDirection,
    ) -> Result<bool> {
        Ok(false)
    }

    fn begin_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
        Ok(())
    }

    fn update_selection(&mut self, _event: TerminalSelectionEvent) -> Result<()> {
        Ok(())
    }

    fn end_selection(&mut self, _event: Option<TerminalSelectionEvent>) -> Result<()> {
        Ok(())
    }

    fn write_input(&mut self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    fn write_paste(&mut self, _text: &str) -> Result<()> {
        Ok(())
    }
    fn encode_key(&mut self, _input: KeyInput) -> Result<()> {
        Ok(())
    }

    fn encode_focus(&mut self, _gained: bool) -> Result<()> {
        Ok(())
    }

    fn encode_mouse(&mut self, _input: MouseInput) -> Result<()> {
        Ok(())
    }

    fn handle_mouse_wheel(&mut self, _input: MouseInput, _scroll_delta: isize) -> Result<()> {
        Ok(())
    }
}

impl TerminalRuntime for TerminalSession {
    fn drain_pty(&mut self) -> DrainStats {
        Self::drain_pty(self)
    }

    fn pending_pty_len(&self) -> usize {
        Self::pending_pty_len(self)
    }

    fn child_exited(&mut self) -> Result<bool> {
        Self::child_exited(self)
    }

    fn tty_name(&self) -> Option<&str> {
        Self::tty_name(self)
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        Self::discard_pending_output(self)
    }

    fn force_resize(&mut self) -> Result<()> {
        Ok(())
    }

    fn prompt(
        &mut self,
        text: Option<(u64, String, bool)>,
    ) -> Result<
        PendingWorkerResponse<
            std::result::Result<bootty_terminal::shell_prompt::PromptSnapshot, String>,
        >,
    > {
        Self::prompt(self, text)
    }
    fn capture(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        Self::capture(self, options)
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Self::format_selection(self, format)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(Self::current_working_directory(self))
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        Self::apply_live_config(self, config)
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Self::is_mouse_tracking(self)
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        Self::scroll_viewport_delta(self, delta)
    }

    fn scroll_viewport_to(&mut self, offset: usize) -> Result<()> {
        Self::scroll_viewport_to(self, offset)
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        Self::enter_copy_mode(self)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        Self::copy_mode_active(self)
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        Self::handle_copy_mode_action(self, action)
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        Self::search_viewport(self, query, direction)
    }

    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<bool> {
        Self::search_viewport_with_options(self, query, direction, options)
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        Self::begin_selection(self, event)
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        Self::update_selection(self, event)
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        Self::end_selection(self, event)
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        Self::write_input(self, bytes)
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        Self::write_paste(self, text)
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        Self::encode_key(self, input)
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        Self::encode_focus(self, gained)
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        Self::encode_mouse(self, input)
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        Self::handle_mouse_wheel(self, input, scroll_delta)
    }
}

impl BackendPaneTerminal {
    /// # Errors
    /// Returns an error when the selected backend has no app provider.
    pub fn new(
        geometry: TerminalGeometry,
        registry: Arc<MuxBackendRegistry>,
        config: &MuxBindingConfig,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<Self> {
        let kind = registry.selected_kind(config);
        let provider = registry.app_provider(config)?;
        let policy = provider.build_pane_policy(config);
        let behavior = provider.app_policy().panes;
        Ok(Self {
            registry,
            policy_kind: kind,
            policy,
            behavior,
            active_target: None,
            geometry,
            display_scale: 1.0,
            render_cell: geometry.cell_metrics(),
            terminal_config,
            repaint_wakeup,
            native_terminals: HashMap::new(),
            native_window_targets: Vec::new(),
            native_window_spawn_geometry: None,
            native_window_id: None,
            native_window_scope: None,
            native_runtime_restarts: HashMap::new(),
            terminal_awaits_resize: false,
            terminal: idle_terminal(),
        })
    }

    /// # Errors
    /// Returns target resolution, terminal startup, or backend synchronization errors.
    pub fn sync_mux_anchor(
        &mut self,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        self.sync_mux_anchor_in_scope(None, config, anchor)
    }

    /// # Errors
    /// Returns target resolution, terminal startup, or backend synchronization errors.
    pub fn sync_scoped_mux_anchor(
        &mut self,
        scope: SpaceId,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        self.sync_mux_anchor_in_scope(Some(scope), config, anchor)
    }

    fn sync_mux_anchor_in_scope(
        &mut self,
        scope: Option<SpaceId>,
        config: &MuxBindingConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        let provider = self.registry.app_provider(config)?;
        let next_policy = provider.build_pane_policy(config);
        let next_kind = self.registry.selected_kind(config);
        let next_behavior = provider.app_policy().panes;
        let backend_changed = self.policy_kind != next_kind
            || self.policy.remote_target() != next_policy.remote_target();
        if backend_changed {
            self.policy.deactivate();
            self.policy_kind = next_kind;
            self.policy = next_policy;
            self.behavior = next_behavior;
            self.active_target = None;
            self.native_terminals.clear();
            self.native_runtime_restarts.clear();
            self.terminal = idle_terminal();
        }
        let target = anchor
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor));
        if scoped_target_matches_anchor(
            self.behavior.topology,
            scope,
            self.active_target.as_ref(),
            anchor,
        ) {
            self.active_target = target;
            self.policy
                .sync_target(self.active_target.as_ref(), config.hide_tmux_status);
            return Ok(());
        }

        self.park_cached_terminal();
        let phase = bootty_terminal::latency::start();
        let terminal = self.start_terminal(target.as_ref()).inspect_err(|_| {
            self.active_target = None;
            self.policy.sync_target(None, config.hide_tmux_status);
            self.terminal = idle_terminal();
        })?;
        bootty_terminal::latency::trace_slow("attach.start_terminal", phase, 2.0);

        self.active_target = terminal.as_ref().and(target);
        let phase = bootty_terminal::latency::start();
        self.set_active_terminal(terminal.unwrap_or_else(idle_terminal));
        bootty_terminal::latency::trace_slow("attach.set_active_terminal", phase, 2.0);
        let phase = bootty_terminal::latency::start();
        self.policy
            .sync_target(self.active_target.as_ref(), config.hide_tmux_status);
        bootty_terminal::latency::trace_slow("attach.backend_policy", phase, 2.0);
        Ok(())
    }

    pub fn set_terminal_config(&mut self, terminal_config: TerminalSessionConfig) {
        self.terminal_config = terminal_config;
    }

    /// # Errors
    /// Returns the first terminal configuration error after attempting every live runtime.
    pub fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.terminal_config.colors.clone_from(&config.colors);
        self.terminal_config.cursor = config.cursor;
        self.terminal_config.features = config.features;

        // Try every runtime. One dead native pane must not block healthy focused or parked panes.
        let mut failed_cached_targets = Vec::new();
        for (target, terminal) in &mut self.native_terminals {
            if terminal.apply_live_config(config.clone()).is_err() {
                // A cached runtime cannot recover from a failed command: its pane or worker is
                // gone. Retire it so the next backend reconciliation can create a fresh runtime.
                failed_cached_targets.push(target.clone());
            }
        }
        for target in failed_cached_targets {
            self.native_terminals.remove(&target);
        }
        self.terminal.apply_live_config(config).map_err(|error| {
            anyhow::anyhow!("failed to apply live terminal config: active terminal: {error}")
        })
    }

    /// # Errors
    /// Returns terminal worker or process inspection errors.
    pub fn current_working_directory(&mut self) -> Result<Option<String>> {
        self.terminal.current_working_directory()
    }

    fn start_terminal(
        &mut self,
        target: Option<&ScopedMuxPaneTarget>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        self.start_terminal_at(
            target,
            self.native_window_spawn_geometry.unwrap_or(self.geometry),
        )
    }

    fn start_terminal_at(
        &mut self,
        target: Option<&ScopedMuxPaneTarget>,
        spawn_geometry: TerminalGeometry,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        let Some(target) = target else {
            return Ok(None);
        };

        if self.behavior.cache_terminals
            && let Some(terminal) = self.native_terminals.remove(target)
        {
            return Ok(Some(terminal));
        }

        let request = PaneStartRequest {
            target,
            geometry: self.geometry,
            spawn_geometry,
            display_scale: self.display_scale,
            render_cell: self.render_cell,
            terminal_config: &self.terminal_config,
            repaint_wakeup: &self.repaint_wakeup,
        };
        self.policy.start_terminal(request)
    }

    /// Swap in the runtime the pane slot renders and takes input through. The next render resize is
    /// forwarded even when the slot's geometry is unchanged: the incoming runtime holds whatever
    /// geometry it was parked at, and only the renderer knows the rect this pane now occupies.
    fn set_active_terminal(&mut self, terminal: Box<dyn TerminalRuntime>) {
        self.terminal = terminal;
        self.terminal_awaits_resize = true;
    }

    /// Reconcile the live native-layout runtimes against the active window's panes: make `focused`
    /// the deref/input runtime and keep every other pane alive in the parked map so it renders and
    /// drains alongside. Panes are only torn down on explicit close, so switching focus or tabs
    /// never kills a shell.
    /// # Errors
    /// Returns target resolution, terminal startup, or backend synchronization errors.
    pub fn sync_native_window(
        &mut self,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
        layout_backend: MuxBackendKind,
        hide_tmux_status: bool,
    ) -> Result<()> {
        self.sync_native_window_in_scope(
            None,
            window_panes,
            focused,
            window_id,
            layout_backend,
            hide_tmux_status,
        )
    }

    /// # Errors
    /// Returns target resolution, terminal startup, or backend synchronization errors.
    pub fn sync_scoped_native_window(
        &mut self,
        scope: SpaceId,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
        layout_backend: MuxBackendKind,
        hide_tmux_status: bool,
    ) -> Result<()> {
        self.sync_native_window_in_scope(
            Some(scope),
            window_panes,
            focused,
            window_id,
            layout_backend,
            hide_tmux_status,
        )
    }

    /// Attach visible panes without changing the terminal that receives keyboard input.
    /// Attach-only providers expose one opaque terminal and cannot use this path.
    /// # Errors
    /// Returns target resolution, terminal startup, or backend synchronization errors.
    pub fn prepare_scoped_native_panes(
        &mut self,
        scope: SpaceId,
        panes: &[MuxPaneAnchor],
        geometry: TerminalGeometry,
    ) -> Result<()> {
        anyhow::ensure!(
            self.behavior.topology != PaneTopology::Attach,
            "this backend exposes an opaque terminal attachment"
        );
        let targets = panes
            .iter()
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(Some(scope), anchor))
            .filter(|target| target.pane_id().is_some())
            .collect::<Vec<_>>();
        self.prepare_native_targets(&targets, geometry)
    }

    fn prepare_native_targets(
        &mut self,
        targets: &[ScopedMuxPaneTarget],
        geometry: TerminalGeometry,
    ) -> Result<()> {
        for target in targets {
            if self.active_target.as_ref() == Some(target)
                || self.native_terminals.contains_key(target)
            {
                continue;
            }
            if let Some(runtime) = self.start_terminal_at(Some(target), geometry)? {
                self.native_terminals.insert(target.clone(), runtime);
            }
        }
        Ok(())
    }

    /// Read or resize a visible pane by binding identity, independently of keyboard focus.
    pub fn scoped_terminal_runtime(
        &mut self,
        scope: SpaceId,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        let matches = |target: &ScopedMuxPaneTarget| {
            target.scope == Some(scope) && target.pane_id() == Some(pane_id)
        };
        if self.active_target.as_ref().is_some_and(matches) {
            return Some(self);
        }
        let (_, runtime) = self
            .native_terminals
            .iter_mut()
            .find(|(target, _)| matches(target))?;
        Some(runtime.as_mut())
    }

    fn sync_native_window_in_scope(
        &mut self,
        scope: Option<SpaceId>,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
        layout_backend: MuxBackendKind,
        hide_tmux_status: bool,
    ) -> Result<()> {
        debug_assert_eq!(self.policy_kind, layout_backend);
        debug_assert!(matches!(
            self.behavior.topology,
            PaneTopology::ProcessLocal | PaneTopology::BackendReconciled
        ));
        let targets: Vec<ScopedMuxPaneTarget> = window_panes
            .iter()
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor))
            .filter(|target| matches!(&target.target, MuxPaneTarget::Pane { .. }))
            .collect();
        let focused_target = focused
            .cloned()
            .map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor))
            .filter(|target| matches!(&target.target, MuxPaneTarget::Pane { .. }))
            .or_else(|| targets.first().cloned());

        if self.active_target.as_ref() != focused_target.as_ref() {
            self.park_cached_terminal();
            let terminal = self
                .start_terminal(focused_target.as_ref())
                .inspect_err(|_| {
                    self.active_target = None;
                    self.terminal = idle_terminal();
                })?;
            self.active_target = terminal.as_ref().and(focused_target);
            self.set_active_terminal(terminal.unwrap_or_else(idle_terminal));
        }

        self.prepare_native_targets(
            &targets,
            self.native_window_spawn_geometry.unwrap_or(self.geometry),
        )?;
        let window_id = window_id.map(str::to_owned);
        if self.native_window_scope != scope || self.native_window_id != window_id {
            self.native_window_scope = scope;
            self.native_window_id = window_id;
            self.policy
                .set_layout_window(self.native_window_id.as_deref());
        }
        self.native_window_targets = targets;
        self.policy
            .sync_target(self.active_target.as_ref(), hide_tmux_status);
        Ok(())
    }

    /// # Errors
    /// Returns backend resize or terminal transport errors.
    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.native_window_spawn_geometry = Some(TerminalGeometry {
            cols,
            rows,
            cell_width: self.geometry.cell_width,
            cell_height: self.geometry.cell_height,
        });
        let window_id = self.native_window_id.clone();
        self.resize_visible_window(window_id.as_deref(), cols, rows)
    }

    /// # Errors
    /// Returns backend resize or terminal transport errors.
    pub fn resize_visible_window(
        &mut self,
        window_id: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> Result<()> {
        let completed = self.policy.resize_layout_window(PaneLayoutResizeRequest {
            window_id,
            cols,
            rows,
            repaint_wakeup: &self.repaint_wakeup,
        })?;
        if completed {
            self.force_native_layout_pane_resizes()?;
        }
        Ok(())
    }

    fn force_native_layout_pane_resizes(&mut self) -> Result<()> {
        self.terminal.force_resize()?;
        let mut failed_cached_targets = Vec::new();
        for (target, runtime) in &mut self.native_terminals {
            if runtime.force_resize().is_err() {
                failed_cached_targets.push(target.clone());
            }
        }
        for target in failed_cached_targets {
            self.native_terminals.remove(&target);
        }
        Ok(())
    }

    /// A non-focused window pane's runtime, for painting it into its own sub-rect. The focused pane
    /// is rendered through `BackendPaneTerminal` itself (which keeps `geometry` in sync).
    pub fn terminal_runtime_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        if self
            .active_target
            .as_ref()
            .map(ScopedMuxPaneTarget::input_selector)
            == Some(pane_id)
        {
            return None;
        }
        let target = self
            .native_window_targets
            .iter()
            .find(|target| target.input_selector() == pane_id)?;
        let terminal = self.native_terminals.get_mut(target)?;
        Some(&mut **terminal)
    }

    /// The requested pane's runtime, including the focused/input pane.
    pub fn focused_terminal_runtime(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        if self
            .active_target
            .as_ref()
            .map(ScopedMuxPaneTarget::input_selector)
            == Some(pane_id)
        {
            return Some(&mut *self.terminal);
        }
        self.terminal_runtime_for_pane(pane_id)
    }

    /// The focused pane's id (the deref/input runtime), if any.
    pub fn focused_pane_id(&self) -> Option<&str> {
        self.active_target
            .as_ref()
            .map(ScopedMuxPaneTarget::input_selector)
    }

    /// Pane ids in the active window whose shell has exited (focused or background), so the layout
    /// can close them. Checked across every live pane, not just the focused one.
    pub fn native_exited_panes(&mut self) -> Vec<String> {
        let mut exited = Vec::new();
        if matches!(self.terminal.child_exited(), Ok(true))
            && let Some(id) = self.focused_pane_id()
        {
            exited.push(id.to_owned());
        }
        for target in &self.native_window_targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if let Some(runtime) = self.native_terminals.get_mut(target)
                && matches!(runtime.child_exited(), Ok(true))
            {
                exited.push(target.input_selector().to_owned());
            }
        }
        exited
    }

    /// Retire controller runtimes that exited while their backend panes still exist.
    ///
    /// Backend-reconciled topology owns pane lifetime outside Bootty. Clearing these slots makes
    /// the next reconciliation restart their controllers without issuing a backend close command.
    pub fn recover_exited_native_runtimes(
        &mut self,
        now: Instant,
    ) -> (Vec<String>, Option<Duration>) {
        let mut statuses = Vec::new();
        if let Some(target) = self.active_target.clone() {
            statuses.push((target, self.terminal.child_exited()));
        }
        for target in &self.native_window_targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if let Some(runtime) = self.native_terminals.get_mut(target) {
                statuses.push((target.clone(), runtime.child_exited()));
            }
        }

        let mut retire = Vec::new();
        let mut errors = Vec::new();
        let mut next_wake = None;
        for (target, status) in statuses {
            match status {
                Ok(false) => {
                    let should_reset =
                        self.native_runtime_restarts
                            .get_mut(&target)
                            .is_some_and(|restart| {
                                restart.failed_at = None;
                                restart.error_reported = false;
                                let healthy_since = restart.healthy_since.get_or_insert(now);
                                now.saturating_duration_since(*healthy_since)
                                    >= NATIVE_RESTART_QUIET_INTERVAL
                            });
                    if should_reset {
                        self.native_runtime_restarts.remove(&target);
                    }
                }
                failure @ (Ok(true) | Err(_)) => {
                    let restart = self
                        .native_runtime_restarts
                        .entry(target.clone())
                        .or_insert_with(|| NativeRuntimeRestart::new(now));
                    if restart.failed_at.is_none() {
                        restart.schedule_failure(now);
                    }
                    if let Err(error) = failure
                        && !restart.error_reported
                    {
                        errors.push(format!("{}: {error}", target.input_selector()));
                        restart.error_reported = true;
                    }
                    let failed_at = *restart.failed_at.get_or_insert(now);
                    let wait = native_restart_delay(restart.failures)
                        .saturating_sub(now.saturating_duration_since(failed_at));
                    if wait.is_zero() {
                        restart.failed_at = None;
                        restart.error_reported = false;
                        retire.push(target);
                    } else {
                        next_wake =
                            Some(next_wake.map_or(wait, |current: Duration| current.min(wait)));
                    }
                }
            }
        }
        for target in retire {
            self.discard_target(&target);
        }
        (errors, next_wake)
    }

    pub fn poll_policy_errors(&mut self) -> Vec<String> {
        self.policy.poll_async_errors()
    }

    fn discard_target(&mut self, target: &ScopedMuxPaneTarget) {
        if self.active_target.as_ref() == Some(target) {
            self.terminal = idle_terminal();
            self.active_target = None;
        } else {
            self.native_terminals.remove(target);
        }
    }

    /// Drop a pane's runtime (killing its PTY) whether it is the focused runtime or a parked sibling.
    pub fn discard_pane(&mut self, pane_id: &str) {
        let target = self
            .active_target
            .as_ref()
            .filter(|target| target.input_selector() == pane_id)
            .cloned()
            .or_else(|| {
                self.native_window_targets
                    .iter()
                    .find(|target| target.input_selector() == pane_id)
                    .cloned()
            });
        if let Some(target) = target {
            self.native_runtime_restarts.remove(&target);
            self.discard_target(&target);
        }
    }

    /// Drain the focused terminal and every cached runtime, including inactive scoped workspaces,
    /// so background PTYs cannot stall while another Space is selected.
    pub fn drain_native_window(&mut self) -> DrainStats {
        let stats = self.terminal.drain_pty();
        for runtime in self.native_terminals.values_mut() {
            runtime.drain_pty();
        }
        stats
    }

    /// # Errors
    /// Returns terminal worker or viewport update errors.
    pub fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
    }

    /// # Errors
    /// Returns terminal worker or viewport update errors.
    pub fn scroll_viewport_to(&mut self, offset: usize) -> Result<()> {
        self.terminal.scroll_viewport_to(offset)
    }

    /// # Errors
    /// Returns terminal worker or copy-mode operation errors.
    pub fn enter_copy_mode(&mut self) -> Result<()> {
        self.terminal.enter_copy_mode()
    }

    /// # Errors
    /// Returns terminal worker or copy-mode operation errors.
    pub fn copy_mode_active(&mut self) -> Result<bool> {
        self.terminal.copy_mode_active()
    }

    /// # Errors
    /// Returns terminal worker or copy-mode operation errors.
    pub fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.terminal.handle_copy_mode_action(action)
    }

    #[must_use]
    pub const fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    /// # Errors
    /// Returns an error if the process or terminal worker cannot report its status.
    pub fn child_exited(&mut self) -> Result<bool> {
        self.terminal.child_exited()
    }

    // Drop the active pane's terminal (its PTY is killed on drop) and forget its target, so the next
    // sync_mux_anchor attaches the surviving pane instead of parking the closed one.
    pub fn discard_active_pane(&mut self) {
        if let Some(target) = &self.active_target {
            self.native_runtime_restarts.remove(target);
        }
        self.terminal = idle_terminal();
        self.active_target = None;
    }

    fn park_cached_terminal(&mut self) {
        if !self.behavior.cache_terminals {
            return;
        }
        let Some(target) = self.active_target.clone() else {
            return;
        };
        let terminal = std::mem::replace(&mut self.terminal, idle_terminal());
        self.native_terminals.insert(target, terminal);
    }
}

impl Drop for BackendPaneTerminal {
    fn drop(&mut self) {
        // Best-effort cleanup: a hard kill skips this, and a later attach reapplies overrides.
        self.policy.deactivate();
    }
}

impl TerminalFrameSource for BackendPaneTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        self.display_scale = display_scale;
        self.terminal.set_display_scale(display_scale)
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        self.render_cell = cell;
        self.terminal.set_render_cell_metrics(cell)
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.geometry == geometry {
            // A runtime that just landed in the slot is still at the geometry it was parked at, and
            // this is the first call that knows the rect it now occupies. Runtimes drop a resize
            // they already applied, so an unchanged one still never reaches the PTY.
            if !std::mem::take(&mut self.terminal_awaits_resize) {
                return Ok(());
            }
            if let Err(error) = self.terminal.resize(geometry) {
                self.terminal_awaits_resize = true;
                return Err(error);
            }
            return Ok(());
        }
        self.terminal_awaits_resize = false;
        self.geometry = geometry;
        if let Err(error) = self.terminal.resize(geometry) {
            // Geometry is the renderer's latest fact, but the runtime did not accept it. Keep the
            // pending bit set so the next frame retries instead of deduplicating the failed write.
            self.terminal_awaits_resize = true;
            return Err(error);
        }
        if self.behavior.resize_cached_terminals {
            let mut failed_cached_targets = Vec::new();
            for (target, terminal) in &mut self.native_terminals {
                if terminal.resize(geometry).is_err() {
                    failed_cached_targets.push(target.clone());
                }
            }
            for target in failed_cached_targets {
                // Cached attach clients are not visible. A failed resize means the client is no
                // longer usable; retire it so it cannot block the live terminal and recreate it
                // when that session is selected again.
                self.native_terminals.remove(&target);
            }
        }
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.terminal.extract_frame()
    }
}

impl TerminalRuntime for BackendPaneTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        self.drain_native_window()
    }

    fn pending_pty_len(&self) -> usize {
        self.terminal.pending_pty_len()
    }

    fn child_exited(&mut self) -> Result<bool> {
        Self::child_exited(self)
    }

    fn tty_name(&self) -> Option<&str> {
        self.terminal.tty_name()
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        self.terminal.discard_pending_output()
    }

    fn force_resize(&mut self) -> Result<()> {
        self.terminal.force_resize()
    }

    fn prompt(
        &mut self,
        text: Option<(u64, String, bool)>,
    ) -> Result<
        PendingWorkerResponse<
            std::result::Result<bootty_terminal::shell_prompt::PromptSnapshot, String>,
        >,
    > {
        self.terminal.prompt(text)
    }
    fn capture(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        self.terminal.capture(options)
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.terminal.format_selection(format)
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Self::current_working_directory(self)
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        Self::apply_live_config(self, config)
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.terminal.is_mouse_tracking()
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
    }

    fn scroll_viewport_to(&mut self, offset: usize) -> Result<()> {
        self.terminal.scroll_viewport_to(offset)
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.terminal.enter_copy_mode()
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        self.terminal.copy_mode_active()
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.terminal.handle_copy_mode_action(action)
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        self.terminal.search_viewport(query, direction)
    }

    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<bool> {
        self.terminal
            .search_viewport_with_options(query, direction, options)
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.terminal.begin_selection(event)
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.terminal.update_selection(event)
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.terminal.end_selection(event)
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.terminal.write_input(bytes)
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.terminal.write_paste(text)
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.terminal.encode_key(input)
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.terminal.encode_focus(gained)
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.terminal.encode_mouse(input)
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.terminal.handle_mouse_wheel(input, scroll_delta)
    }
}

#[derive(Clone, Debug, Eq)]
pub enum MuxPaneTarget {
    Session {
        session_id: String,
        cwd: Option<String>,
    },
    Pane {
        session_id: String,
        pane_id: String,
        cwd: Option<String>,
    },
}

impl PartialEq for MuxPaneTarget {
    fn eq(&self, other: &Self) -> bool {
        self.session_id() == other.session_id() && self.input_selector() == other.input_selector()
    }
}

impl Hash for MuxPaneTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.session_id().hash(state);
        self.input_selector().hash(state);
    }
}

impl MuxPaneTarget {
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Session { session_id, .. } | Self::Pane { session_id, .. } => session_id,
        }
    }

    #[must_use]
    pub fn input_selector(&self) -> &str {
        match self {
            Self::Pane { pane_id, .. } => pane_id,
            Self::Session { session_id, .. } => session_id,
        }
    }

    #[must_use]
    pub fn pane_id(&self) -> Option<&str> {
        match self {
            Self::Pane { pane_id, .. } => Some(pane_id),
            Self::Session { .. } => None,
        }
    }

    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        match self {
            Self::Session { cwd, .. } | Self::Pane { cwd, .. } => cwd.as_deref(),
        }
    }
}

impl From<MuxPaneAnchor> for MuxPaneTarget {
    fn from(anchor: MuxPaneAnchor) -> Self {
        match anchor.pane_id {
            Some(pane_id) => Self::Pane {
                session_id: anchor.session_id,
                pane_id,
                cwd: anchor.cwd,
            },
            None => Self::Session {
                session_id: anchor.session_id,
                cwd: anchor.cwd,
            },
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ScopedMuxPaneTarget {
    scope: Option<SpaceId>,
    target: MuxPaneTarget,
}

impl ScopedMuxPaneTarget {
    fn from_anchor(scope: Option<SpaceId>, anchor: MuxPaneAnchor) -> Self {
        Self {
            scope,
            target: MuxPaneTarget::from(anchor),
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        self.target.session_id()
    }

    #[must_use]
    pub const fn mux_target(&self) -> &MuxPaneTarget {
        &self.target
    }

    #[must_use]
    pub fn input_selector(&self) -> &str {
        self.target.input_selector()
    }

    #[must_use]
    pub fn pane_id(&self) -> Option<&str> {
        self.target.pane_id()
    }

    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.target.cwd()
    }

    #[must_use]
    pub fn side_effect_pane_id(&self) -> Option<String> {
        let pane_id = self.pane_id()?;
        Some(self.scope.map_or_else(
            || pane_id.to_owned(),
            |scope| encode_scoped_pane_id(scope, pane_id),
        ))
    }
}

impl From<MuxPaneTarget> for ScopedMuxPaneTarget {
    fn from(target: MuxPaneTarget) -> Self {
        Self {
            scope: None,
            target,
        }
    }
}

const SCOPED_PANE_PREFIX: &str = "bootty-scope:";

#[must_use]
pub fn encode_scoped_pane_id(scope: SpaceId, pane_id: &str) -> String {
    format!(
        "{SCOPED_PANE_PREFIX}{}:{pane_id}",
        scope.persistence_value()
    )
}

#[must_use]
pub fn decode_scoped_pane_id(value: &str) -> Option<(SpaceId, String)> {
    let mut parts = value.strip_prefix(SCOPED_PANE_PREFIX)?.splitn(2, ':');
    let space_id = parts.next()?.parse().ok()?;
    let pane_id = parts.next()?.to_owned();
    Some((SpaceId::from_persistence(space_id), pane_id))
}

fn scoped_target_matches_anchor(
    topology: PaneTopology,
    scope: Option<SpaceId>,
    target: Option<&ScopedMuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    if target.is_some_and(|target| target.scope != scope) {
        return false;
    }
    target_matches_anchor(topology, target.map(|target| &target.target), anchor)
}

fn target_matches_anchor(
    topology: PaneTopology,
    target: Option<&MuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    match (target, anchor) {
        (None, None) => true,
        (Some(target), Some(anchor)) if target.session_id() == anchor.session_id => {
            // Attached multiplexer clients follow pane and window changes
            // server-side; restarting them on an active-pane
            // change blanks the whole surface for nothing.
            let anchor_selector = anchor.pane_id.as_deref().unwrap_or(&anchor.session_id);
            topology == PaneTopology::Attach || target.input_selector() == anchor_selector
        }
        _ => false,
    }
}
