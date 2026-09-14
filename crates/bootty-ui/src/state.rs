pub mod agent_attention;
mod clipboard;
mod recovery;
mod themes;
use bootty_mux::pane_layout::Divider;
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use crate::gpui::{FrameInputSnapshot, InputEvent, Point, PointerButton};
use crate::product_dialogs::terminal_find::{TerminalFindModel, TerminalFindOutput};
use crate::terminal_text::TerminalTextConfig;
use anyhow::Result;
use bootty_config::config::{MultiplexerBackendConfig, WindowFullscreen};
use bootty_config::{
    config::{AppearanceMode, AppearanceVariant, BoottyConfig, ConfigDocument, ConfigResult},
    config_reload::CONFIG_HOT_RELOAD_INTERVAL,
};
use bootty_control::{CommandInvocation, CommandTarget, ControlEventSender, ResourceKind};
use bootty_mux::{
    RepaintHandle,
    controller::{MuxController, SpaceId},
    provider::{MuxBackendRegistry, selected_backend},
    snapshot::{MuxSession, MuxWindow},
    terminal::{ActiveTerminal, TerminalRuntime, decode_scoped_pane_id},
};
use bootty_terminal::geometry::{CellMetrics, SurfaceRect, TerminalSurface, ViewTransform};
use bootty_terminal::terminal_engine::{
    TerminalSideEffect, TerminalSideEffectEvent, encode_iterm2_report_cell_size,
    encode_iterm2_report_variable, encode_osc52_response,
};
use bootty_terminal::terminal_input::{DirectKeyInput, ModifierSideState};
use bootty_terminal::{
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal_session::DrainStats,
};

mod dialog_runtime;
mod dialogs;
pub mod ditch;
mod input;
mod keybinds;
mod mux_actions;
mod notifications;
pub use dialog_runtime::ModalDialog;
pub use mux_actions::ExactMuxAction;
mod recorded_chord;
mod spaces;

use crate::commands::{CommandRuntime, ExactMuxTarget};
use crate::config_runtime::AppConfigRuntime;
use crate::error_catalog::ErrorNotice;
use crate::keymap_runtime::{KeymapFocus, KeymapRuntime, KeymapSnapshot};
use crate::presentation::dialogs::SpaceMoveTarget;
use crate::terminal_interaction::{TerminalFocusIntent, TerminalInteractionRuntime};
use bootty_mux::terminal_config::terminal_live_config;
use bootty_mux::workspace::{
    BindingSessionGroup, ScopedSessionTarget, TerminalProgress, WorkspaceRuntime,
};
use dialog_runtime::DialogRuntime;
use input::NeutralWheelScrollState;
use keybinds::terminal_cursor_icon_for_mouse_shape;

use crate::{
    diagnostics::StabilityTraceSample,
    frame_facts::RendererMetrics,
    input::focus::InputFocus,
    platform::{read_clipboard_text, write_clipboard_text},
    theme::theme_from_config,
};
use bootty_mux::command::MuxCommand;
use bootty_mux::repository::WorkspacePersistenceError;

const PRIMARY_WINDOW_STATE_KEY: &str = "main";

/// Per-frame snapshot of everything the state machine needs from the host.
#[derive(Clone, Debug)]
pub struct FrameInputs {
    pub now: Instant,
    pub input: FrameInputSnapshot,
    pub viewport: ViewportSnapshot,
    /// Core Graphics display id of the concrete GPUI window that produced this frame.
    pub display_id: Option<u32>,
    pub renderer_metrics: RendererMetrics,
    pub terminal_cell_width: f32,
    pub terminal_cell_height: f32,
    pub terminal_scale_factor: f32,
    pub terminal_view_transform: ViewTransform,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ViewportSnapshot {
    pub fullscreen: bool,
    pub maximized: bool,
    pub content_height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorIcon {
    Default,
    None,
    PointingHand,
    #[default]
    Text,
    VerticalText,
    Crosshair,
    Help,
    Wait,
    Progress,
    Cell,
    Copy,
    Alias,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ResizeHorizontal,
    ResizeVertical,
    ResizeNeSw,
    ResizeNwSe,
    ResizeEast,
    ResizeSouth,
    ResizeWest,
    ResizeNorth,
    ResizeNorthEast,
    ResizeNorthWest,
    ResizeSouthEast,
    ResizeSouthWest,
    ZoomIn,
    ZoomOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnclaimedSession {
    pub session_id: String,
    pub name: String,
}

/// Window and screen facts the chrome layout needs, sampled once per frame outside the paint
/// pass. Each field is measured in points, in screen space; the view adds its own origin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowChromeFacts {
    /// Native or non-native fullscreen: chrome drops its borders and may occupy the notch band.
    pub fullscreen: bool,
    /// The active screen has a notch and we are fullscreen.
    pub notched: bool,
    /// Measured height of the macOS notch band, or 0 when unreadable.
    pub notch_band: f32,
    /// Horizontal span the notch occupies, sampled only while tabs-in-notch is on.
    pub notch_span: Option<(f32, f32)>,
}

impl WindowChromeFacts {
    /// Place a top chrome row inside the display's notch band while keeping the terminal content
    /// below the physical notch. Without notch integration the whole measured band is reserved.
    #[must_use]
    pub fn top_inset(
        self,
        tabs_in_notch: bool,
        chrome_height: f32,
        configured_inset: Option<f32>,
    ) -> f32 {
        if self.notched {
            configured_inset
                .unwrap_or(if tabs_in_notch {
                    self.notch_band - chrome_height
                } else {
                    self.notch_band
                })
                .max(0.0)
        } else {
            0.0
        }
    }
}

/// A file or directory requested on the selected workspace host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFilesRequest {
    pub scope: bootty_mux::controller::SpaceId,
    pub target: bootty_control::CommandTarget,
    pub host: String,
    pub path: String,
    pub document: bool,
    pub line: u32,
    pub column: u32,
}

/// Host actions requested by a frame update, applied by the active window adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum AppEffect {
    Dock(crate::commands::DockRequest),
    CloseWindow,
    OpenWindow,
    OpenSpaceWindow(bootty_mux::controller::SpaceId),
    QuitApplication,
    SetWindowTitle(String),
    SetFullscreen(bool),
    SetMaximized(bool),
    SetDecorations(bool),
    RequestCopy,
    RequestRepaint,
    Bell,
    DesktopNotification {
        title: String,
        body: String,
    },
    RepaintAfter(Duration),
    SetTerminalTextConfig(TerminalTextConfig),
    SetTerminalCursorIcon(CursorIcon),
    /// Reinstall UI-chrome fonts (settings/sidebar/status) so a `font.ui-family` edit applies
    /// live, mirroring how `SetTerminalTextConfig` re-fonts the terminal.
    SetUiFonts(Vec<String>),
    SetUiFontWeights(bootty_config::FontWeightAssignments),
    /// Resize all non-terminal UI text independently from the terminal cell font.
    SetUiFontSize(f32),
    SetWindowFocus,
    FocusTerminal,
    ApplyMacosNonNativeFullscreen,
    RestoreMacosPresentation,
    OpenUrl(String),
    OpenSettings,
    OpenSetting(String),
    OpenFiles(OpenFilesRequest),
    OpenGitChanges {
        scope: bootty_mux::controller::SpaceId,
        target: bootty_control::CommandTarget,
        directory: String,
        host: String,
    },
    CommandAction(crate::gpui::CommandAction),
    /// Open settings to the keybindings page focused on the given action name,
    /// adding an editable row for it if none exists yet.
    ConfigureKeybind(String),
}

pub struct AppState {
    recovery: recovery::RecoveryState,
    image_clipboard: clipboard::ImageClipboard,
    pub(crate) localizer: crate::i18n::Localizer,
    pub(super) window_state_key: String,
    pub(super) commands: CommandRuntime,
    pub(super) workspace: WorkspaceRuntime,
    repaint_scheduler: RepaintScheduler,
    pub(super) last_error: Option<ErrorNotice>,
    last_drain: DrainStats,
    notifications: notifications::TerminalNotifications,
    agent_notifications: agent_attention::AgentNotifications,
    terminal_surface: Option<TerminalSurface>,
    /// Geometry and optional pane identity captured before each pointer event is encoded. Native
    /// split panes render independent runtimes while the aggregate terminal owns keyboard input.
    pending_mouse_input_targets: std::collections::VecDeque<Option<PendingMouseInputTarget>>,
    /// The pane that owns the current pointer press. Motion and release stay with this pane even
    /// after the pointer crosses a split or leaves the terminal surface.
    mouse_input_capture: Option<(PendingMouseInputTarget, PointerButton)>,
    /// The full terminal area the panes were last laid out within, for geometric neighbor lookup.
    last_pane_area: Option<SurfaceRect>,
    terminal_view_transform: ViewTransform,
    config_runtime: AppConfigRuntime,
    keymap_runtime: KeymapRuntime,
    active_appearance_variant: AppearanceVariant,
    input_focus: InputFocus,
    pub(super) repaint: RepaintHandle,
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    modifier_sides: ModifierSideState,
    pending_direct_input: Vec<DirectKeyInput>,
    terminal_interaction: TerminalInteractionRuntime,
    /// Screen rects of chrome resize handles (sidebar edge, pane dividers) registered during the
    /// previous frame's UI build. A primary press inside one of these must not begin a terminal
    /// text selection — the handle owns that drag. Populated each frame in `show_fixed_layout`.
    chrome_handle_rects: Vec<SurfaceRect>,
    /// Held while the status-bar caffeinate toggle is on. The OS assertion is owner state: the
    /// chrome view only reports and toggles it.
    keep_awake: Option<keepawake::KeepAwake>,
    wheel_scroll_state: NeutralWheelScrollState,
    terminal_cursor_icon: CursorIcon,
    mouse_pointer_hidden_while_typing: bool,
    last_mouse_hover_pos: Option<Point>,
    dialogs: DialogRuntime,
    sidebar_hovered_session: Option<ScopedSessionTarget>,
    theme_picker_restore_config: Option<BoottyConfig>,
    macos_non_native_fullscreen_active: bool,
    macos_non_native_fullscreen_pending_apply: bool,
    window_chrome: WindowChromeFacts,
}

#[derive(Clone)]
struct PendingMouseInputTarget {
    owner: Option<CommandTarget>,
    pane_id: Option<String>,
    surface: TerminalSurface,
    view: ViewTransform,
    position: Option<Point>,
}
fn scoped_terminal_transition_key(
    scope: SpaceId,
    backend: MultiplexerBackendConfig,
    session_id: &str,
    pane_id: Option<&str>,
) -> String {
    format!(
        "{}:{backend:?}:{session_id}:{}",
        scope.persistence_value(),
        pane_id.unwrap_or_default(),
    )
}

fn terminal_report_variable_response(name: &str, session_name: Option<&str>) -> Option<Vec<u8>> {
    match name {
        "session.name" => session_name.map(encode_iterm2_report_variable),
        _ => None,
    }
}

/// Where a session starts when nothing else says otherwise.
pub fn default_session_cwd(config: &BoottyConfig) -> String {
    let cwd = config
        .session
        .working_directory
        .clone()
        .or_else(bootty_config::config::default_working_directory)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| {
            config
                .config_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_owned()
        });
    cwd.to_string_lossy().into_owned()
}

impl AppState {
    /// Initialize the application state for the default window.
    ///
    /// # Errors
    /// Returns errors from workspace restoration, backend attachment, or recovery initialization.
    pub fn new(
        config: BoottyConfig,
        backends: Arc<MuxBackendRegistry>,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        Self::new_for_window(
            config,
            PRIMARY_WINDOW_STATE_KEY.to_owned(),
            backends,
            repaint,
            direct_input_rx,
            modifier_side_rx,
        )
    }
    /// Initialize application state for a persisted window identity.
    ///
    /// # Errors
    /// Returns errors from workspace restoration, backend attachment, or recovery initialization.
    pub fn new_for_window(
        config: BoottyConfig,
        window_state_key: String,
        backends: Arc<MuxBackendRegistry>,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        Self::new_for_window_with_agents(
            config,
            window_state_key,
            backends,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            None,
        )
    }

    /// Construct a window with native agent command and event ownership composed by its host.
    /// Tests and headless callers use [`Self::new_for_window`] and receive explicit unsupported
    /// outcomes for the static agent catalog.
    ///
    /// # Errors
    /// Returns errors from workspace restoration, backend attachment, or recovery initialization.
    pub fn new_for_window_with_agents(
        config: BoottyConfig,
        window_state_key: String,
        backends: Arc<MuxBackendRegistry>,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
        agent_events: Option<ControlEventSender>,
    ) -> Result<Self> {
        let config_runtime = AppConfigRuntime::new(config)?;
        let commands = agent_events.map_or_else(
            || CommandRuntime::new(repaint.clone()),
            |events| CommandRuntime::new_with_agents(repaint.clone(), events),
        );
        let keymap_runtime = KeymapRuntime::new(config_runtime.current(), commands.catalog());
        let keymap_diagnostic = keymap_runtime.snapshot().diagnostic_summary();
        let config = config_runtime.current();
        let active_appearance_variant = config.appearance.mode.variant(AppearanceVariant::Dark);
        let workspace = WorkspaceRuntime::open(
            config,
            &window_state_key,
            backends,
            active_appearance_variant,
            repaint.clone(),
        )?;
        commands.refresh_agent_scopes(&workspace);
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
        let macos_non_native_fullscreen_pending_apply = macos_non_native_fullscreen_active;

        Ok(Self {
            localizer: crate::i18n::Localizer::new(&config.locale)?,
            commands,
            workspace,
            repaint_scheduler: RepaintScheduler::default(),
            last_error: keymap_diagnostic.map(ErrorNotice::from_text),
            last_drain: DrainStats::default(),
            recovery: recovery::RecoveryState::new(&window_state_key, repaint.clone())?,
            window_state_key,
            image_clipboard: clipboard::ImageClipboard::default(),
            notifications: notifications::TerminalNotifications::default(),
            agent_notifications: agent_attention::AgentNotifications::default(),
            terminal_surface: None,
            pending_mouse_input_targets: std::collections::VecDeque::new(),
            mouse_input_capture: None,
            last_pane_area: None,
            chrome_handle_rects: Vec::new(),
            keep_awake: None,
            terminal_view_transform: ViewTransform::IDENTITY,
            config_runtime,
            keymap_runtime,
            active_appearance_variant,
            input_focus: InputFocus::Terminal,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            modifier_sides: ModifierSideState::default(),
            pending_direct_input: Vec::new(),
            terminal_interaction: TerminalInteractionRuntime::default(),
            wheel_scroll_state: NeutralWheelScrollState::default(),
            terminal_cursor_icon: CursorIcon::Text,
            mouse_pointer_hidden_while_typing: false,
            last_mouse_hover_pos: None,
            dialogs: DialogRuntime::default(),
            sidebar_hovered_session: None,
            theme_picker_restore_config: None,
            macos_non_native_fullscreen_active,
            macos_non_native_fullscreen_pending_apply,
            window_chrome: WindowChromeFacts::default(),
        })
    }
    pub const fn config(&self) -> &BoottyConfig {
        self.config_runtime.current()
    }

    pub fn keymap_snapshot(&self) -> KeymapSnapshot {
        self.keymap_runtime.snapshot().clone()
    }

    pub fn keymap_focus(&self) -> KeymapFocus {
        if let Some(dialog) = self.modal_dialog() {
            return if matches!(dialog, ModalDialog::SpaceEditor(_)) {
                KeymapFocus::Other
            } else {
                KeymapFocus::Command
            };
        }
        match self.input_focus {
            InputFocus::Terminal => KeymapFocus::Terminal,
            InputFocus::Sidebar => KeymapFocus::Sidebar,
            InputFocus::Find => KeymapFocus::Other,
        }
    }

    /// Persist a keymap edit and publish its diagnostics.
    ///
    /// # Errors
    /// Returns validation, concurrency, and persistence errors from the keymap writer.
    pub fn edit_keymap(
        &mut self,
        edit: &bootty_config::keymap_file::KeymapEdit,
    ) -> Result<bootty_config::keymap_file::KeymapWriteOutcome> {
        let outcome = self.keymap_runtime.edit(edit)?;
        if let Some(warning) = outcome.durability_warning() {
            self.record_error(warning);
        } else if let Some(diagnostic) = self.keymap_runtime.snapshot().diagnostic_summary() {
            self.record_error(diagnostic);
        }
        Ok(outcome)
    }

    /// Reload the keymap and publish compilation diagnostics.
    ///
    /// # Errors
    /// Returns an error when the keymap file cannot be loaded.
    pub fn reload_keymap(&mut self) -> Result<()> {
        if let Some(diagnostic) = self.keymap_runtime.reload()? {
            self.record_error(diagnostic);
        }
        Ok(())
    }

    /// Identifies the accepted config/document pair, so a view can tell whether the copy it
    /// already holds is current.
    pub const fn config_revision(&self) -> u64 {
        self.config_runtime.revision()
    }

    /// The accepted native settings schema.
    pub(crate) fn settings_schema(
        &self,
    ) -> std::sync::Arc<bootty_config::settings_schema::SettingsSchema> {
        self.config_runtime.settings_schema()
    }

    pub(crate) fn config_document(&self) -> ConfigDocument {
        self.config_runtime.document().clone()
    }

    pub(crate) fn commit_settings_document(
        &mut self,
        document: ConfigDocument,
    ) -> Result<(ConfigDocument, Option<String>, Vec<AppEffect>)> {
        let backend = self.workspace.active.binding.multiplexer().backend;
        let (change, document, outcome) = self.config_runtime.commit_document(
            document,
            backend,
            self.active_appearance_variant,
        )?;
        let warning = outcome.durability_warning().map(str::to_owned);
        let mut effects = Vec::new();
        self.apply_accepted_config(change, &mut effects);
        let config = self.config().clone();
        self.keymap_runtime.sync_config(&config);
        if let Some(warning) = &warning {
            let message = self.last_error.take().map_or_else(
                || warning.clone(),
                |existing| format!("{}; {warning}", existing.raw_message()),
            );
            self.record_error(message);
        }
        Ok((document, warning, effects))
    }

    fn mutate_config_document(
        &mut self,
        mutate: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>,
        effects: &mut Vec<AppEffect>,
    ) {
        let mut document = self.config_runtime.document().clone();
        if let Err(error) = mutate(&mut document) {
            self.record_error(error);
            return;
        }
        match self.commit_settings_document(document) {
            Ok((_, _, accepted_effects)) => effects.extend(accepted_effects),
            Err(error) => self.record_error(error),
        }
    }

    /// Apply a dragged sidebar width to the live config without touching disk, so the layout
    /// tracks the pointer each frame. [`Self::persist_sidebar_width`] writes the final value.
    pub fn set_sidebar_width_live(&mut self, width: f32) {
        self.config_runtime.set_sidebar_width(width);
    }

    /// Persist the sidebar width to `config.toml` on drag release. The live value already matches,
    /// so the hot-reload baseline is refreshed to skip the redundant reload the write would trigger.
    pub fn persist_sidebar_width(&mut self, width: f32, effects: &mut Vec<AppEffect>) {
        self.mutate_config_document(
            |document| document.set_f32(&["chrome", "sidebar-width"], width),
            effects,
        );
    }

    fn set_runtime_fullscreen(&mut self, enabled: bool, effects: &mut Vec<AppEffect>) {
        let mode = match self.config().window.fullscreen {
            WindowFullscreen::Disabled => WindowFullscreen::Native,
            mode => mode,
        };
        let was_non_native = self.macos_non_native_fullscreen_active;
        self.macos_non_native_fullscreen_active = enabled
            && matches!(
                mode,
                WindowFullscreen::NonNative
                    | WindowFullscreen::NonNativeVisibleMenu
                    | WindowFullscreen::NonNativePaddedNotch
            );

        if self.macos_non_native_fullscreen_active {
            effects.push(AppEffect::SetFullscreen(false));
            effects.push(AppEffect::ApplyMacosNonNativeFullscreen);
            if !crate::platform::macos_handles_non_native_fullscreen_frame(&self.config().window) {
                effects.push(AppEffect::SetMaximized(true));
            }
        } else {
            if was_non_native {
                effects.push(AppEffect::RestoreMacosPresentation);
            }
            effects.push(AppEffect::SetFullscreen(
                enabled && mode == WindowFullscreen::Native,
            ));
            if was_non_native
                && !crate::platform::macos_handles_non_native_fullscreen_frame(
                    &self.config().window,
                )
            {
                effects.push(AppEffect::SetMaximized(false));
            }
        }
    }
    fn persist_appearance_mode(&mut self, mode: AppearanceMode, effects: &mut Vec<AppEffect>) {
        let token = match mode {
            AppearanceMode::System => "system",
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
        };
        self.mutate_config_document(
            |document| document.set_str(&["appearance", "mode"], token),
            effects,
        );
    }
    fn persist_active_theme(&mut self, theme: &str, effects: &mut Vec<AppEffect>) {
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "light",
            AppearanceVariant::Dark => "dark",
        };
        self.mutate_config_document(
            |document| document.set_str(&["appearance", branch, "theme"], theme),
            effects,
        );
    }
    fn preview_active_theme(&mut self, theme: &str, effects: &mut Vec<AppEffect>) {
        let path = self.config().config_path.clone();
        let Some(config_dir) = path.parent() else {
            return;
        };
        let resolved = match bootty_config::config::resolve_theme(theme, config_dir) {
            Ok(theme) => theme,
            Err(error) => {
                self.record_error(error);
                return;
            }
        };
        let variant = self.active_appearance_variant;
        let mut config = self.config().clone();
        let branch = match variant {
            AppearanceVariant::Light => &mut config.appearance.light,
            AppearanceVariant::Dark => &mut config.appearance.dark,
        };
        branch.theme = Some(theme.to_owned());
        branch.colors = resolved.colors;
        self.config_runtime.replace_preview_config(config);
        self.publish_live_terminal_config(variant);
        effects.push(AppEffect::RequestRepaint);
    }
    fn restore_theme_picker_preview(&mut self) -> bool {
        let Some(config) = self.theme_picker_restore_config.clone() else {
            return false;
        };
        self.config_runtime.replace_preview_config(config);
        self.publish_live_terminal_config(self.active_appearance_variant);
        true
    }

    fn publish_live_terminal_config(&mut self, variant: AppearanceVariant) {
        let config = self.config().clone();
        let live_config = terminal_live_config(&config, variant);
        let warnings = self
            .workspace
            .publish_terminal_config(&config, variant, Some(&live_config));
        if !warnings.is_empty() {
            self.record_error(warnings.join("; "));
        }
    }
    pub fn theme_picker_preview_active(&self) -> bool {
        self.theme_picker_restore_config.is_some() && self.dialogs.is_theme_picker()
    }
    pub fn set_appearance_variant(&mut self, variant: AppearanceVariant) {
        if self.active_appearance_variant == variant {
            return;
        }
        self.active_appearance_variant = variant;
        self.publish_live_terminal_config(variant);
    }
    pub const fn active_appearance_variant(&self) -> AppearanceVariant {
        self.active_appearance_variant
    }
    pub fn ui_theme(&self) -> crate::gpui::UiTheme {
        theme_from_config(self.config(), self.active_appearance_variant)
    }

    pub(crate) fn black_notch_chrome(&self) -> bool {
        self.window_chrome_facts().notched
            && self.active_appearance_variant() == AppearanceVariant::Dark
            && self.config().chrome.notched_fullscreen_black_chrome
    }
    pub const fn mux(&self) -> &MuxController {
        self.workspace.active.binding.mux()
    }
    pub const fn mux_scope(&self) -> SpaceId {
        self.workspace.active.binding.scope()
    }
    pub const fn active_space_id(&self) -> SpaceId {
        self.workspace.active_space_id()
    }
    pub fn binding_session_groups(&self) -> Vec<BindingSessionGroup> {
        self.workspace.active_binding_session_groups()
    }

    /// Every session the workspace can reach, grouped by the Space that owns it, with a trailing
    /// group for the sessions no Space claims. The finder needs the owner to know whether selecting a
    /// session means switching Spaces or adopting the session into the current one; the sidebar stays
    /// on `binding_session_groups`, which is this Space only.
    pub fn session_finder_groups(&self) -> Vec<BindingSessionGroup> {
        self.workspace.session_finder_groups()
    }
    pub(super) const fn active_multiplexer(&self) -> &bootty_config::config::MultiplexerConfig {
        self.workspace.active.binding.multiplexer()
    }
    pub const fn multiplexer_backend(&self) -> bootty_config::config::MultiplexerBackendConfig {
        self.workspace.active.binding.multiplexer().backend
    }
    pub fn terminal_transition_key(&self) -> Option<String> {
        self.workspace
            .active
            .binding
            .mux()
            .selected_session_anchor()
            .map(|anchor| {
                scoped_terminal_transition_key(
                    self.workspace.active.binding.scope(),
                    selected_backend(self.active_multiplexer()),
                    &anchor.session_id,
                    anchor.pane_id.as_deref(),
                )
            })
    }
    pub fn last_error(&self) -> Option<String> {
        self.workspace
            .active
            .binding
            .mux()
            .last_error()
            .map(str::to_owned)
            .or_else(|| self.last_error.as_ref().map(ErrorNotice::raw_message))
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Reporting consumes errors at the application boundary while accepting borrowed diagnostics"
    )]
    pub(crate) fn record_error(&mut self, error: impl ToString) {
        self.last_error = Some(ErrorNotice::from_text(error.to_string()));
    }
    pub(crate) fn record_notice(&mut self, notice: ErrorNotice) {
        self.last_error = Some(notice);
    }
    pub fn clear_last_error(&mut self) {
        self.workspace.active.binding.mux_mut().set_error(None);
        self.last_error = None;
    }
    pub const fn macos_non_native_fullscreen_active(&self) -> bool {
        self.macos_non_native_fullscreen_active
    }

    pub const fn window_chrome_facts(&self) -> WindowChromeFacts {
        self.window_chrome
    }

    /// Read this frame's window/screen facts and re-assert the window chrome `AppKit` resets across
    /// fullscreen transitions. Runs once per frame in the update phase, never from paint.
    fn sample_window_chrome(
        &mut self,
        viewport: ViewportSnapshot,
        display_id: Option<u32>,
        window_focused: bool,
    ) {
        let fullscreen = self.macos_non_native_fullscreen_active || viewport.fullscreen;
        if window_focused && fullscreen {
            crate::window::macos_disable_titlebar_separator();
        }

        let screen = if fullscreen {
            crate::window::macos_screen_facts(display_id)
        } else {
            crate::window::MacosScreenFacts::default()
        };
        let notch_span = (screen.notched && self.config().window.fullscreen_tabs_in_notch)
            .then_some(screen.notch_span)
            .flatten();
        self.window_chrome = WindowChromeFacts {
            fullscreen,
            notched: screen.notched,
            notch_band: screen.notch_height,
            notch_span,
        };
    }
    fn sync_macos_non_native_fullscreen_presentation(&mut self, effects: &mut Vec<AppEffect>) {
        if !self.macos_non_native_fullscreen_active {
            return;
        }
        if self.macos_non_native_fullscreen_pending_apply {
            self.macos_non_native_fullscreen_pending_apply = false;
            effects.push(AppEffect::ApplyMacosNonNativeFullscreen);
        }
    }
    pub fn terminal_mut(&mut self) -> &mut ActiveTerminal {
        self.workspace.active.binding.terminal_mut()
    }
    pub const fn record_surface(&mut self, surface: TerminalSurface) {
        self.terminal_surface = Some(surface);
    }
    /// Preserve the hit pane's presented geometry until its pointer event is encoded.
    pub fn record_mouse_input_target(&mut self, surface: TerminalSurface, view: ViewTransform) {
        self.record_mouse_input_target_for_pane(None, surface, view, None);
    }
    /// Preserve the hit pane's identity and presented geometry until its pointer event is encoded.
    pub fn record_mouse_input_target_for_pane(
        &mut self,
        pane_id: Option<String>,
        surface: TerminalSurface,
        view: ViewTransform,
        position: Option<Point>,
    ) {
        let owner = self.mouse_input_owner();
        self.pending_mouse_input_targets
            .push_back(Some(PendingMouseInputTarget {
                owner,
                pane_id,
                surface,
                view,
                position,
            }));
    }
    fn mouse_input_owner(&self) -> Option<CommandTarget> {
        self.current_command_target(ResourceKind::MuxWindow)
            .or_else(|| self.current_command_target(ResourceKind::Binding))
    }
    /// Record a pointer event that hit no terminal pane. An explicit empty target is different
    /// from an unrecorded target: the latter is the compatibility fallback for frame-level tests
    /// and non-native terminal input.
    pub fn record_mouse_input_target_none(&mut self) {
        self.pending_mouse_input_targets.push_back(None);
    }
    pub fn record_render_error(&mut self, error: impl ToString) {
        self.record_error(error);
    }

    pub const fn keep_awake_active(&self) -> bool {
        self.keep_awake.is_some()
    }

    /// Take or release the display/idle sleep assertion behind the caffeinate status item.
    pub fn toggle_keep_awake(&mut self) {
        if self.keep_awake.take().is_some() {
            return;
        }
        match keepawake::Builder::default()
            .display(true)
            .idle(true)
            .reason("Bootty status-bar toggle")
            .app_name("Bootty")
            .app_reverse_domain("dev.bootty")
            .create()
        {
            Ok(guard) => self.keep_awake = Some(guard),
            Err(error) => self.record_render_error(error),
        }
    }

    /// Replace the frame's chrome-handle rects with the ones chrome just painted, so the next
    /// input pass suppresses selection over live handles only. A frame that paints no chrome (the
    /// settings surface) leaves the previous set in place.
    pub fn set_chrome_handles(&mut self, rects: Vec<SurfaceRect>) {
        self.chrome_handle_rects = rects;
    }

    /// Add a handle painted after chrome, during the terminal pass (the pane dividers).
    pub fn register_chrome_handle(&mut self, rect: SurfaceRect) {
        self.chrome_handle_rects.push(rect);
    }
    pub(super) fn uses_native_terminal_layout(&self) -> bool {
        self.workspace.active.binding.uses_native_terminal_layout()
    }
    pub fn pane_widget_key(&self, pane_id: &str) -> String {
        self.workspace.active.binding.pane_widget_key(pane_id)
    }
    fn sync_terminal_panes(&mut self) -> Result<()> {
        self.workspace.sync_active_terminal_panes()
    }

    fn publish_backend_transition(&mut self) {
        self.terminal_surface = None;
        // The corresponding pointer events remain queued in the host. Preserve their slots
        // as rejected hits so they cannot fall back to the newly active terminal.
        for target in &mut self.pending_mouse_input_targets {
            *target = None;
        }
        self.mouse_input_capture = None;
        self.last_pane_area = None;
    }

    fn sync_terminal_panes_or_record_error(&mut self) {
        if let Err(error) = self.sync_terminal_panes() {
            self.record_error(error);
        }
    }
    pub fn native_multi_pane(&self) -> bool {
        self.workspace.active.binding.native_multi_pane()
    }
    pub fn focused_pane(&self) -> Option<String> {
        self.workspace.active.binding.focused_pane()
    }
    pub(crate) fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.workspace.active.binding.pane_progress(pane_id)
    }
    pub(crate) fn session_ports(&self, session: &MuxSession) -> Vec<u16> {
        self.workspace.active.binding.session_ports(session)
    }
    /// The names the active binding shows for `sessions`, in the same order.
    pub(crate) fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        self.workspace
            .active
            .binding
            .session_display_names(sessions)
    }
    pub(crate) fn window_has_indeterminate_progress(&self, window: &MuxWindow) -> bool {
        self.workspace
            .active
            .binding
            .window_has_indeterminate_progress(window)
    }
    pub(crate) fn window_progress(&self, window: &MuxWindow) -> Option<u8> {
        self.workspace.active.binding.window_progress(window)
    }
    pub fn pane_rects(&self, area: SurfaceRect, gap: f32) -> Vec<(String, SurfaceRect)> {
        self.workspace.active.binding.pane_rects(area, gap)
    }
    pub fn pane_dividers(&self, area: SurfaceRect, gap: f32) -> Vec<Divider> {
        self.workspace.active.binding.pane_dividers(area, gap)
    }
    pub fn focus_pane(&mut self, pane_id: &str) {
        self.workspace.active.binding.focus_pane(pane_id);
    }
    pub fn set_pane_ratio(&mut self, path: &[u8], ratio: f32, min_fraction: f32) {
        if self.pane_arrangement_pending() {
            return;
        }
        self.workspace
            .active
            .binding
            .set_pane_ratio(path, ratio, min_fraction);
    }
    pub fn terminal_runtime_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.workspace
            .active
            .binding
            .terminal_runtime_for_pane(pane_id)
    }
    pub fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.workspace
            .active
            .binding
            .pane_terminal_window_size(leaf_size)
    }
    /// Resize the active native terminal layout.
    ///
    /// # Errors
    /// Returns an error when the backend cannot resize its window.
    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.workspace
            .active
            .binding
            .resize_native_layout_window(cols, rows)
    }
    pub(super) fn sync_terminal_panes_now(&mut self) {
        self.sync_terminal_panes_or_record_error();
    }
    pub const fn record_pane_area(&mut self, area: SurfaceRect) {
        self.last_pane_area = Some(area);
    }
    pub fn activate_scoped_session_from_ui(&mut self, target: &ScopedSessionTarget) -> bool {
        let started = crate::diagnostics::latency_start();
        // A session that belongs to another Space is switched to there, not dragged over here: its
        // connection, terminal, and pane layout all live in that Space.
        if target.scope != self.workspace.active.id && !self.activate_space_from_ui(target.scope) {
            return false;
        }
        if let Err(error) =
            self.workspace
                .activate_target(target.scope, &target.session_id, None, &self.repaint)
        {
            self.record_error(error);
            return false;
        }
        self.sync_terminal_panes_now();
        self.sidebar_hovered_session = Some(target.clone());
        (self.repaint)();
        crate::diagnostics::trace_phase("session.activate_and_sync", started);
        true
    }
    pub fn activate_session_from_ui(&mut self, session_id: &str) {
        let target = ScopedSessionTarget::new(self.workspace.active.binding.scope(), session_id);
        self.activate_scoped_session_from_ui(&target);
    }
    pub fn activate_relative_session_from_ui(&mut self, session_id: &str, delta: isize) -> bool {
        let Some(session_id) = self
            .workspace
            .active
            .binding
            .relative_session_id(session_id, delta)
        else {
            return false;
        };
        self.activate_session_from_ui(&session_id);
        true
    }
    pub fn activate_relative_scoped_session_from_ui(
        &mut self,
        target: &ScopedSessionTarget,
        delta: isize,
    ) -> bool {
        if !self.activate_scoped_session_from_ui(target) {
            return false;
        }
        self.activate_relative_session_from_ui(&target.session_id, delta)
    }
    pub fn activate_last_session_from_ui(&mut self) -> bool {
        let Some(session_id) = self.workspace.active.binding.previous_session_id() else {
            return false;
        };
        self.activate_session_from_ui(&session_id);
        true
    }
    pub(crate) fn apply_exact_mux_action(
        &mut self,
        action: ExactMuxAction,
        target: ExactMuxTarget,
    ) -> bool {
        let Some(command) = self.plan_exact_mux_action(action, &target) else {
            return false;
        };
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => {
                if let Err(error) = self.workspace.activate_target(
                    target.scope(),
                    &session_id,
                    Some(&window_id),
                    &self.repaint,
                ) {
                    self.record_error(error);
                    return false;
                }
                self.sync_terminal_panes_now();
            }
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id),
            } => {
                let (ExactMuxTarget::Window(_, _, window_id)
                | ExactMuxTarget::Pane(_, _, window_id, _)) = target
                else {
                    return false;
                };
                let (window, target_is_current) = {
                    let binding = &self.workspace.active.binding;
                    let window = binding.window_id(session_id.clone(), window_id);
                    let target_is_current = binding.current_window_id() == window;
                    (window, target_is_current)
                };
                if let Err(error) = bootty_mux::executor::execute_local_command(
                    &mut self.workspace,
                    &self.repaint,
                    MuxCommand::ClosePane {
                        session_id,
                        pane_id: Some(pane_id.clone()),
                    },
                ) {
                    self.record_error(error);
                    return false;
                }
                let binding = &mut self.workspace.active.binding;
                binding.terminal_mut().discard_pane(&pane_id);
                if binding.uses_native_terminal_layout() {
                    binding.remove_pane_from_layout(&window, &pane_id, target_is_current);
                }
            }
            command => self.execute_mux_command(command),
        }
        true
    }
    pub fn reorder_window_before_from_ui(&mut self, source: &str, before: Option<&str>) -> bool {
        let changed =
            self.workspace
                .active
                .binding
                .reorder_window_before(&self.repaint, source, before);
        if changed {
            self.sync_terminal_panes_now();
        }
        changed
    }
    fn create_project_session_for_cwd(&mut self, cwd: &str) {
        let command = self.workspace.project_session_command(cwd);
        self.execute_mux_command(command);
    }
    fn move_selected_session(&mut self, delta: i32) -> bool {
        let Some(selected) = self
            .workspace
            .active
            .binding
            .mux()
            .selected_session()
            .map(str::to_owned)
        else {
            return false;
        };
        self.move_session_from_ui(&selected, delta)
    }
    pub fn move_session_from_ui(&mut self, session_id: &str, delta: i32) -> bool {
        let result = self.workspace.move_active_session(session_id, delta);
        self.apply_workspace_change(result)
    }
    pub fn reorder_session_before(&mut self, source: &str, target: Option<&str>) -> bool {
        let result = self.workspace.reorder_active_session_before(source, target);
        self.apply_workspace_change(result)
    }
    fn apply_workspace_change(&mut self, result: Result<bool, WorkspacePersistenceError>) -> bool {
        match result {
            Ok(changed) => changed,
            Err(error) => {
                self.record_error(error);
                false
            }
        }
    }
    pub fn detach_scoped_session_from_space(&mut self, target: &ScopedSessionTarget) -> bool {
        let result = self.workspace.detach_session_from_space(
            target.scope,
            &target.session_id,
            &self.repaint,
        );
        let changed = self.apply_workspace_change(result);
        if changed {
            (self.repaint)();
        }
        changed
    }
    pub fn move_scoped_session_to_space(
        &mut self,
        target: &ScopedSessionTarget,
        space_id: SpaceId,
    ) -> bool {
        let result = self.workspace.move_session_to_space(
            target.scope,
            &target.session_id,
            space_id,
            &self.repaint,
        );
        let changed = self.apply_workspace_change(result);
        if changed {
            (self.repaint)();
        }
        changed
    }

    /// Sessions on the active Space's multiplexer that no Space claims.
    ///
    /// Membership is explicit, so these would otherwise be invisible: ones started outside bootty,
    /// and ones a deleted Space left running. A direct backend cannot stamp membership, so its
    /// authoritative sessions already belong to the active binding and never appear here.
    pub fn unclaimed_sessions(&self) -> Vec<UnclaimedSession> {
        if !self.workspace.active.binding.tracks_session_membership() {
            return Vec::new();
        }
        let claimed = self
            .workspace
            .all_bindings()
            .flat_map(|binding| binding.sessions().sessions())
            .map(|session| session.identity.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.workspace
            .active
            .binding
            .mux()
            .all_sessions()
            .iter()
            .filter(|session| {
                session
                    .tag
                    .identity
                    .as_deref()
                    .is_none_or(|identity| !claimed.contains(identity))
            })
            .map(|session| UnclaimedSession {
                session_id: session.id.clone(),
                name: session.name.clone(),
            })
            .collect()
    }

    /// Claim a session into the active Space and open it.
    pub fn adopt_and_activate_scoped_session(&mut self, target: &ScopedSessionTarget) -> bool {
        let adopted = self.workspace.adopt_session_into_binding(
            target.scope,
            &target.session_id,
            &self.repaint,
        );
        if !self.apply_workspace_change(adopted) {
            return false;
        }
        self.activate_scoped_session_from_ui(target)
    }

    /// What bootty calls `target`, or `None` if the backend no longer has it.
    pub fn session_display_name(&self, target: &ScopedSessionTarget) -> Option<String> {
        let binding = self.workspace.binding(target.scope)?;
        let session = binding
            .mux()
            .backend_session_by_id_or_name(&target.session_id)?;
        Some(
            session
                .tag
                .identity
                .as_deref()
                .and_then(|identity| binding.sessions().get(identity))
                .map_or(session.name.as_str(), |claimed| claimed.label())
                .to_owned(),
        )
    }

    /// The Spaces `target` could move to, in switcher order, for the sidebar's move menu.
    pub fn session_move_targets(&self, target: &ScopedSessionTarget) -> Vec<SpaceMoveTarget> {
        self.workspace
            .spaces()
            .map(|space| SpaceMoveTarget {
                id: space.id,
                name: space.name.clone(),
                icon: space.icon.clone(),
                reachable: self
                    .workspace
                    .session_move_is_possible(target.scope, space.id),
                current: space.binding.scope() == target.scope,
            })
            .collect()
    }

    pub const fn take_terminal_find_dialog(&mut self) -> Option<TerminalFindModel> {
        self.terminal_interaction.take_find_dialog()
    }
    pub fn terminal_find_projection(&self) -> Option<crate::gpui::DialogSpec> {
        self.terminal_interaction.find_dialog().map(|model| {
            let mut spec = crate::presentation::dialogs::terminal_find_spec(model);
            spec.title = self.localizer.message("find-title", None);
            spec.text_hint = Some(spec.title.clone());
            for row in &mut spec.rows {
                let key = match row.id.0.as_str() {
                    "previous" => "find-previous",
                    "next" => "find-next",
                    "regex" => "find-regex",
                    "case_sensitive" => "find-case",
                    _ => continue,
                };
                row.label = self.localizer.message(key, None);
            }
            spec
        })
    }
    pub(crate) fn terminal_find_error(&self) -> Option<&str> {
        self.terminal_interaction
            .find_dialog()
            .and_then(TerminalFindModel::error)
    }
    pub fn apply_terminal_find_dialog_intent(&mut self, intent: &crate::gpui::DialogIntent) {
        if let crate::gpui::DialogIntent::Activate { dialog, action, .. } = intent
            && dialog.0 == crate::presentation::dialogs::TERMINAL_FIND_ID
        {
            let command = match action.0.as_str() {
                "regex" => Some("toggle_search_regex"),
                "case_sensitive" => Some("toggle_search_case_sensitive"),
                _ => None,
            };
            if let Some(command) = command {
                self.commands.queue(CommandInvocation::from_action(
                    command,
                    bootty_control::Caller::Internal,
                ));
                return;
            }
        }
        let Some(mut dialog) = self.terminal_interaction.take_find_dialog() else {
            return;
        };
        let Some(event) =
            crate::presentation::dialogs::apply_terminal_find_intent(&mut dialog, intent)
        else {
            self.terminal_interaction.restore_find_dialog(dialog);
            return;
        };
        self.apply_terminal_find_event(dialog, event);
    }
    pub fn apply_terminal_find_event(
        &mut self,
        dialog: TerminalFindModel,
        event: TerminalFindOutput,
    ) {
        let focused_pane_id = self.focused_pane();
        let outcome = self.terminal_interaction.apply_find_event(
            self.workspace.active.binding.terminal_mut(),
            dialog,
            event,
            focused_pane_id.as_deref(),
        );
        self.apply_terminal_outcome(outcome.last_error, outcome.focus_intent);
    }
    fn apply_terminal_outcome(
        &mut self,
        last_error: Option<String>,
        focus_intent: TerminalFocusIntent,
    ) {
        if let Some(error) = last_error {
            self.record_error(error);
        }
        self.apply_terminal_focus_intent(focus_intent);
    }
    const fn apply_terminal_focus_intent(&mut self, intent: TerminalFocusIntent) {
        match intent {
            TerminalFocusIntent::None => {}
            TerminalFocusIntent::Terminal => self.input_focus = InputFocus::Terminal,
            TerminalFocusIntent::Find => self.input_focus = InputFocus::Find,
        }
    }
    fn drain_terminal_side_effects(
        &mut self,
        side_effects: Vec<TerminalSideEffectEvent>,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        for side_effect in side_effects {
            self.apply_terminal_side_effect_event(
                side_effect,
                effects,
                terminal_cell_width,
                terminal_cell_height,
                terminal_scale_factor,
            );
        }
    }
    fn apply_terminal_side_effect_event(
        &mut self,
        event: TerminalSideEffectEvent,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        let TerminalSideEffectEvent {
            source_pane_id,
            effect,
            ..
        } = event;
        let source_pane_id = match source_pane_id {
            Some(source_pane_id) => {
                if let Some((scope, pane_id)) = decode_scoped_pane_id(&source_pane_id) {
                    if scope != self.workspace.active.binding.scope() {
                        return;
                    }
                    Some(pane_id)
                } else {
                    Some(source_pane_id)
                }
            }
            None => None,
        };
        match effect {
            TerminalSideEffect::ClipboardWrite(text) => {
                if let Err(error) = write_clipboard_text(&text) {
                    self.record_error(error);
                }
            }
            TerminalSideEffect::ClipboardQuery { selection } => {
                self.reply_clipboard_query(&selection);
            }
            TerminalSideEffect::WindowTitle(title) => {
                self.apply_terminal_window_title(source_pane_id.as_deref(), title, effects);
            }
            TerminalSideEffect::DesktopNotification { title, body } => {
                effects.push(AppEffect::DesktopNotification { title, body });
            }
            TerminalSideEffect::MouseShape(shape) => {
                if let Some(icon) = terminal_cursor_icon_for_mouse_shape(&shape) {
                    self.terminal_cursor_icon = icon;
                    effects.push(AppEffect::SetTerminalCursorIcon(
                        self.effective_terminal_cursor_icon(),
                    ));
                }
            }
            TerminalSideEffect::OpenUrl(url) => effects.push(AppEffect::OpenUrl(url)),
            TerminalSideEffect::FocusWindow => effects.push(AppEffect::SetWindowFocus),
            TerminalSideEffect::ReportCellSize => {
                let response = encode_iterm2_report_cell_size(
                    terminal_cell_width,
                    terminal_cell_height,
                    terminal_scale_factor,
                );
                self.reply_terminal(&response);
            }
            TerminalSideEffect::ReportVariable(name) => {
                if let Some(response) = terminal_report_variable_response(
                    &name,
                    self.workspace.active.binding.mux().selected_session(),
                ) {
                    self.reply_terminal(&response);
                }
            }
            TerminalSideEffect::ConEmuProgress { state, value } => {
                self.workspace.active.binding.record_terminal_progress(
                    source_pane_id.as_deref(),
                    &state,
                    value,
                );
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalSideEffect::Iterm2UserVarPorts(ports) => {
                self.workspace
                    .active
                    .binding
                    .record_terminal_ports(source_pane_id.as_deref(), ports);
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalSideEffect::ClipboardPacket(_)
            | TerminalSideEffect::ClipboardReset
            | TerminalSideEffect::Bell
            | TerminalSideEffect::ShellLifecycle(_)
            | TerminalSideEffect::ShellPrompt(_)
            | TerminalSideEffect::WindowIcon(_)
            | TerminalSideEffect::SemanticPrompt(_)
            | TerminalSideEffect::KittyTextSizing(_)
            | TerminalSideEffect::ConEmuControl(_)
            | TerminalSideEffect::Iterm2Control(_)
            | TerminalSideEffect::Iterm2File(_)
            | TerminalSideEffect::UnsupportedHostCommand { .. } => {}
        }
    }
    fn reply_terminal(&mut self, bytes: &[u8]) {
        if let Err(error) = self
            .workspace
            .active
            .binding
            .terminal_mut()
            .write_input(bytes)
        {
            self.record_error(error);
        }
    }

    fn reply_clipboard_query(&mut self, selection: &str) {
        match read_clipboard_text() {
            Ok(Some(text)) => {
                if let Err(error) = self
                    .workspace
                    .active
                    .binding
                    .terminal_mut()
                    .write_input(&encode_osc52_response(selection, &text))
                {
                    self.record_error(error);
                }
            }
            Ok(None) => {}
            Err(error) => self.record_error(error),
        }
    }
    fn apply_terminal_window_title(
        &mut self,
        source_pane_id: Option<&str>,
        title: String,
        effects: &mut Vec<AppEffect>,
    ) {
        self.workspace.active.binding.apply_window_title(
            source_pane_id,
            title.clone(),
            &self.repaint,
        );
        if source_pane_id.is_none()
            || self.workspace.active.binding.terminal().focused_pane_id() == source_pane_id
        {
            effects.push(AppEffect::SetWindowTitle(title));
        }
    }
    pub fn update_frame(&mut self, inputs: FrameInputs) -> Vec<AppEffect> {
        let frame_started = crate::diagnostics::latency_start();
        let FrameInputs {
            now,
            input,
            viewport,
            display_id,
            renderer_metrics,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
            terminal_view_transform,
        } = inputs;
        let FrameInputSnapshot {
            events,
            dropped_file_paths,
            modifiers,
            hover_position,
            pressed_mouse_button,
            window_focused,
        } = input;
        let mut effects = Vec::new();

        self.process_frame_commands(viewport, window_focused, &mut effects);

        self.sync_macos_non_native_fullscreen_presentation(&mut effects);
        self.update_terminal_metrics(
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        let drain = self.workspace.drain();
        self.last_drain = drain.active_drain;
        self.retain_terminal_notifications();
        self.poll_image_clipboard(now);
        self.poll_recovery(now);
        let scope = self.workspace.active.binding.scope();
        let generation = self.workspace.active.binding.mux().binding_generation();
        for (scope, generation, event) in &drain.terminal_notifications {
            self.apply_image_clipboard(*scope, *generation, event, now);
            self.apply_terminal_notification(
                *scope,
                *generation,
                event,
                now,
                window_focused,
                &mut effects,
            );
        }
        for event in &drain.active_terminal_side_effects {
            self.apply_image_clipboard(scope, generation, event, now);
            self.apply_terminal_notification(
                scope,
                generation,
                event,
                now,
                window_focused,
                &mut effects,
            );
        }
        self.drain_terminal_side_effects(
            drain.active_terminal_side_effects,
            &mut effects,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        let frame_config = self.config().clone();
        let workspace_frame = self.workspace.advance_frame(
            &frame_config,
            self.active_appearance_variant,
            &self.repaint,
            now,
            window_focused,
        );
        self.commands.refresh_agent_scopes(&self.workspace);
        if let Some(after) = workspace_frame.next_wake {
            effects.push(AppEffect::RepaintAfter(after));
        }
        self.hot_reload_config_if_changed(&mut effects, now);
        self.hot_reload_keymap_if_changed(now);
        for error in workspace_frame.errors {
            self.record_error(error);
        }
        self.terminal_view_transform = terminal_view_transform;
        self.restore_mouse_pointer_after_pointer_moved(&events, hover_position, &mut effects);
        let input_commands = self
            .handle_direct_input(viewport, &mut effects)
            .saturating_add(self.handle_input(
                events,
                modifiers,
                hover_position,
                pressed_mouse_button,
                viewport,
                &mut effects,
            ))
            .saturating_add(self.handle_dropped_file_paths(&dropped_file_paths));
        // Sampled last: this frame's keybinds may have toggled fullscreen and its config reload may
        // have changed the tabs-in-notch gate, and the chrome paint that follows must see both.
        self.sample_window_chrome(viewport, display_id, window_focused);
        self.schedule_frame_repaint(renderer_metrics, input_commands, &mut effects);
        crate::diagnostics::trace_slow("frame.update_frame", frame_started, 8.0);
        effects
    }

    fn process_frame_commands(
        &mut self,
        viewport: ViewportSnapshot,
        window_focused: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        self.commands.refresh_agent_scopes(&self.workspace);
        self.drain_app_commands(viewport, effects);
        self.sync_agent_attention(window_focused, effects);

        // A command-palette choice from the previous frame runs as soon as viewport/effects are
        // available, before mux refresh can retarget selected-window actions back to backend-active.
        if let Some(invocation) = self.commands.take_queued() {
            let _ = self.dispatch_command(invocation, viewport, effects);
        }
    }

    fn update_terminal_metrics(
        &mut self,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        let terminal_host_metrics = {
            let terminal = self.workspace.active.binding.terminal_mut();
            terminal
                .set_display_scale(terminal_scale_factor)
                .and_then(|()| {
                    terminal.set_render_cell_metrics(CellMetrics::new(
                        terminal_cell_width,
                        terminal_cell_height,
                    ))
                })
        };
        if let Err(error) = terminal_host_metrics {
            self.record_error(error);
        }
    }

    fn schedule_frame_repaint(
        &mut self,
        renderer_metrics: RendererMetrics,
        input_commands: usize,
        effects: &mut Vec<AppEffect>,
    ) {
        let pending_pty_bytes = self.workspace.active.binding.terminal().pending_pty_len();
        let (cols, rows) = self.workspace.active.binding.terminal().grid_size();
        let last_error = self.last_error.as_ref().map(ErrorNotice::raw_message);
        self.config_runtime.record_stability(StabilityTraceSample {
            selected_session: self.workspace.active.binding.mux().selected_session(),
            cols,
            rows,
            pending_pty_bytes,
            drain_bytes: self.last_drain.bytes,
            drain_elapsed_us: self.last_drain.elapsed_us,
            text_runs: renderer_metrics.text_runs,
            last_error: last_error.as_deref(),
        });
        let repaint = self.repaint_scheduler.recommend(RepaintSignal {
            drained_bytes: self.last_drain.bytes,
            drain_elapsed_us: self.last_drain.elapsed_us,
            pending_bytes: pending_pty_bytes,
            dirty_rows: renderer_metrics.dirty_rows,
            // Each GPUI TerminalView owns its cursor timer and invalidates only itself.
            cursor_blinking: false,
            input_commands,
        });
        let repaint_after = repaint.min(CONFIG_HOT_RELOAD_INTERVAL);
        if repaint_after.is_zero() {
            if !effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::RequestRepaint))
            {
                effects.push(AppEffect::RequestRepaint);
            }
        } else {
            effects.push(AppEffect::RepaintAfter(repaint_after));
        }
    }

    /// Only one floating dialog is shown at a time; opening one closes the rest.
    pub fn reload_config(&mut self, effects: &mut Vec<AppEffect>) -> bool {
        let change = match self.config_runtime.reload(
            self.workspace.active.binding.multiplexer().backend,
            self.active_appearance_variant,
        ) {
            Ok(change) => change,
            Err(error) => {
                self.record_error(error);
                return false;
            }
        };
        self.apply_accepted_config(change, effects);
        let config = self.config().clone();
        self.keymap_runtime.sync_config(&config);
        true
    }

    fn apply_accepted_config(
        &mut self,
        change: crate::config_runtime::AcceptedConfigChange,
        effects: &mut Vec<AppEffect>,
    ) {
        if self.localizer.locale() != self.config().locale {
            match crate::i18n::Localizer::new(&self.config().locale) {
                Ok(localizer) => self.localizer = localizer,
                Err(error) => self.record_error(error),
            }
        }
        if let Some(text_config) = change.text_config {
            effects.push(AppEffect::SetTerminalTextConfig(text_config));
        }
        if let Some(ui_fonts) = change.ui_fonts {
            effects.push(AppEffect::SetUiFonts(ui_fonts));
        }
        if let Some(weights) = change.ui_font_weights {
            effects.push(AppEffect::SetUiFontWeights(weights));
        }
        if let Some(ui_font_size) = change.ui_font_size {
            effects.push(AppEffect::SetUiFontSize(ui_font_size));
        }
        if let Some(window_title) = change.window_title {
            effects.push(AppEffect::SetWindowTitle(window_title));
        }
        if change.window_fullscreen.is_some() {
            self.apply_configured_fullscreen(effects);
        }

        let mut warnings = Vec::new();
        let profile_reload_error = if change.ssh_profiles_changed {
            self.workspace
                .rebuild_profile_bindings(
                    &change.config,
                    None,
                    self.active_appearance_variant,
                    &self.repaint,
                )
                .err()
                .map(|error| error.to_string())
        } else {
            None
        };
        if let Some(error) = profile_reload_error {
            warnings.push(error);
        }
        warnings.extend(self.workspace.publish_terminal_config(
            &change.config,
            self.active_appearance_variant,
            change.live_config.as_ref(),
        ));
        self.set_mouse_pointer_hidden_while_typing(self.mouse_pointer_hidden_while_typing, effects);
        self.workspace
            .active
            .binding
            .clear_pending_generated_names();
        if let Err(error) = self.workspace.reconcile_binding_states(&self.repaint) {
            warnings.push(error.to_string());
        }
        if self.config_runtime.has_new_session_config_changes() {
            warnings.push(
                "config reloaded; session/window settings require a new window or restart"
                    .to_owned(),
            );
        }
        if let Some(warning) = change.compatibility_warning {
            warnings.push(warning);
        }
        self.last_error =
            (!warnings.is_empty()).then(|| ErrorNotice::from_text(warnings.join("; ")));
        effects.push(AppEffect::RequestRepaint);
    }

    /// Apply fullscreen at the native-window seam. Unlike process/session configuration, the
    /// running GPUI window can transition immediately; its resize callback then publishes the new
    /// viewport and the next frame derives terminal cells and PTY geometry from that viewport.
    fn apply_configured_fullscreen(&mut self, effects: &mut Vec<AppEffect>) {
        let enabled = self.window_chrome.fullscreen;
        self.macos_non_native_fullscreen_pending_apply = false;
        self.set_runtime_fullscreen(enabled, effects);
    }
    fn hot_reload_config_if_changed(&mut self, effects: &mut Vec<AppEffect>, now: Instant) {
        if !self.config_runtime.reload_due(now) {
            return;
        }
        self.reload_config(effects);
    }
    fn hot_reload_keymap_if_changed(&mut self, now: Instant) {
        if !self.keymap_runtime.reload_due(now) {
            return;
        }
        if let Err(error) = self.reload_keymap() {
            self.record_error(error);
        }
    }
    fn split_app_actions(
        &mut self,
        events: Vec<InputEvent>,
    ) -> (Vec<InputEvent>, Vec<CommandInvocation>) {
        let focus = self.keymap_focus();
        let backend = self.workspace.multiplexer_backend();
        self.keymap_runtime
            .split_events(events, self.modifier_sides, focus, backend)
    }
}
