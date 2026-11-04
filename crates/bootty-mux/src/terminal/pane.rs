use bootty_terminal::terminal_search::TerminalSearchOptions;
use bootty_terminal::{
    terminal_capture::{CaptureOptions, TerminalCapture},
    terminal_session::PendingWorkerResponse,
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use anyhow::Result;
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::terminal_frame::RenderFrame;
use derive_more::{Deref, DerefMut};

use crate::MuxBindingConfig;
use bootty_terminal::{TerminalSession, TerminalSessionConfig, frame_source::TerminalFrameSource};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_input_model::{KeyInput, MouseInput},
};

use crate::{controller::SpaceId, snapshot::MuxPaneAnchor};

#[derive(Clone, Copy)]
pub(super) struct PaneStartRequest<'a> {
    pub(super) target: &'a ScopedMuxPaneTarget,
    pub(super) backend: &'a crate::native::NativeBackend,
    pub spawn_geometry: TerminalGeometry,
    pub display_scale: f32,
    pub render_cell: CellMetrics,
    pub terminal_config: &'a TerminalSessionConfig,
    pub repaint_wakeup: &'a Arc<dyn Fn() + Send + Sync + 'static>,
}

#[derive(Deref, DerefMut)]
pub struct ActiveTerminal {
    backend: crate::native::NativeBackend,
    active_target: Option<ScopedMuxPaneTarget>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    terminal_config: TerminalSessionConfig,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    native_terminals: HashMap<ScopedMuxPaneTarget, Box<dyn TerminalRuntime>>,
    /// The active window's panes, including parked siblings rendered beside the focused pane.
    native_window_targets: Vec<ScopedMuxPaneTarget>,
    native_window_id: Option<String>,
    native_window_scope: Option<SpaceId>,
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
    /// # Errors
    /// Returns an error if the process or terminal worker cannot report its status.
    fn child_exited(&mut self) -> Result<bool>;
    /// Whether this runtime's process has started: `Ok(false)` while it still starts in the
    /// background. A runtime that starts synchronously has started by the time it exists.
    /// # Errors
    /// Returns why the process could not start.
    fn started(&mut self) -> Result<bool> {
        Ok(true)
    }
    /// Restore validated text and styles without writing to the pane process.
    /// # Errors
    /// Returns unsupported or renderer worker errors.
    fn restore_history(&mut self, _text: &str) -> Result<()> {
        anyhow::bail!("this terminal cannot restore presentation history")
    }

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
    /// Capture a bounded checkpoint; owned terminal runtimes reduce whole rows to fit.
    /// # Errors
    /// Returns unsupported capture or terminal worker errors.
    fn capture_checkpoint(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        self.capture(options)
    }
    /// # Errors
    /// Returns terminal worker or selection formatting errors.
    fn format_selection(
        &mut self,
        format: TerminalSelectionFormat,
    ) -> Result<PendingWorkerResponse<std::result::Result<Option<Vec<u8>>, String>>>;
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
    /// Read the last published copy-mode state without a worker round trip.
    /// # Errors
    /// Returns worker or published-frame errors.
    fn copy_mode_active(&mut self) -> Result<bool>;
    /// # Errors
    /// Returns terminal worker or copy-mode operation errors.
    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome>;
    /// # Errors
    /// Returns terminal worker or search execution errors.
    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<()>;
    /// # Errors
    /// Returns terminal worker or search execution errors.
    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<()> {
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
    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn format_selection(
        &mut self,
        _format: TerminalSelectionFormat,
    ) -> Result<PendingWorkerResponse<std::result::Result<Option<Vec<u8>>, String>>> {
        let (done, response) = bootty_terminal::terminal_session::worker_request();
        if done.try_claim() {
            done.send(Ok(None));
        }
        Ok(response)
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

    fn search_viewport(&mut self, _query: &str, _direction: TerminalSearchDirection) -> Result<()> {
        Ok(())
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
    fn child_exited(&mut self) -> Result<bool> {
        Self::child_exited(self)
    }

    fn restore_history(&mut self, text: &str) -> Result<()> {
        Self::restore_history(self, text)
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
    fn capture_checkpoint(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        Self::capture_checkpoint(self, options)
    }

    fn format_selection(
        &mut self,
        format: TerminalSelectionFormat,
    ) -> Result<PendingWorkerResponse<std::result::Result<Option<Vec<u8>>, String>>> {
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

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<()> {
        Self::search_viewport(self, query, direction).map(|_| ())
    }

    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<()> {
        Self::search_viewport_with_options(self, query, direction, options).map(|_| ())
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

impl ActiveTerminal {
    /// # Errors
    /// Returns an error when the selected backend has no app provider.
    pub fn new(
        geometry: TerminalGeometry,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<Self> {
        Ok(Self {
            backend: crate::native::NativeBackend::new(),
            active_target: None,
            geometry,
            display_scale: 1.0,
            render_cell: geometry.cell_metrics(),
            terminal_config,
            repaint_wakeup,
            native_terminals: HashMap::new(),
            native_window_targets: Vec::new(),
            native_window_id: None,
            native_window_scope: None,
            terminal_awaits_resize: false,
            terminal: idle_terminal(),
        })
    }

    #[must_use]
    pub fn repaint_handle(&self) -> crate::controller::RepaintHandle {
        Arc::clone(&self.repaint_wakeup)
    }

    pub(crate) fn native_backend(&self) -> crate::native::NativeBackend {
        self.backend.clone()
    }

    pub fn set_native_backend(&mut self, backend: crate::native::NativeBackend) {
        self.backend = backend;
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
        if let Some(remote) = &config.remote
            && remote.as_ssh().is_none()
        {
            return Err(crate::workspace::RemoteSpaceUnavailable::new(remote).into());
        }
        let target = anchor
            .cloned()
            .and_then(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor));
        if scoped_target_matches_anchor(scope, self.active_target.as_ref(), anchor) {
            self.active_target = target;
            return Ok(());
        }

        self.park_cached_terminal();
        let phase = bootty_terminal::latency::start();
        let terminal = self.start_terminal(target.as_ref());
        bootty_terminal::latency::trace_slow("attach.start_terminal", phase, 2.0);

        self.active_target = terminal.as_ref().and(target);
        let phase = bootty_terminal::latency::start();
        self.set_active_terminal(terminal.unwrap_or_else(idle_terminal));
        bootty_terminal::latency::trace_slow("attach.set_active_terminal", phase, 2.0);
        Ok(())
    }

    pub fn set_terminal_config(&mut self, terminal_config: TerminalSessionConfig) {
        self.terminal_config = terminal_config;
    }

    /// Start a pane now, running `command` in place of the shell, before anything shows it. It
    /// stays parked, running and drained like any hidden pane, and the sync that first shows the
    /// pane presents this runtime.
    /// # Errors
    /// Returns an error when this backend cannot keep a hidden pane, when the pane already has a
    /// runtime, or when its terminal cannot start.
    pub fn start_scoped_native_command(
        &mut self,
        scope: SpaceId,
        pane: MuxPaneAnchor,
        command: Vec<String>,
    ) -> Result<()> {
        self.start_scoped_native_pane(scope, pane, command, None)
    }

    /// Start a restored shell with styled history present before its first output.
    /// # Errors
    /// Returns invalid history or pane startup errors.
    pub fn start_scoped_restored_native(
        &mut self,
        scope: SpaceId,
        pane: MuxPaneAnchor,
        history: &str,
    ) -> Result<()> {
        self.start_scoped_native_pane(scope, pane, Vec::new(), Some(std::sync::Arc::from(history)))
    }

    fn start_scoped_native_pane(
        &mut self,
        scope: SpaceId,
        pane: MuxPaneAnchor,
        _command: Vec<String>,
        _history: Option<std::sync::Arc<str>>,
    ) -> Result<()> {
        let target = ScopedMuxPaneTarget::from_anchor(Some(scope), pane)
            .ok_or_else(|| anyhow::anyhow!("the session has no pane to start"))?;
        let started = self.active_target.as_ref() == Some(&target)
            || self.native_terminals.contains_key(&target);
        if started {
            return Ok(());
        }
        let runtime = super::startup::start_native_terminal(PaneStartRequest {
            backend: &self.backend,
            target: &target,
            spawn_geometry: self.geometry,
            display_scale: self.display_scale,
            render_cell: self.render_cell,
            terminal_config: &self.terminal_config,
            repaint_wakeup: &self.repaint_wakeup,
        });
        self.native_terminals.insert(target, runtime);
        Ok(())
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
    ) -> Option<Box<dyn TerminalRuntime>> {
        self.start_terminal_at(target, self.geometry)
    }

    fn start_terminal_at(
        &mut self,
        target: Option<&ScopedMuxPaneTarget>,
        spawn_geometry: TerminalGeometry,
    ) -> Option<Box<dyn TerminalRuntime>> {
        let target = target?;

        if let Some(terminal) = self.native_terminals.remove(target) {
            return Some(terminal);
        }

        let request = PaneStartRequest {
            backend: &self.backend,
            target,
            spawn_geometry,
            display_scale: self.display_scale,
            render_cell: self.render_cell,
            terminal_config: &self.terminal_config,
            repaint_wakeup: &self.repaint_wakeup,
        };
        Some(super::startup::start_native_terminal(request))
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
    ) {
        self.sync_native_window_in_scope(None, window_panes, focused, window_id);
    }

    /// # Errors
    /// Returns target resolution, terminal startup, or backend synchronization errors.
    pub fn sync_scoped_native_window(
        &mut self,
        scope: SpaceId,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        window_id: Option<&str>,
    ) {
        self.sync_native_window_in_scope(Some(scope), window_panes, focused, window_id);
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
    ) {
        let targets = panes
            .iter()
            .cloned()
            .filter_map(|anchor| ScopedMuxPaneTarget::from_anchor(Some(scope), anchor))
            .collect::<Vec<_>>();
        self.prepare_native_targets(&targets, geometry);
    }

    fn prepare_native_targets(
        &mut self,
        targets: &[ScopedMuxPaneTarget],
        geometry: TerminalGeometry,
    ) {
        for target in targets {
            if self.active_target.as_ref() == Some(target)
                || self.native_terminals.contains_key(target)
            {
                continue;
            }
            if let Some(runtime) = self.start_terminal_at(Some(target), geometry) {
                self.native_terminals.insert(target.clone(), runtime);
            }
        }
    }

    /// Read or resize a visible pane by binding identity, independently of keyboard focus.
    pub fn scoped_terminal_runtime(
        &mut self,
        scope: SpaceId,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        let matches = |target: &ScopedMuxPaneTarget| {
            target.scope == Some(scope) && target.pane_id() == pane_id
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
    ) {
        let targets: Vec<ScopedMuxPaneTarget> = window_panes
            .iter()
            .cloned()
            .filter_map(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor))
            .collect();
        let focused_target = focused
            .cloned()
            .and_then(|anchor| ScopedMuxPaneTarget::from_anchor(scope, anchor))
            .or_else(|| targets.first().cloned());

        if self.active_target.as_ref() != focused_target.as_ref() {
            self.park_cached_terminal();
            let terminal = self.start_terminal(focused_target.as_ref());
            self.active_target = terminal.as_ref().and(focused_target);
            self.set_active_terminal(terminal.unwrap_or_else(idle_terminal));
        }

        self.prepare_native_targets(&targets, self.geometry);
        let window_id = window_id.map(str::to_owned);
        if self.native_window_scope != scope || self.native_window_id != window_id {
            self.native_window_scope = scope;
            self.native_window_id = window_id;
        }
        self.native_window_targets = targets;
    }

    /// A non-focused window pane's runtime, for painting it into its own sub-rect. The focused pane
    /// is rendered through `ActiveTerminal` itself (which keeps `geometry` in sync).
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

    fn discard_target(&mut self, target: &ScopedMuxPaneTarget) {
        if self.active_target.as_ref() == Some(target) {
            self.terminal = idle_terminal();
            self.active_target = None;
        } else {
            self.native_terminals.remove(target);
        }
    }

    /// Detach a pane's local VT whether it is the focused runtime or a parked sibling.
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
            self.discard_target(&target);
        }
    }

    /// Drop one Space's pane runtime, focused, rendered alongside, or hidden, killing its PTY.
    /// Native pane ids repeat across Spaces, so the Space decides which runtime goes.
    pub fn discard_scoped_pane(&mut self, scope: SpaceId, pane_id: &str) {
        let matches = |target: &ScopedMuxPaneTarget| {
            target.scope == Some(scope) && target.pane_id() == pane_id
        };
        let target = self
            .active_target
            .as_ref()
            .filter(|target| matches(target))
            .cloned()
            .or_else(|| {
                self.native_terminals
                    .keys()
                    .find(|target| matches(target))
                    .cloned()
            });
        if let Some(target) = target {
            self.discard_target(&target);
        }
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

    // Detach the active pane's local VT and forget its target, so the next
    // sync_mux_anchor attaches the surviving pane instead of parking the closed one.
    pub fn discard_active_pane(&mut self) {
        self.terminal = idle_terminal();
        self.active_target = None;
    }

    fn park_cached_terminal(&mut self) {
        let Some(target) = self.active_target.clone() else {
            return;
        };
        let terminal = std::mem::replace(&mut self.terminal, idle_terminal());
        self.native_terminals.insert(target, terminal);
    }
}

impl TerminalFrameSource for ActiveTerminal {
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
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.terminal.extract_frame()
    }
}

impl TerminalRuntime for ActiveTerminal {
    fn started(&mut self) -> Result<bool> {
        self.terminal.started()
    }

    fn child_exited(&mut self) -> Result<bool> {
        Self::child_exited(self)
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
    fn capture_checkpoint(
        &mut self,
        options: CaptureOptions,
    ) -> Result<PendingWorkerResponse<std::result::Result<TerminalCapture, String>>> {
        self.terminal.capture_checkpoint(options)
    }

    fn format_selection(
        &mut self,
        format: TerminalSelectionFormat,
    ) -> Result<PendingWorkerResponse<std::result::Result<Option<Vec<u8>>, String>>> {
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

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<()> {
        self.terminal.search_viewport(query, direction)
    }

    fn search_viewport_with_options(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        options: TerminalSearchOptions,
    ) -> Result<()> {
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
pub(super) struct ScopedMuxPaneTarget {
    scope: Option<SpaceId>,
    session_id: String,
    pane_id: String,
    cwd: Option<String>,
}

impl PartialEq for ScopedMuxPaneTarget {
    fn eq(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.session_id == other.session_id
            && self.pane_id == other.pane_id
    }
}
impl Hash for ScopedMuxPaneTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.scope.hash(state);
        self.session_id.hash(state);
        self.pane_id.hash(state);
    }
}
impl ScopedMuxPaneTarget {
    fn from_anchor(scope: Option<SpaceId>, anchor: MuxPaneAnchor) -> Option<Self> {
        Some(Self {
            scope,
            session_id: anchor.session_id,
            pane_id: anchor.pane_id?,
            cwd: anchor.cwd,
        })
    }
    fn input_selector(&self) -> &str {
        &self.pane_id
    }
    pub(super) fn pane_id(&self) -> &str {
        &self.pane_id
    }
    pub(super) fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
    pub(super) fn side_effect_pane_id(&self) -> String {
        self.scope.map_or_else(
            || self.pane_id.clone(),
            |scope| encode_scoped_pane_id(scope, &self.pane_id),
        )
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
    scope: Option<SpaceId>,
    target: Option<&ScopedMuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    match (target, anchor) {
        (None, None) => true,
        (Some(target), Some(anchor)) => {
            target.scope == scope
                && target.session_id == anchor.session_id
                && Some(target.pane_id()) == anchor.pane_id.as_deref()
        }
        _ => false,
    }
}
