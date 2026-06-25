use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    hash::{Hash, Hasher},
    path::Path,
    process::Command,
    sync::Arc,
};

use anyhow::Result;
use bootty_surface::geometry::TerminalGeometry;
use bootty_terminal::terminal_frame::RenderFrame;
use derive_more::{Deref, DerefMut};

use bootty_config::config::MultiplexerConfig;
use bootty_runtime::{
    DrainStats, TerminalSession, TerminalSessionConfig, render_source::TerminalRenderSource,
};
use bootty_terminal::{
    terminal_engine::{
        TerminalCursorConfig, TerminalFeatureConfig, TerminalSelectionEvent,
        TerminalSelectionFormat,
    },
    terminal_input_model::{KeyInput, MouseInput},
};

use crate::{
    config::{MuxBackendKind, selected_backend},
    snapshot::MuxPaneAnchor,
};

use super::{rmux_native::RmuxNativeTerminal, tmux_control::TmuxControlTerminal};

pub(super) const TMUX_CLIENT_FEATURES: &str =
    "256,RGB,clipboard,focus,hyperlinks,overline,strikethrough,sync,title";

#[derive(Deref, DerefMut)]
pub struct BackendPaneTerminal {
    backend: MuxBackendKind,
    pub(super) active_target: Option<MuxPaneTarget>,
    geometry: TerminalGeometry,
    terminal_config: TerminalSessionConfig,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    native_terminals: HashMap<MuxPaneTarget, ActiveTerminalRuntime>,
    /// The active native window's panes (focused + the parked siblings rendered alongside it). Empty
    /// for non-native backends, which render a single attach surface.
    native_window_targets: Vec<MuxPaneTarget>,
    /// Session whose tmux `status` option bootty has toggled off so its own
    /// status bar is the only one shown; restored when bootty stops showing it.
    status_hidden_session: Option<String>,
    #[deref]
    #[deref_mut]
    terminal: ActiveTerminalRuntime,
}

#[derive(Deref, DerefMut)]
#[deref(forward)]
#[deref_mut(forward)]
pub struct ActiveTerminalRuntime(Box<dyn TerminalRuntime>);

impl ActiveTerminalRuntime {
    fn idle() -> Self {
        Self(Box::new(IdleRenderSource))
    }
}

// Lets a non-focused pane's runtime be rendered directly by a per-pane `TerminalWidget` without
// going through `BackendPaneTerminal` (which only ever exposes the focused pane).
impl TerminalRenderSource for ActiveTerminalRuntime {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.0.resize(geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.0.extract_frame()
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.0.is_mouse_tracking()
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.0.scroll_viewport_delta(delta)
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.0.begin_selection(event)
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.0.update_selection(event)
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.0.end_selection(event)
    }
}

pub trait TerminalRuntime: TerminalRenderSource {
    fn drain_pty(&mut self) -> DrainStats;
    fn pending_pty_len(&self) -> usize;
    fn child_exited(&mut self) -> Result<bool>;
    fn tty_name(&self) -> Option<&str> {
        None
    }
    fn discard_pending_output(&mut self) -> Result<()> {
        Ok(())
    }
    fn format_selection(&mut self, _format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn set_cursor_config(&mut self, _cursor: TerminalCursorConfig) -> Result<()> {
        Ok(())
    }
    fn set_feature_config(&mut self, _features: TerminalFeatureConfig) -> Result<()> {
        Ok(())
    }
    fn set_colors(
        &mut self,
        colors: bootty_terminal::terminal_engine::TerminalColorConfig,
    ) -> Result<()>;
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    fn write_paste(&mut self, text: &str) -> Result<()>;
    fn encode_key(&mut self, input: KeyInput) -> Result<()>;
    fn encode_focus(&mut self, gained: bool) -> Result<()>;
    fn encode_mouse(&mut self, input: MouseInput) -> Result<()>;
    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()>;
}
struct IdleRenderSource;

impl TerminalRenderSource for IdleRenderSource {
    fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(RenderFrame::default()))
    }
}

impl TerminalRuntime for IdleRenderSource {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn set_colors(
        &mut self,
        _colors: bootty_terminal::terminal_engine::TerminalColorConfig,
    ) -> Result<()> {
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

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        Self::format_selection(self, format)
    }

    fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        Self::set_cursor_config(self, cursor)
    }

    fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        Self::set_feature_config(self, features)
    }

    fn set_colors(
        &mut self,
        colors: bootty_terminal::terminal_engine::TerminalColorConfig,
    ) -> Result<()> {
        Self::set_colors(self, colors)
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
    pub fn new(
        geometry: TerminalGeometry,
        config: &MultiplexerConfig,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self::new_with_backend(
            geometry,
            selected_backend(config),
            terminal_config,
            repaint_wakeup,
        )
    }

    pub(super) fn new_with_backend(
        geometry: TerminalGeometry,
        backend: MuxBackendKind,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self {
            backend,
            active_target: None,
            geometry,
            terminal_config,
            repaint_wakeup,
            native_terminals: HashMap::new(),
            native_window_targets: Vec::new(),
            status_hidden_session: None,
            terminal: ActiveTerminalRuntime::idle(),
        }
    }

    pub fn sync_mux_anchor(
        &mut self,
        config: &MultiplexerConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        let backend = selected_backend(config);
        if self.backend == backend
            && target_matches_anchor(backend, self.active_target.as_ref(), anchor)
        {
            // Attach and target unchanged; still reconcile the status override so a
            // runtime toggle of hide-tmux-status takes effect without a re-attach.
            self.sync_status_bar(config.hide_tmux_status);
            return Ok(());
        }

        let target = anchor.cloned().map(MuxPaneTarget::from);
        if self.backend == MuxBackendKind::Tmux
            && backend == MuxBackendKind::Tmux
            && target.is_some()
            && self.active_target.is_some()
            && self
                .switch_tmux_client(target.as_ref().expect("target checked above"))
                .is_ok()
        {
            self.active_target = target;
            self.sync_status_bar(config.hide_tmux_status);
            return Ok(());
        }
        self.park_native_terminal();
        let terminal = self
            .start_terminal(backend, target.as_ref())
            .inspect_err(|_| {
                self.backend = backend;
                self.active_target = None;
                self.clear_terminal();
            })?;

        self.backend = backend;
        self.active_target = target;
        self.terminal = terminal;
        self.sync_status_bar(config.hide_tmux_status);
        Ok(())
    }

    /// Toggle tmux's per-session `status` option so only bootty's own status bar
    /// shows in its client, restoring the previous session's bar when bootty
    /// moves off it. Session-scoped and reversible: it never touches a global
    /// option, and only ever acts on the tmux backend. Best-effort, so a failed
    /// toggle never blocks the attach.
    fn sync_status_bar(&mut self, hide_enabled: bool) {
        let want = status_bar_hidden_target(
            hide_enabled,
            self.backend,
            self.active_target.as_ref().map(MuxPaneTarget::session_id),
        );
        if self.status_hidden_session.as_deref() == want {
            return;
        }
        if let Some(previous) = self.status_hidden_session.take() {
            let _ = set_session_status_hidden(&previous, false);
        }
        if let Some(session) = want
            && set_session_status_hidden(session, true).is_ok()
        {
            self.status_hidden_session = Some(session.to_owned());
        }
    }

    pub fn set_terminal_config(&mut self, terminal_config: TerminalSessionConfig) {
        self.terminal_config = terminal_config;
    }

    fn start_terminal(
        &mut self,
        backend: MuxBackendKind,
        target: Option<&MuxPaneTarget>,
    ) -> Result<ActiveTerminalRuntime> {
        let Some(target) = target else {
            return Ok(ActiveTerminalRuntime::idle());
        };

        if backend == MuxBackendKind::Native {
            // A native session whose tabs have all been closed resolves to a session-level target
            // with no pane; it has no shell to attach, so it renders as idle.
            if !matches!(target, MuxPaneTarget::Pane { .. }) {
                return Ok(ActiveTerminalRuntime::idle());
            }
            if let Some(mut terminal) = self.native_terminals.remove(target) {
                terminal.resize(self.geometry)?;
                return Ok(terminal);
            }
            self.spawn_native_runtime(target)
        } else if matches!(backend, MuxBackendKind::Tmux | MuxBackendKind::Zellij) {
            let config = backend_attach_session_config(
                self.terminal_config.clone(),
                backend,
                target.session_id(),
                bootty_runtime::terminfo::vendored_terminfo_dir().is_some(),
            )?;
            Ok(ActiveTerminalRuntime(Box::new(
                TerminalSession::new_with_config(
                    self.geometry,
                    config,
                    Arc::clone(&self.repaint_wakeup),
                )?,
            )))
        } else if backend == MuxBackendKind::Rmux {
            Ok(ActiveTerminalRuntime(Box::new(RmuxNativeTerminal::new(
                target.clone(),
                self.geometry,
                self.terminal_config.colors.clone(),
            )?)))
        } else {
            Ok(ActiveTerminalRuntime(Box::new(TmuxControlTerminal::new(
                backend,
                target.clone(),
                self.geometry,
                self.terminal_config.colors.clone(),
                self.terminal_config.macos_option_as_alt,
                self.terminal_config.side_effect_tx.clone(),
                Arc::clone(&self.repaint_wakeup),
            )?)))
        }
    }

    fn spawn_native_runtime(&self, target: &MuxPaneTarget) -> Result<ActiveTerminalRuntime> {
        let mut config = self.terminal_config.clone();
        config.launch.working_directory = target.cwd().map(Path::new).map(Path::to_path_buf);
        Ok(ActiveTerminalRuntime(Box::new(
            TerminalSession::new_with_config(
                self.geometry,
                config,
                Arc::clone(&self.repaint_wakeup),
            )?,
        )))
    }

    /// Reconcile the live native runtimes against the active window's panes: make `focused` the
    /// deref (input) runtime and keep every other pane alive in the parked map so it renders and
    /// drains alongside. Panes are only torn down on explicit close, so switching focus or tabs
    /// never kills a shell.
    pub fn sync_native_window(
        &mut self,
        window_panes: &[MuxPaneAnchor],
        focused: Option<&MuxPaneAnchor>,
        hide_tmux_status: bool,
    ) -> Result<()> {
        self.backend = MuxBackendKind::Native;
        let targets: Vec<MuxPaneTarget> = window_panes
            .iter()
            .cloned()
            .map(MuxPaneTarget::from)
            .filter(|target| matches!(target, MuxPaneTarget::Pane { .. }))
            .collect();
        let focused_target = focused
            .cloned()
            .map(MuxPaneTarget::from)
            .filter(|target| matches!(target, MuxPaneTarget::Pane { .. }))
            .or_else(|| targets.first().cloned());

        if self.active_target.as_ref() != focused_target.as_ref() {
            self.park_native_terminal();
            let terminal = self
                .start_terminal(MuxBackendKind::Native, focused_target.as_ref())
                .inspect_err(|_| {
                    self.active_target = None;
                    self.clear_terminal();
                })?;
            self.active_target = focused_target;
            self.terminal = terminal;
        }

        for target in &targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if !self.native_terminals.contains_key(target) {
                let runtime = self.spawn_native_runtime(target)?;
                self.native_terminals.insert(target.clone(), runtime);
            }
        }
        self.native_window_targets = targets;
        self.sync_status_bar(hide_tmux_status);
        Ok(())
    }

    /// A non-focused window pane's render source, for painting it into its own sub-rect. The focused
    /// pane is rendered through `BackendPaneTerminal` itself (which keeps `geometry` in sync).
    pub fn render_source_for_pane(&mut self, pane_id: &str) -> Option<&mut ActiveTerminalRuntime> {
        if self
            .active_target
            .as_ref()
            .map(MuxPaneTarget::input_selector)
            == Some(pane_id)
        {
            return None;
        }
        let target = self
            .native_window_targets
            .iter()
            .find(|target| target.input_selector() == pane_id)?
            .clone();
        self.native_terminals.get_mut(&target)
    }

    /// The focused pane's id (the deref/input runtime), if any.
    pub fn focused_pane_id(&self) -> Option<&str> {
        self.active_target
            .as_ref()
            .map(MuxPaneTarget::input_selector)
    }

    /// Drain the focused pane and every parked sibling in the active window so background panes keep
    /// processing PTY output and repaint. Returns the focused pane's drain stats.
    pub fn drain_native_window(&mut self) -> DrainStats {
        let stats = self.terminal.drain_pty();
        let targets = self.native_window_targets.clone();
        for target in &targets {
            if self.active_target.as_ref() == Some(target) {
                continue;
            }
            if let Some(runtime) = self.native_terminals.get_mut(target) {
                runtime.drain_pty();
            }
        }
        stats
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
    }

    pub fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.terminal.format_selection(format)
    }

    pub fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        self.terminal.set_cursor_config(cursor)
    }

    pub fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        self.terminal.set_feature_config(features)
    }

    pub fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    pub fn child_exited(&mut self) -> Result<bool> {
        self.terminal.child_exited()
    }

    fn switch_tmux_client(&mut self, target: &MuxPaneTarget) -> Result<()> {
        let tty = self
            .terminal
            .tty_name()
            .ok_or_else(|| anyhow::anyhow!("active tmux attach client tty unavailable"))?
            .to_owned();
        self.terminal.discard_pending_output()?;
        let status = Command::new(resolve_launch_program("tmux")?)
            .args(["switch-client", "-c", &tty, "-t", target.session_id()])
            .env_remove("TMUX")
            .env_remove("ZELLIJ")
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux switch-client failed with status {status}");
        }
        Ok(())
    }

    // Drop the active pane's terminal (its PTY is killed on drop) and forget its target, so the next
    // sync_mux_anchor attaches the surviving pane instead of parking the closed one.
    pub fn discard_active_pane(&mut self) {
        self.terminal = ActiveTerminalRuntime::idle();
        self.active_target = None;
    }

    fn clear_terminal(&mut self) {
        self.terminal = ActiveTerminalRuntime::idle();
    }

    fn park_native_terminal(&mut self) {
        if self.backend != MuxBackendKind::Native {
            return;
        }
        let Some(target) = self.active_target.clone() else {
            return;
        };
        let terminal = std::mem::replace(&mut self.terminal, ActiveTerminalRuntime::idle());
        self.native_terminals.insert(target, terminal);
    }
}

impl Drop for BackendPaneTerminal {
    fn drop(&mut self) {
        // Bring the tmux status bar back when bootty stops showing the session
        // (window closed, app quit). Best-effort: a hard kill skips this, and a
        // later attach re-hides while a clean detach restores.
        if let Some(session) = self.status_hidden_session.take() {
            let _ = set_session_status_hidden(&session, false);
        }
    }
}

impl TerminalRenderSource for BackendPaneTerminal {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.geometry = geometry;
        self.terminal.resize(geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.terminal.extract_frame()
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.terminal.is_mouse_tracking()
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
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
}

#[derive(Clone, Debug, Eq)]
pub(super) enum MuxPaneTarget {
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
    pub(super) fn session_id(&self) -> &str {
        match self {
            Self::Session { session_id, .. } | Self::Pane { session_id, .. } => session_id,
        }
    }

    pub(super) fn input_selector(&self) -> &str {
        match self {
            Self::Pane { pane_id, .. } => pane_id,
            target => target.session_id(),
        }
    }

    fn cwd(&self) -> Option<&str> {
        match self {
            Self::Session { cwd, .. } | Self::Pane { cwd, .. } => cwd.as_deref(),
        }
    }

    pub(super) fn tmux_pane_number(&self) -> Option<usize> {
        let Self::Pane { pane_id, .. } = self else {
            return None;
        };
        pane_id.strip_prefix('%')?.parse().ok()
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

fn target_matches_anchor(
    backend: MuxBackendKind,
    target: Option<&MuxPaneTarget>,
    anchor: Option<&MuxPaneAnchor>,
) -> bool {
    match (target, anchor) {
        (None, None) => true,
        (Some(target), Some(anchor)) => {
            if target.session_id() != anchor.session_id {
                return false;
            }
            // Attached clients (tmux/zellij attach PTYs) follow pane and
            // window changes server-side; restarting them on an active-pane
            // change blanks the whole surface for nothing.
            if matches!(backend, MuxBackendKind::Tmux | MuxBackendKind::Zellij) {
                return true;
            }
            let anchor_selector = anchor.pane_id.as_deref().unwrap_or(&anchor.session_id);
            target.input_selector() == anchor_selector
        }
        _ => false,
    }
}

pub(super) fn backend_attach_launch(
    backend: MuxBackendKind,
    session: &str,
) -> (String, Vec<String>) {
    let session = session.to_owned();
    match backend {
        // -T declares outer-terminal features tmux cannot learn from the
        // forced xterm-256color terminfo; "clipboard" enables OSC 52 and
        // "sync" wraps redraws in DEC 2026 to avoid blank layout flashes.
        MuxBackendKind::Tmux => (
            "tmux".to_owned(),
            vec![
                "-T".to_owned(),
                TMUX_CLIENT_FEATURES.to_owned(),
                "attach-session".to_owned(),
                "-t".to_owned(),
                session,
            ],
        ),
        MuxBackendKind::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MuxBackendKind::Native => unreachable!("native panes are rendered directly by Bootty"),
        MuxBackendKind::Zellij => (
            "zellij".to_owned(),
            vec!["attach".to_owned(), "--create".to_owned(), session],
        ),
    }
}

fn backend_attach_env_remove(backend: MuxBackendKind) -> Vec<String> {
    match backend {
        MuxBackendKind::Tmux => vec!["TMUX".to_owned()],
        MuxBackendKind::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MuxBackendKind::Native => unreachable!("native panes are rendered directly by Bootty"),
        MuxBackendKind::Zellij => vec!["ZELLIJ".to_owned()],
    }
}

fn backend_attach_session_config(
    mut config: TerminalSessionConfig,
    backend: MuxBackendKind,
    attach_session: &str,
    bootty_terminfo_available: bool,
) -> Result<TerminalSessionConfig> {
    let (program, args) = backend_attach_launch(backend, attach_session);
    config.launch.shell = Some(resolve_launch_program(&program)?);
    config.launch.args = args;
    config.launch.env_remove = backend_attach_env_remove(backend);
    // The attach client hard-fails on a TERM it cannot resolve. xterm-bootty
    // only resolves through Bootty's vendored terminfo; anything else falls
    // back to the universally installed xterm-256color, with required
    // features pinned via the -T attach flag either way.
    if config.launch.term != bootty_runtime::terminfo::XTERM_BOOTTY || !bootty_terminfo_available {
        config.launch.term = "xterm-256color".to_owned();
    }
    Ok(config)
}

fn resolve_launch_program(program: &str) -> Result<String> {
    resolve_launch_program_with_path(program, env::var_os("PATH").as_deref())
}

fn resolve_launch_program_with_path(program: &str, path: Option<&OsStr>) -> Result<String> {
    if Path::new(program).is_absolute() {
        return Ok(program.to_owned());
    }
    if let Some(found) = path
        .into_iter()
        .flat_map(env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
    {
        return Ok(found.to_string_lossy().into_owned());
    }
    anyhow::bail!("backend attach program {program:?} not found in PATH")
}

/// The session whose tmux status bar should be hidden: only with the feature on,
/// the tmux backend, and a session attached. Native/rmux/zellij are never
/// touched, so this can only ever issue a `set-option` against a tmux server.
fn status_bar_hidden_target(
    hide_enabled: bool,
    backend: MuxBackendKind,
    session_id: Option<&str>,
) -> Option<&str> {
    if hide_enabled && backend == MuxBackendKind::Tmux {
        session_id
    } else {
        None
    }
}

/// Toggle a single tmux session's `status` option on the default-socket server
/// bootty attached. Hiding sets it off for that session alone; restoring unsets
/// the session override so it falls back to the global default. Never sets a
/// global option, so it cannot affect any other session.
fn set_session_status_hidden(session_id: &str, hidden: bool) -> Result<()> {
    let program = resolve_launch_program("tmux")?;
    let mut command = Command::new(program);
    if hidden {
        command.args(["set-option", "-t", session_id, "status", "off"]);
    } else {
        command.args(["set-option", "-u", "-t", session_id, "status"]);
    }
    command.env_remove("TMUX").env_remove("ZELLIJ");
    let output = command.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux set-option status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootty_terminal::terminal_engine::TerminalColorConfig;
    use tempfile::TempDir;

    use bootty_config::config::{MultiplexerBackendConfig, MultiplexerConfig};

    #[test]
    fn status_bar_hidden_only_targets_tmux_when_enabled() {
        // Enabled, tmux backend, attached session: that session is the target.
        assert_eq!(
            status_bar_hidden_target(true, MuxBackendKind::Tmux, Some("$1")),
            Some("$1")
        );
        // Disabled: never hide, even on tmux.
        assert_eq!(
            status_bar_hidden_target(false, MuxBackendKind::Tmux, Some("$1")),
            None
        );
        // Safety contract: a non-tmux backend is never touched, so bootty can
        // never run `set-option` against native/rmux/zellij sessions.
        assert_eq!(
            status_bar_hidden_target(true, MuxBackendKind::Native, Some("$1")),
            None
        );
        // No attached session means nothing to toggle.
        assert_eq!(
            status_bar_hidden_target(true, MuxBackendKind::Tmux, None),
            None
        );
    }

    fn terminal_config() -> TerminalSessionConfig {
        TerminalSessionConfig {
            launch: Default::default(),
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            benchmark_trace: None,
        }
    }

    #[test]
    fn attach_target_uses_session_and_pane_identity_not_process_metadata() {
        let before = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            cwd: Some("/repo".to_owned()),
            process: Some("nvim".to_owned()),
        };
        let after = MuxPaneAnchor {
            process: Some("zsh".to_owned()),
            cwd: Some("/repo/subdir".to_owned()),
            ..before.clone()
        };

        assert_eq!(MuxPaneTarget::from(before), MuxPaneTarget::from(after));
    }

    #[test]
    fn sync_mux_anchor_does_not_commit_target_after_restart_failure() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MuxBackendKind::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );

        let anchor = MuxPaneAnchor {
            session_id: String::new(),
            pane_id: Some("%11".to_owned()),
            cwd: None,
            process: None,
        };
        let result = terminal.sync_mux_anchor(
            &MultiplexerConfig {
                backend: MultiplexerBackendConfig::Rmux,
                ..Default::default()
            },
            Some(&anchor),
        );

        assert!(result.is_err());
        assert_eq!(terminal.active_target, None);
    }

    #[test]
    fn target_match_uses_session_and_pane_without_cloning_metadata() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: Some("/repo".to_owned()),
        };
        let anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            cwd: Some("/repo/subdir".to_owned()),
            process: Some("zsh".to_owned()),
        };

        assert!(target_matches_anchor(
            MuxBackendKind::Rmux,
            Some(&target),
            Some(&anchor)
        ));
    }

    #[test]
    fn pane_rendering_backends_restart_on_missing_and_changed_panes() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: None,
        };
        let session_anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: None,
            cwd: None,
            process: None,
        };
        let other_pane = MuxPaneAnchor {
            pane_id: Some("%4".to_owned()),
            ..session_anchor.clone()
        };

        for backend in [MuxBackendKind::Rmux, MuxBackendKind::Native] {
            assert!(!target_matches_anchor(
                backend,
                Some(&target),
                Some(&session_anchor)
            ));
            assert!(!target_matches_anchor(
                backend,
                Some(&target),
                Some(&other_pane)
            ));
            assert!(target_matches_anchor(backend, None, None));
        }
    }

    #[test]
    fn attached_client_backends_follow_pane_changes_without_restart() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: None,
        };
        let split_changed_active_pane = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%4".to_owned()),
            cwd: None,
            process: None,
        };
        let other_session = MuxPaneAnchor {
            session_id: "dotfiles".to_owned(),
            ..split_changed_active_pane.clone()
        };

        for backend in [MuxBackendKind::Tmux, MuxBackendKind::Zellij] {
            assert!(target_matches_anchor(
                backend,
                Some(&target),
                Some(&split_changed_active_pane)
            ));
            assert!(!target_matches_anchor(
                backend,
                Some(&target),
                Some(&other_session)
            ));
            assert!(!target_matches_anchor(backend, Some(&target), None));
        }
    }

    #[test]
    fn backend_owned_ui_launches_normal_backend_attach() {
        assert_eq!(
            backend_attach_launch(MuxBackendKind::Tmux, "agents"),
            (
                "tmux".to_owned(),
                vec![
                    "-T".to_owned(),
                    "256,RGB,clipboard,focus,hyperlinks,overline,strikethrough,sync,title"
                        .to_owned(),
                    "attach-session".to_owned(),
                    "-t".to_owned(),
                    "agents".to_owned()
                ]
            )
        );
        assert_eq!(
            backend_attach_launch(MuxBackendKind::Zellij, "agents"),
            (
                "zellij".to_owned(),
                vec![
                    "attach".to_owned(),
                    "--create".to_owned(),
                    "agents".to_owned()
                ]
            )
        );
    }

    #[test]
    fn backend_owned_ui_removes_nested_backend_environment() {
        assert_eq!(
            backend_attach_env_remove(MuxBackendKind::Tmux),
            vec!["TMUX".to_owned()]
        );
        assert_eq!(
            backend_attach_env_remove(MuxBackendKind::Zellij),
            vec!["ZELLIJ".to_owned()]
        );
    }

    #[test]
    fn attach_keeps_bootty_term_only_when_vendored_terminfo_resolves() {
        let config = TerminalSessionConfig {
            launch: bootty_runtime::SessionLaunchConfig {
                term: "xterm-bootty".to_owned(),
                ..Default::default()
            },
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            benchmark_trace: None,
        };

        let with_terminfo =
            backend_attach_session_config(config.clone(), MuxBackendKind::Tmux, "agents", true)
                .expect("attach config");
        assert_eq!(with_terminfo.launch.term, "xterm-bootty");

        let without_terminfo =
            backend_attach_session_config(config, MuxBackendKind::Tmux, "agents", false)
                .expect("attach config");
        assert_eq!(without_terminfo.launch.term, "xterm-256color");
    }

    #[test]
    fn attach_downgrades_unresolvable_custom_term_to_tmux_compatible() {
        let config = TerminalSessionConfig {
            launch: bootty_runtime::SessionLaunchConfig {
                term: "st-256color".to_owned(),
                ..Default::default()
            },
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            benchmark_trace: None,
        };

        let attach = backend_attach_session_config(config, MuxBackendKind::Tmux, "agents", true)
            .expect("attach config");
        assert_eq!(attach.launch.term, "xterm-256color");
    }

    #[test]
    fn backend_owned_ui_uses_tmux_compatible_term() {
        let mut config = TerminalSessionConfig {
            launch: bootty_runtime::SessionLaunchConfig {
                term: "xterm-bootty".to_owned(),
                ..Default::default()
            },
            colors: TerminalColorConfig::default(),
            cursor: TerminalCursorConfig::default(),
            features: TerminalFeatureConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
            side_effect_tx: None,
            benchmark_trace: None,
        };
        let (program, args) = backend_attach_launch(MuxBackendKind::Tmux, "agents");
        config.launch.shell = Some(program);
        config.launch.args = args;
        config.launch.env_remove = backend_attach_env_remove(MuxBackendKind::Tmux);
        config.launch.term = "xterm-256color".to_owned();

        assert_eq!(config.launch.term, "xterm-256color");
        assert_eq!(config.launch.env_remove, vec!["TMUX".to_owned()]);
    }

    #[test]
    fn backend_attach_program_is_resolved_to_absolute_path() {
        let temp = TempDir::new().unwrap();
        let program = temp.path().join("tmux");
        std::fs::write(&program, "").unwrap();

        let resolved = resolve_launch_program_with_path("tmux", Some(temp.path().as_os_str()))
            .expect("program should resolve from supplied PATH");

        assert_eq!(resolved, program.to_string_lossy());
    }
}
