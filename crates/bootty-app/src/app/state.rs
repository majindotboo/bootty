use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{self, Pos2};

use crate::{
    app_actions::{
        AppAction, AppKeyBindings, FontSizeAction, KeybindAction, MuxKeyAction, SidebarAction,
        SidebarKeyBindings, TerminalScrollAction, builtin_app_action_for_direct_key,
        split_app_actions_for_bindings,
    },
    config::{
        BoottyConfig, ConfigState, WindowConfig, load_config_from_path,
        load_or_create_config_document,
    },
    config_reload::{CONFIG_HOT_RELOAD_INTERVAL, ConfigHotReload, new_session_only_config_changed},
    diagnostics::{
        STATUS_METRICS_SAMPLE_INTERVAL, StabilityTrace, StabilityTraceSample, StatusMetrics,
    },
    direct_input::{DirectKeyInput, ModifierSideState},
    geometry::{SurfacePoint, TerminalSurface, ViewTransform},
    input::{
        InputSnapshot, TerminalInputCommand, WheelScrollState, focus::InputFocus,
        router::route_events, terminal_input_commands_with_wheel_state,
    },
    modifier_remap::ModifierRemapSet,
    mux::{
        RepaintHandle,
        command::MuxCommand,
        config::{MuxBackendKind, selected_backend},
        controller::{MUX_SESSION_REFRESH_INTERVAL, MuxController},
        terminal::ActiveTerminal,
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, macos_handles_non_native_fullscreen_frame,
        read_clipboard_text, restore_macos_presentation, show_desktop_notification,
        write_clipboard_text,
    },
    renderer::{RendererMetrics, TerminalRenderSource, TerminalWidget},
    scheduler::{RepaintScheduler, RepaintSignal},
    session_order::SessionOrderStore,
    terminal::{DrainStats, KeyInput, MouseButton, TerminalKey, TerminalSessionConfig},
    terminal_text::TerminalTextConfig,
    theme::theme_from_config,
    ui::{
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
        session_picker::{SessionPickerDialog, SessionPickerEvent},
    },
};
use bootty_terminal::terminal_engine::{
    TerminalSelectionEvent, TerminalSelectionFormat, TerminalSideEffect,
    encode_iterm2_report_cell_size, encode_iterm2_report_variable, encode_osc52_response,
};

fn mux_refresh_repaint_after(config: &crate::config::MultiplexerConfig) -> Option<Duration> {
    (selected_backend(config) != MuxBackendKind::Native).then_some(MUX_SESSION_REFRESH_INTERVAL)
}
/// Per-frame snapshot of everything the state machine needs from the host.
/// Captured once at frame start; `egui::Context` never enters this module.
#[derive(Clone, Debug)]
pub struct FrameInputs {
    pub now: Instant,
    pub stable_dt_ms: f32,
    pub events: Vec<egui::Event>,
    pub dropped_file_paths: Vec<PathBuf>,
    pub modifiers: egui::Modifiers,
    pub hover_pos: Option<Pos2>,
    pub pressed_mouse_button: Option<MouseButton>,
    pub viewport: ViewportSnapshot,
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

/// Host actions requested by a frame update, applied by the eframe adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum AppEffect {
    CloseWindow,
    SetWindowTitle(String),
    SetFullscreen(bool),
    SetMaximized(bool),
    SetDecorations(bool),
    RequestCopy,
    RequestRepaint,
    Bell,
    RepaintAfter(Duration),
    SetTerminalTextConfig(TerminalTextConfig),
    SetTerminalCursorIcon(egui::CursorIcon),
    SetWindowFocus,
    OpenUrl(String),
    OpenSettings,
}

pub struct AppState {
    terminal: ActiveTerminal,
    repaint_scheduler: RepaintScheduler,
    last_error: Option<String>,
    last_drain: DrainStats,
    last_frame_dt_ms: f32,
    status_metrics: StatusMetrics,
    last_status_metrics_sample: Instant,
    terminal_surface: Option<TerminalSurface>,
    terminal_view_transform: ViewTransform,
    config_state: ConfigState,
    input_focus: InputFocus,
    app_key_bindings: AppKeyBindings,
    sidebar_key_bindings: SidebarKeyBindings,
    has_new_session_config_changes: bool,
    mux: MuxController,
    repaint: RepaintHandle,
    terminal_side_effect_tx: mpsc::Sender<TerminalSideEffect>,
    terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffect>,
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    modifier_sides: ModifierSideState,
    pending_direct_input: Vec<DirectKeyInput>,
    suppress_next_egui_paste: bool,
    terminal_selection_drag_active: bool,
    wheel_scroll_state: WheelScrollState,
    modifier_remaps: ModifierRemapSet,
    terminal_cursor_icon: egui::CursorIcon,
    mouse_pointer_hidden_while_typing: bool,
    macos_option_as_alt: crate::terminal::MacosOptionAsAlt,
    stability_trace: Option<StabilityTrace>,
    config_hot_reload: ConfigHotReload,
    session_order: SessionOrderStore,
    new_mux_session_dialog: Option<NewMuxSessionDialog>,
    sidebar_hovered_session: Option<String>,
    session_picker_dialog: Option<SessionPickerDialog>,
    macos_non_native_fullscreen_active: bool,
    macos_non_native_fullscreen_pending_apply: bool,
}

fn terminal_session_config_with_side_effects(
    config: &BoottyConfig,
    side_effect_tx: &mpsc::Sender<TerminalSideEffect>,
) -> TerminalSessionConfig {
    let mut session_config = config.terminal_session_config();
    session_config.side_effect_tx = Some(side_effect_tx.clone());
    session_config
}

fn remove_first_paste_event(events: &mut Vec<egui::Event>) -> bool {
    if let Some(index) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
    {
        events.remove(index);
        true
    } else {
        false
    }
}

#[derive(Clone, Debug, PartialEq)]
enum TerminalSelectionAction {
    Begin(TerminalSelectionEvent),
    Update(TerminalSelectionEvent),
    End(Option<TerminalSelectionEvent>),
}

fn route_terminal_selection_events(
    events: Vec<egui::Event>,
    surface: Option<TerminalSurface>,
    view: ViewTransform,
    active: &mut bool,
    mouse_tracking: bool,
    frame_modifiers: egui::Modifiers,
) -> (Vec<egui::Event>, Vec<TerminalSelectionAction>) {
    let Some(surface) = surface else {
        *active = false;
        return (events, Vec::new());
    };

    let mut terminal_events = Vec::with_capacity(events.len());
    let mut selection_actions = Vec::new();
    for event in events {
        match event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            } if surface.rect.contains(pos)
                && (modifiers.shift || frame_modifiers.shift || !mouse_tracking) =>
            {
                let rectangle = modifiers.alt || frame_modifiers.alt;
                if let Some(selection_event) =
                    terminal_selection_event(surface, view, pos, rectangle)
                {
                    *active = true;
                    selection_actions.push(TerminalSelectionAction::Begin(selection_event));
                    continue;
                }
                terminal_events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                });
            }
            egui::Event::PointerMoved(pos) if *active => {
                if let Some(selection_event) =
                    terminal_selection_event(surface, view, pos, frame_modifiers.alt)
                {
                    selection_actions.push(TerminalSelectionAction::Update(selection_event));
                }
            }
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            } if *active => {
                let selection_event = terminal_selection_event(
                    surface,
                    view,
                    pos,
                    modifiers.alt || frame_modifiers.alt,
                );
                selection_actions.push(TerminalSelectionAction::End(selection_event));
                *active = false;
            }
            event => terminal_events.push(event),
        }
    }

    (terminal_events, selection_actions)
}

fn terminal_selection_event(
    surface: TerminalSurface,
    view: ViewTransform,
    pos: Pos2,
    rectangle: bool,
) -> Option<TerminalSelectionEvent> {
    let position = surface.relative_position(view.inverse_point(pos))?;
    Some(TerminalSelectionEvent {
        surface,
        position: SurfacePoint {
            x: position.x,
            y: position.y,
        },
        rectangle,
    })
}

fn copy_shortcut_pressed(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::C,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if modifiers.command && !modifiers.ctrl && !modifiers.alt
    )
}

fn direct_copy_shortcut_pressed(input: KeyInput) -> bool {
    input.key == TerminalKey::C
        && input.mods.command
        && !input.mods.ctrl
        && !input.mods.alt
        && !input.repeat
}

fn terminal_cursor_icon_for_mouse_shape(shape: &str) -> Option<egui::CursorIcon> {
    let normalized = shape.to_ascii_lowercase().replace('_', "-");
    for token in normalized
        .split([';', ',', ':', '=', ' '])
        .filter(|token| !token.is_empty())
    {
        let icon = match token {
            "default" | "reset" | "arrow" => egui::CursorIcon::Default,
            "none" | "hidden" => egui::CursorIcon::None,
            "pointer" | "hand" | "pointing-hand" => egui::CursorIcon::PointingHand,
            "text" | "ibeam" | "i-beam" => egui::CursorIcon::Text,
            "vertical-text" => egui::CursorIcon::VerticalText,
            "crosshair" => egui::CursorIcon::Crosshair,
            "help" => egui::CursorIcon::Help,
            "wait" => egui::CursorIcon::Wait,
            "progress" => egui::CursorIcon::Progress,
            "cell" => egui::CursorIcon::Cell,
            "copy" => egui::CursorIcon::Copy,
            "alias" => egui::CursorIcon::Alias,
            "move" => egui::CursorIcon::Move,
            "no-drop" => egui::CursorIcon::NoDrop,
            "not-allowed" | "forbidden" => egui::CursorIcon::NotAllowed,
            "grab" => egui::CursorIcon::Grab,
            "grabbing" => egui::CursorIcon::Grabbing,
            "all-scroll" => egui::CursorIcon::AllScroll,
            "ew-resize" | "col-resize" | "resize-horizontal" => egui::CursorIcon::ResizeHorizontal,
            "ns-resize" | "row-resize" | "resize-vertical" => egui::CursorIcon::ResizeVertical,
            "nesw-resize" | "resize-nesw" => egui::CursorIcon::ResizeNeSw,
            "nwse-resize" | "resize-nwse" => egui::CursorIcon::ResizeNwSe,
            "e-resize" | "resize-east" => egui::CursorIcon::ResizeEast,
            "s-resize" | "resize-south" => egui::CursorIcon::ResizeSouth,
            "w-resize" | "resize-west" => egui::CursorIcon::ResizeWest,
            "n-resize" | "resize-north" => egui::CursorIcon::ResizeNorth,
            "ne-resize" | "resize-north-east" => egui::CursorIcon::ResizeNorthEast,
            "nw-resize" | "resize-north-west" => egui::CursorIcon::ResizeNorthWest,
            "se-resize" | "resize-south-east" => egui::CursorIcon::ResizeSouthEast,
            "sw-resize" | "resize-south-west" => egui::CursorIcon::ResizeSouthWest,
            "zoom-in" => egui::CursorIcon::ZoomIn,
            "zoom-out" => egui::CursorIcon::ZoomOut,
            _ => continue,
        };
        return Some(icon);
    }
    None
}

fn terminal_report_variable_response(name: &str, session_name: Option<&str>) -> Option<Vec<u8>> {
    match name {
        "session.name" => session_name.map(encode_iterm2_report_variable),
        _ => None,
    }
}

#[cfg(test)]
fn new_mux_session_request_with_name(
    config: &BoottyConfig,
    name: impl Into<String>,
) -> crate::ui::new_session_picker::NewMuxSessionRequest {
    let cwd = config
        .session
        .working_directory
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| {
            config
                .config_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_owned()
        });
    crate::ui::new_session_picker::NewMuxSessionRequest {
        session_id: name.into(),
        cwd: cwd.to_string_lossy().into_owned(),
    }
}

impl AppState {
    pub fn new(
        config: BoottyConfig,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        let modifier_remaps = config.input.modifier_remaps()?;
        let macos_option_as_alt = config.input.macos_option_as_alt.into();
        let keybinds = config
            .input
            .keybinds_for_backend(config.multiplexer.backend);
        let app_key_bindings = AppKeyBindings::from_keybinds(&keybinds)?;
        let sidebar_key_bindings =
            SidebarKeyBindings::from_keybinds(&config.input.sidebar_keybind)?;
        let stability_trace = StabilityTrace::from_config(&config);
        let (terminal_side_effect_tx, terminal_side_effect_rx) = mpsc::channel();
        let session_config =
            terminal_session_config_with_side_effects(&config, &terminal_side_effect_tx);
        let config_hot_reload = ConfigHotReload::new(&config.config_path);
        let session_order = SessionOrderStore::lazy_for_config_path(&config.config_path);
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
        let macos_non_native_fullscreen_applied =
            apply_macos_non_native_fullscreen_presentation(&config.window);
        let macos_non_native_fullscreen_pending_apply =
            macos_non_native_fullscreen_active && !macos_non_native_fullscreen_applied;

        Ok(Self {
            terminal: ActiveTerminal::new(
                TerminalWidget::initial_geometry(),
                &config.multiplexer,
                session_config,
                repaint.clone(),
            ),
            repaint_scheduler: RepaintScheduler::default(),
            last_error: None,
            last_drain: DrainStats::default(),
            last_frame_dt_ms: 0.0,
            status_metrics: StatusMetrics::default(),
            last_status_metrics_sample: Instant::now() - STATUS_METRICS_SAMPLE_INTERVAL,
            terminal_surface: None,
            terminal_view_transform: ViewTransform::IDENTITY,
            config_state: ConfigState::new(config),
            input_focus: InputFocus::Terminal,
            app_key_bindings,
            sidebar_key_bindings,
            has_new_session_config_changes: false,
            mux: MuxController::new(),
            terminal_side_effect_tx,
            terminal_side_effect_rx,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            modifier_sides: ModifierSideState::default(),
            pending_direct_input: Vec::new(),
            suppress_next_egui_paste: false,
            terminal_selection_drag_active: false,
            wheel_scroll_state: WheelScrollState::default(),
            modifier_remaps,
            terminal_cursor_icon: egui::CursorIcon::Default,
            mouse_pointer_hidden_while_typing: false,
            macos_option_as_alt,
            stability_trace,
            config_hot_reload,
            session_order,
            new_mux_session_dialog: None,
            sidebar_hovered_session: None,
            session_picker_dialog: None,
            macos_non_native_fullscreen_active,
            macos_non_native_fullscreen_pending_apply,
        })
    }

    pub fn config(&self) -> &BoottyConfig {
        self.config_state.current()
    }

    /// Apply a dragged sidebar width to the live config without touching disk, so the layout
    /// tracks the pointer each frame. [`Self::persist_sidebar_width`] writes the final value.
    pub fn set_sidebar_width_live(&mut self, width: f32) {
        self.config_state.current_mut().chrome.sidebar_width = width;
    }

    /// Persist the sidebar width to `config.toml` on drag release. The live value already matches,
    /// so the hot-reload baseline is refreshed to skip the redundant reload the write would trigger.
    pub fn persist_sidebar_width(&mut self, width: f32) {
        let path = self.config().config_path.clone();
        let result = (|| {
            let mut document = load_or_create_config_document(&path)?;
            document.set_item(
                &["chrome", "sidebar-width"],
                bootty_config::toml_edit::value(f64::from(width)),
            )?;
            document.write_to_disk()
        })();
        match result {
            Ok(()) => self.config_hot_reload.refresh_after_reload(&path),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    pub fn ui_theme(&self) -> bootty_ui::Theme {
        theme_from_config(self.config())
    }

    pub fn mux(&self) -> &MuxController {
        &self.mux
    }

    pub fn status_metrics(&self) -> StatusMetrics {
        self.status_metrics
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn sidebar_focused(&self) -> bool {
        self.input_focus == InputFocus::Sidebar
    }

    pub fn terminal_focused(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    pub fn sidebar_hovered_session(&self) -> Option<&str> {
        self.sidebar_hovered_session.as_deref()
    }
    pub fn direct_input_suppresses_egui_events(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    pub fn macos_non_native_fullscreen_active(&self) -> bool {
        self.macos_non_native_fullscreen_active
    }

    fn sync_macos_non_native_fullscreen_presentation(&mut self) {
        if !self.macos_non_native_fullscreen_pending_apply {
            return;
        }
        if apply_macos_non_native_fullscreen_presentation(&self.config().window) {
            self.macos_non_native_fullscreen_pending_apply = false;
        }
    }

    pub fn terminal_mut(&mut self) -> &mut ActiveTerminal {
        &mut self.terminal
    }

    pub fn record_surface(&mut self, surface: TerminalSurface) {
        self.terminal_surface = Some(surface);
    }

    pub fn record_render_error(&mut self, error: impl ToString) {
        self.last_error = Some(error.to_string());
    }

    pub fn activate_session_from_ui(&mut self, session_id: &str) {
        self.mux.activate_session(session_id);
        self.sidebar_hovered_session = Some(session_id.to_owned());
        (self.repaint)();
    }

    pub fn activate_window_from_ui(&mut self, session_id: &str, window_id: &str) {
        let mux_config = self.config().multiplexer.clone();
        self.mux
            .activate_window(session_id, window_id, &self.repaint, &mux_config);
    }

    fn sync_session_order(&mut self) {
        let ordered_names = self.session_order.sync_sessions(
            self.mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        );
        self.mux.apply_session_order(&ordered_names);
    }

    fn move_selected_session(&mut self, delta: i32) -> bool {
        let Some(selected) = self.mux.selected_session() else {
            return false;
        };
        let Some(selected_name) = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.name.clone())
        else {
            return false;
        };
        if !self.session_order.move_session(
            &selected_name,
            delta,
            self.mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        ) {
            return false;
        }
        self.sync_session_order();
        true
    }

    pub fn reorder_session_before(&mut self, source: &str, target: Option<&str>) -> bool {
        // Per-session anchors: a drag reorders within a group when source and target share one,
        // and moves the whole group across groups.
        if !self.session_order.move_session_before(
            source,
            target,
            self.mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        ) {
            return false;
        }
        self.sync_session_order();
        true
    }

    pub fn take_dialog(&mut self) -> Option<NewMuxSessionDialog> {
        self.new_mux_session_dialog.take()
    }

    pub fn take_session_picker_dialog(&mut self) -> Option<SessionPickerDialog> {
        self.session_picker_dialog.take()
    }

    pub fn apply_session_picker_event(
        &mut self,
        dialog: SessionPickerDialog,
        event: SessionPickerEvent,
    ) {
        match event {
            SessionPickerEvent::None => {
                self.session_picker_dialog = Some(dialog);
            }
            SessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            SessionPickerEvent::ActivateSession(session_id) => {
                self.input_focus = InputFocus::Terminal;
                self.activate_session_from_ui(&session_id);
            }
        }
    }

    pub fn apply_picker_event(
        &mut self,
        dialog: NewMuxSessionDialog,
        event: NewSessionPickerEvent,
    ) {
        match event {
            NewSessionPickerEvent::None => {
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            NewSessionPickerEvent::NewWorktreeUnavailable => {
                self.last_error = Some("new worktree creation is not wired yet".to_owned());
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::CreateSession(request) => {
                let mux_config = self.config().multiplexer.clone();
                self.mux
                    .create_project_session(request, &self.repaint, &mux_config);
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn drain_direct_input(&mut self) {
        if let Some(rx) = &self.modifier_side_rx
            && let Some(latest) = rx.try_iter().last()
        {
            self.modifier_sides = latest;
        }
        let Some(rx) = &self.direct_input_rx else {
            return;
        };
        self.pending_direct_input.extend(rx.try_iter());
    }

    fn drain_terminal_side_effects(
        &mut self,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        let side_effects = self.terminal_side_effect_rx.try_iter().collect::<Vec<_>>();
        for side_effect in side_effects {
            self.apply_terminal_side_effect(
                side_effect,
                effects,
                terminal_cell_width,
                terminal_cell_height,
                terminal_scale_factor,
            );
        }
    }

    fn apply_terminal_side_effect(
        &mut self,
        side_effect: TerminalSideEffect,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        match side_effect {
            TerminalSideEffect::Bell => effects.push(AppEffect::Bell),
            TerminalSideEffect::ClipboardWrite(text) => {
                if let Err(error) = write_clipboard_text(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ClipboardQuery { selection } => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self
                        .terminal
                        .write_input(&encode_osc52_response(&selection, &text))
                    {
                        self.last_error = Some(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            },
            TerminalSideEffect::WindowTitle(title) => {
                effects.push(AppEffect::SetWindowTitle(title));
            }
            TerminalSideEffect::WindowIcon(_) => {}
            TerminalSideEffect::DesktopNotification { title, body } => {
                if let Err(error) = show_desktop_notification(&title, &body) {
                    self.last_error = Some(error.to_string());
                }
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
                if let Err(error) = self.terminal.write_input(&response) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ReportVariable(name) => {
                if let Some(response) =
                    terminal_report_variable_response(&name, self.mux.selected_session())
                    && let Err(error) = self.terminal.write_input(&response)
                {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ConEmuProgress { .. } => {}
            TerminalSideEffect::SemanticPrompt(_)
            | TerminalSideEffect::KittyTextSizing(_)
            | TerminalSideEffect::ConEmuControl(_)
            | TerminalSideEffect::Iterm2Control(_)
            | TerminalSideEffect::Iterm2File(_)
            | TerminalSideEffect::UnsupportedHostCommand { .. } => {}
        }
    }

    fn effective_terminal_cursor_icon(&self) -> egui::CursorIcon {
        if self.mouse_pointer_hidden_while_typing {
            egui::CursorIcon::None
        } else {
            self.terminal_cursor_icon
        }
    }

    fn set_mouse_pointer_hidden_while_typing(
        &mut self,
        hidden: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        let hidden = hidden && self.config().input.hide_mouse_pointer_while_typing;
        if self.mouse_pointer_hidden_while_typing == hidden {
            return;
        }
        self.mouse_pointer_hidden_while_typing = hidden;
        effects.push(AppEffect::SetTerminalCursorIcon(
            self.effective_terminal_cursor_icon(),
        ));
    }

    fn hide_mouse_pointer_for_terminal_typing(&mut self, effects: &mut Vec<AppEffect>) {
        self.set_mouse_pointer_hidden_while_typing(true, effects);
    }

    fn restore_mouse_pointer_after_pointer_moved(
        &mut self,
        events: &[egui::Event],
        effects: &mut Vec<AppEffect>,
    ) {
        if events
            .iter()
            .any(|event| matches!(event, egui::Event::PointerMoved(_)))
        {
            self.set_mouse_pointer_hidden_while_typing(false, effects);
        }
    }

    pub fn pending_direct_input(&self) -> &[DirectKeyInput] {
        &self.pending_direct_input
    }

    pub fn update_frame(&mut self, inputs: FrameInputs) -> Vec<AppEffect> {
        let FrameInputs {
            now,
            stable_dt_ms,
            events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            renderer_metrics,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
            terminal_view_transform,
        } = inputs;
        let mut effects = Vec::new();

        self.sync_macos_non_native_fullscreen_presentation();
        self.last_drain = self.terminal.drain_pty();
        self.drain_terminal_side_effects(
            &mut effects,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        match self.terminal.child_exited() {
            // A shell exiting closes its pane, cascading to the tab, instead of the whole window.
            Ok(true) => self.close_active_pane(),
            Ok(false) => {}
            Err(error) => self.last_error = Some(error.to_string()),
        }

        if let Some(error) = self
            .mux
            .refresh_sessions(&self.repaint, &self.config_state.current().multiplexer)
        {
            self.last_error = Some(error);
        }
        if let Some(result) = self.mux.poll_command() {
            self.last_error = result.err();
        }
        if let Some(after) = mux_refresh_repaint_after(&self.config_state.current().multiplexer) {
            effects.push(AppEffect::RepaintAfter(after));
        }
        self.sync_session_order();
        if let Err(error) = self.terminal.sync_mux_anchor(
            &self.config_state.current().multiplexer,
            self.mux.selected_session_anchor(),
        ) {
            self.last_error = Some(error.to_string());
        }
        self.hot_reload_config_if_changed(&mut effects, now);
        self.terminal_view_transform = terminal_view_transform;
        let input_commands = self.handle_direct_input(viewport, &mut effects)
            + self.handle_egui_input(
                events,
                modifiers,
                hover_pos,
                pressed_mouse_button,
                viewport,
                &mut effects,
            )
            + self.handle_dropped_file_paths(dropped_file_paths);
        self.last_frame_dt_ms = stable_dt_ms;

        let pending_pty_bytes = self.terminal.pending_pty_len();
        let (cols, rows) = self.terminal.grid_size();
        if let Some(trace) = &mut self.stability_trace {
            trace.record(StabilityTraceSample {
                elapsed_ms: trace.started_at.elapsed().as_millis(),
                selected_session: self.mux.selected_session(),
                cols,
                rows,
                pending_pty_bytes,
                drain_bytes: self.last_drain.bytes,
                drain_elapsed_us: self.last_drain.elapsed_us,
                text_runs: renderer_metrics.text_runs,
                last_error: self.last_error.as_deref(),
            });
        }
        if now.duration_since(self.last_status_metrics_sample) >= STATUS_METRICS_SAMPLE_INTERVAL {
            self.status_metrics = StatusMetrics {
                drain: self.last_drain,
                renderer: renderer_metrics,
                cols,
                rows,
            };
            self.last_status_metrics_sample = now;
        }
        let repaint = self.repaint_scheduler.recommend(RepaintSignal {
            drained_bytes: self.last_drain.bytes,
            drain_elapsed_us: self.last_drain.elapsed_us,
            pending_bytes: pending_pty_bytes,
            dirty_rows: renderer_metrics.dirty_rows,
            cursor_blinking: renderer_metrics.cursor_blinking,
            input_commands,
        });
        effects.push(AppEffect::RepaintAfter(
            repaint.after.min(CONFIG_HOT_RELOAD_INTERVAL),
        ));
        effects
    }

    fn open_new_mux_session_dialog(&mut self) {
        self.session_picker_dialog = None;
        self.new_mux_session_dialog = Some(NewMuxSessionDialog::open());
        self.input_focus = InputFocus::Picker;
    }

    fn toggle_session_picker_dialog(&mut self) {
        self.new_mux_session_dialog = None;
        if self.session_picker_dialog.is_some() {
            self.session_picker_dialog = None;
            self.input_focus = InputFocus::Terminal;
        } else {
            self.session_picker_dialog = Some(SessionPickerDialog::open());
            self.input_focus = InputFocus::Picker;
        }
    }

    fn direct_terminal_input_enabled(&self) -> bool {
        self.input_focus.terminal_owns_input()
            && self.new_mux_session_dialog.is_none()
            && self.session_picker_dialog.is_none()
    }

    fn reload_config(&mut self, effects: &mut Vec<AppEffect>) -> bool {
        let previous = self.config().clone();
        let path = previous.config_path.clone();
        let next = match load_config_from_path(&path) {
            Ok(config) => config,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let modifier_remaps = match next.input.modifier_remaps() {
            Ok(remaps) => remaps,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let keybinds = next.input.keybinds_for_backend(next.multiplexer.backend);
        let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
            Ok(bindings) => bindings,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let sidebar_key_bindings =
            match SidebarKeyBindings::from_keybinds(&next.input.sidebar_keybind) {
                Ok(bindings) => bindings,
                Err(error) => {
                    self.config_state.reject(error.to_string());
                    self.last_error = self.config_state.last_error().map(str::to_owned);
                    return false;
                }
            };

        if previous.colors != next.colors
            && let Err(error) = self
                .terminal
                .set_colors(next.colors.terminal_color_config())
        {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        if previous.cursor != next.cursor
            && let Err(error) = self
                .terminal
                .set_cursor_config(next.cursor.terminal_cursor_config())
        {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        if previous.session.glyph_protocol != next.session.glyph_protocol
            && let Err(error) = self
                .terminal
                .set_feature_config(next.session.terminal_feature_config())
        {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        if previous.font != next.font {
            effects.push(AppEffect::SetTerminalTextConfig(
                next.font.terminal_text_config(),
            ));
        }
        if previous.window.title != next.window.title {
            effects.push(AppEffect::SetWindowTitle(next.window.title.clone()));
        }
        if previous.diagnostics != next.diagnostics {
            self.stability_trace = StabilityTrace::from_config(&next);
        }

        self.modifier_remaps = modifier_remaps;
        self.macos_option_as_alt = next.input.macos_option_as_alt.into();
        self.app_key_bindings = app_key_bindings;
        self.sidebar_key_bindings = sidebar_key_bindings;
        let session_config =
            terminal_session_config_with_side_effects(&next, &self.terminal_side_effect_tx);
        self.terminal.set_terminal_config(session_config);
        self.has_new_session_config_changes = new_session_only_config_changed(&previous, &next)
            || self.has_new_session_config_changes;
        self.config_state.accept(next);
        self.set_mouse_pointer_hidden_while_typing(self.mouse_pointer_hidden_while_typing, effects);
        self.session_order = SessionOrderStore::lazy_for_config_path(&self.config().config_path);
        self.sync_session_order();
        self.last_error = if self.has_new_session_config_changes {
            Some(
                "config reloaded; session/window settings require a new window or restart"
                    .to_owned(),
            )
        } else {
            None
        };
        effects.push(AppEffect::RequestRepaint);
        true
    }

    fn hot_reload_config_if_changed(&mut self, effects: &mut Vec<AppEffect>, now: Instant) {
        if !self.config_hot_reload.changed(now) {
            return;
        }
        let path = self.config().config_path.clone();
        if self.reload_config(effects) {
            self.config_hot_reload.refresh_after_reload(&path);
        }
    }

    fn split_app_actions(
        &mut self,
        events: Vec<egui::Event>,
    ) -> (Vec<egui::Event>, Vec<KeybindAction>) {
        split_app_actions_for_bindings(&mut self.app_key_bindings, events)
    }

    fn terminal_mouse_tracking_for_selection(
        &mut self,
        events: &[egui::Event],
        terminal_input_enabled: bool,
    ) -> bool {
        if !terminal_input_enabled
            || !events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::PointerButton {
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        ..
                    }
                )
            })
        {
            return false;
        }

        match TerminalRenderSource::is_mouse_tracking(&mut self.terminal) {
            Ok(mouse_tracking) => mouse_tracking,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn apply_terminal_selection_actions(
        &mut self,
        actions: Vec<TerminalSelectionAction>,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let count = actions.len();
        for action in actions {
            let result = match action {
                TerminalSelectionAction::Begin(event) => {
                    TerminalRenderSource::begin_selection(&mut self.terminal, event)
                }
                TerminalSelectionAction::Update(event) => {
                    TerminalRenderSource::update_selection(&mut self.terminal, event)
                }
                TerminalSelectionAction::End(event) => {
                    TerminalRenderSource::end_selection(&mut self.terminal, event)
                }
            };
            match result {
                Ok(()) => effects.push(AppEffect::RequestRepaint),
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        count
    }

    fn handle_egui_input(
        &mut self,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
        hover_pos: Option<Pos2>,
        pressed_mouse_button: Option<MouseButton>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let suppress_next_egui_paste = std::mem::take(&mut self.suppress_next_egui_paste);
        let mut events = events;
        if suppress_next_egui_paste {
            remove_first_paste_event(&mut events);
        }
        self.restore_mouse_pointer_after_pointer_moved(&events, effects);
        let terminal_input_enabled = self.direct_terminal_input_enabled();
        let selection_surface = terminal_input_enabled
            .then_some(self.terminal_surface)
            .flatten();
        let mouse_tracking =
            self.terminal_mouse_tracking_for_selection(&events, terminal_input_enabled);
        let (mut events, selection_actions) = route_terminal_selection_events(
            events,
            selection_surface,
            self.terminal_view_transform,
            &mut self.terminal_selection_drag_active,
            mouse_tracking,
            modifiers,
        );
        let selection_count = self.apply_terminal_selection_actions(selection_actions, effects);
        let copy_selection_count = self.consume_copy_shortcut_for_terminal_selection(&mut events);
        let (events, actions) = self.split_app_actions(events);
        let routed = route_events(self.input_focus, events);
        let sidebar_count = self.handle_sidebar_input(routed.ui_events);
        let events = if terminal_input_enabled {
            routed.terminal_events
        } else {
            Vec::new()
        };
        let snapshot = InputSnapshot {
            events,
            modifiers,
            modifier_sides: self.modifier_sides,
            hover_pos,
            pressed_mouse_button,
            surface: self.terminal_surface,
            mouse_exclusion: self
                .terminal_surface
                .map(crate::renderer::scrollbar_hit_rect),
            view: self.terminal_view_transform,
        };
        let commands = terminal_input_commands_with_wheel_state(
            snapshot,
            &self.modifier_remaps,
            self.macos_option_as_alt,
            &mut self.wheel_scroll_state,
        );
        let count =
            commands.len() + actions.len() + sidebar_count + selection_count + copy_selection_count;

        for action in actions {
            self.apply_keybind_action(action, viewport, effects);
        }

        for command in commands {
            self.apply_terminal_input(command, effects);
        }

        count
    }

    fn handle_dropped_file_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        if !self.direct_terminal_input_enabled() {
            return 0;
        }
        let Some(text) = bootty_winit::file_paths::format_file_paths_for_paste(
            paths.iter().map(PathBuf::as_path),
        ) else {
            return 0;
        };
        if let Err(error) = self.terminal.write_paste(&text) {
            self.last_error = Some(error.to_string());
            return 0;
        }
        1
    }

    fn handle_direct_input(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let inputs = std::mem::take(&mut self.pending_direct_input);
        let count = inputs.len();
        if !self.direct_terminal_input_enabled() {
            return count;
        }
        for input in inputs {
            let mut input = input.input();
            input.mods = self.modifier_remaps.apply(input.mods);
            if direct_copy_shortcut_pressed(input) && self.copy_terminal_selection_if_any() {
                continue;
            }
            if let Some(action) = self.app_key_bindings.action_for_input(input) {
                if matches!(action, KeybindAction::PasteFromClipboard) {
                    self.suppress_next_egui_paste = true;
                }
                self.apply_keybind_action(action, viewport, effects);
                continue;
            }
            if let Some(KeybindAction::App(AppAction::NewMuxSession)) =
                builtin_app_action_for_direct_key(input)
            {
                self.open_new_mux_session_dialog();
                continue;
            }
            if input.mods.command {
                continue;
            }
            self.apply_terminal_input(TerminalInputCommand::Key(input), effects);
        }
        count
    }

    fn handle_sidebar_input(&mut self, events: Vec<egui::Event>) -> usize {
        if self.input_focus != InputFocus::Sidebar {
            return 0;
        }
        self.ensure_sidebar_hovered_session();
        let mut count = 0;
        for event in events {
            count += 1;
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let Some(action) = self.sidebar_key_bindings.action_for_key(key, modifiers) else {
                continue;
            };
            match action {
                SidebarAction::Ignore => {}
                SidebarAction::PreviousSession => {
                    self.move_sidebar_hover(-1);
                }
                SidebarAction::NextSession => {
                    self.move_sidebar_hover(1);
                }
                SidebarAction::ActivateSession => {
                    self.activate_sidebar_hovered_session();
                }
                SidebarAction::FocusTerminal => {
                    self.input_focus = InputFocus::Terminal;
                }
            }
        }
        count
    }

    fn apply_keybind_action(
        &mut self,
        action: KeybindAction,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        match action {
            KeybindAction::App(AppAction::ReloadConfig) => {
                if self.reload_config(effects) {
                    let path = self.config().config_path.clone();
                    self.config_hot_reload.refresh_after_reload(&path);
                }
            }
            KeybindAction::App(AppAction::Ignore) => {}
            KeybindAction::App(AppAction::NewWindow | AppAction::NewMuxSession) => {
                self.open_new_mux_session_dialog();
            }

            KeybindAction::App(AppAction::SessionPicker) => {
                self.toggle_session_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::Close) => {
                effects.push(AppEffect::CloseWindow);
            }
            KeybindAction::App(AppAction::OpenSettings) => {
                effects.push(AppEffect::OpenSettings);
            }
            KeybindAction::App(AppAction::ToggleFullscreen) => {
                if should_toggle_native_fullscreen(&self.config().window) {
                    effects.push(AppEffect::SetFullscreen(!viewport.fullscreen));
                } else {
                    let next_maximized = next_non_native_fullscreen_state(
                        macos_handles_non_native_fullscreen_frame(&self.config().window),
                        self.macos_non_native_fullscreen_active,
                        viewport.maximized,
                    );
                    self.macos_non_native_fullscreen_active = next_maximized;
                    if next_maximized {
                        self.macos_non_native_fullscreen_pending_apply =
                            !apply_macos_non_native_fullscreen_presentation(&self.config().window);
                    } else {
                        restore_macos_presentation();
                        self.macos_non_native_fullscreen_pending_apply = false;
                    }
                    effects.push(AppEffect::SetFullscreen(false));
                    if !macos_handles_non_native_fullscreen_frame(&self.config().window) {
                        effects.push(AppEffect::SetMaximized(next_maximized));
                    }
                }
            }
            KeybindAction::App(AppAction::ToggleSidebarFocus) => {
                self.session_picker_dialog = None;
                self.new_mux_session_dialog = None;
                if self.input_focus == InputFocus::Sidebar {
                    self.input_focus = InputFocus::Terminal;
                } else {
                    self.config_state.current_mut().chrome.sidebar = true;
                    self.input_focus = InputFocus::Sidebar;
                    self.sidebar_hovered_session = self
                        .mux
                        .selected_session()
                        .and_then(|selected| self.session_id_matching(selected))
                        .or_else(|| {
                            self.mux
                                .sessions()
                                .first()
                                .map(|session| session.id.clone())
                        });
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::ToggleSidebarVisibility) => {
                let chrome = &mut self.config_state.current_mut().chrome;
                chrome.sidebar = !chrome.sidebar;
                if !chrome.sidebar {
                    self.input_focus = InputFocus::Terminal;
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::Mux(action) => self.apply_mux_key_action(action),
            KeybindAction::Scroll(action) => self.apply_terminal_scroll_action(action),
            KeybindAction::Write(bytes) => {
                if let Err(error) = self.terminal.write_input(&bytes) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            KeybindAction::Font(action) => self.apply_font_size_action(action, effects),
            KeybindAction::CopyToClipboard => {
                self.copy_terminal_selection_or_request_copy(effects);
            }
            KeybindAction::PasteFromClipboard => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self.terminal.write_paste(&text) {
                        self.last_error = Some(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            },
        }
    }

    fn consume_copy_shortcut_for_terminal_selection(
        &mut self,
        events: &mut Vec<egui::Event>,
    ) -> usize {
        let Some(index) = events.iter().position(copy_shortcut_pressed) else {
            return 0;
        };
        if !self.copy_terminal_selection_if_any() {
            return 0;
        }
        events.remove(index);
        1
    }

    fn copy_terminal_selection_if_any(&mut self) -> bool {
        match self
            .terminal
            .format_selection(TerminalSelectionFormat::PlainText)
        {
            Ok(Some(bytes)) => {
                let text = String::from_utf8_lossy(&bytes);
                if let Err(error) = write_clipboard_text(&text) {
                    self.last_error = Some(error.to_string());
                }
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn copy_terminal_selection_or_request_copy(&mut self, effects: &mut Vec<AppEffect>) {
        if !self.copy_terminal_selection_if_any() {
            effects.push(AppEffect::RequestCopy);
        }
    }

    // Close the focused pane (cmd+w or its shell exiting) and let the mux cascade to the tab. The
    // active terminal is dropped here so its PTY is reaped; sync_mux_anchor then attaches whatever
    // pane the mux selected next (or idle when the session has no tabs left).
    fn close_active_pane(&mut self) {
        let session_id = self.mux.selected_session().unwrap_or("local").to_owned();
        let mux_config = self.config().multiplexer.clone();
        self.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane { session_id },
        );
        self.terminal.discard_active_pane();
    }

    fn apply_mux_key_action(&mut self, action: MuxKeyAction) {
        if self.apply_session_navigation_action(action) {
            return;
        }
        if let MuxKeyAction::MoveSession(delta) = action {
            self.move_selected_session(delta);
            return;
        }
        if matches!(action, MuxKeyAction::ClosePane) {
            self.close_active_pane();
            return;
        }
        let selected_session = self.mux.selected_session().unwrap_or("local").to_owned();
        let selected_cwd = self
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.cwd.clone());
        let command = match action {
            MuxKeyAction::NewTab => MuxCommand::NewWindow {
                session_id: selected_session,
                cwd: selected_cwd,
            },
            MuxKeyAction::NextTab => MuxCommand::ActivateNextWindow {
                session_id: selected_session,
            },
            MuxKeyAction::PreviousTab => MuxCommand::ActivatePreviousWindow {
                session_id: selected_session,
            },
            MuxKeyAction::LastTab => MuxCommand::ActivateLastWindow {
                session_id: selected_session,
            },
            MuxKeyAction::SelectTab(index) => MuxCommand::ActivateWindowIndex {
                session_id: selected_session,
                index,
            },
            MuxKeyAction::MoveTab(delta) => MuxCommand::MoveWindow {
                session_id: selected_session,
                delta,
            },
            MuxKeyAction::SplitPane => MuxCommand::SplitPane {
                session_id: selected_session,
            },
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id: selected_session,
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id: selected_session,
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id: selected_session,
            },
            MuxKeyAction::ClosePane => {
                unreachable!("close pane is handled before the command match")
            }
            MuxKeyAction::TogglePaneZoom => MuxCommand::TogglePaneZoom {
                session_id: selected_session,
            },
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => {
                unreachable!("session actions are handled by Bootty state")
            }
            MuxKeyAction::DitchSession => MuxCommand::DitchSession {
                session_id: selected_session,
            },
        };
        let mux_config = self.config().multiplexer.clone();
        self.mux
            .execute_command(&self.repaint, &mux_config, command);
    }

    fn ensure_sidebar_hovered_session(&mut self) {
        if self.sidebar_hovered_index().is_some() {
            return;
        }
        self.sidebar_hovered_session = self
            .mux
            .selected_session()
            .and_then(|selected| self.session_id_matching(selected))
            .or_else(|| {
                self.mux
                    .sessions()
                    .first()
                    .map(|session| session.id.clone())
            });
    }

    fn move_sidebar_hover(&mut self, delta: isize) {
        self.ensure_sidebar_hovered_session();
        let Some(current) = self.sidebar_hovered_index() else {
            return;
        };
        let sessions = self.mux.sessions();
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        self.sidebar_hovered_session = sessions.get(next).map(|session| session.id.clone());
    }

    fn activate_sidebar_hovered_session(&mut self) {
        self.ensure_sidebar_hovered_session();
        if let Some(session_id) = self.sidebar_hovered_session.clone() {
            self.activate_session_from_ui(&session_id);
        }
        self.input_focus = InputFocus::Terminal;
    }

    fn sidebar_hovered_index(&self) -> Option<usize> {
        let hovered = self.sidebar_hovered_session.as_deref()?;
        self.mux
            .sessions()
            .iter()
            .position(|session| session.id == hovered || session.name == hovered)
    }

    fn session_id_matching(&self, value: &str) -> Option<String> {
        self.mux
            .sessions()
            .iter()
            .find(|session| session.id == value || session.name == value)
            .map(|session| session.id.clone())
    }

    fn apply_session_navigation_action(&mut self, action: MuxKeyAction) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .mux
                .sessions()
                .get(index.saturating_sub(1) as usize)
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(1),
            MuxKeyAction::PreviousSession => self.relative_session(-1),
            MuxKeyAction::LastSession => self.mux.previous_selected_session().map(str::to_owned),
            _ => None,
        };
        let Some(target) = target else {
            return false;
        };
        self.mux.activate_session(&target);
        true
    }

    fn relative_session(&self, delta: isize) -> Option<String> {
        let sessions = self.mux.sessions();
        if sessions.is_empty() {
            return None;
        }
        let selected = self.mux.selected_session();
        let current = selected
            .and_then(|selected| {
                sessions
                    .iter()
                    .position(|session| session.id == selected || session.name == selected)
            })
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        sessions.get(next).map(|session| session.id.clone())
    }

    fn apply_terminal_scroll_action(&mut self, action: TerminalScrollAction) {
        let delta = match action {
            TerminalScrollAction::Top => -1_000_000,
            TerminalScrollAction::Bottom => 1_000_000,
            TerminalScrollAction::PageUp => -(self.terminal.grid_size().1 as isize),
            TerminalScrollAction::PageDown => self.terminal.grid_size().1 as isize,
            TerminalScrollAction::Lines(lines) => isize::from(lines),
        };
        if let Err(error) = self.terminal.scroll_viewport_delta(delta) {
            self.last_error = Some(error.to_string());
        }
    }

    fn apply_terminal_input(
        &mut self,
        command: TerminalInputCommand,
        effects: &mut Vec<AppEffect>,
    ) {
        match command {
            TerminalInputCommand::Text(text) => {
                if let Err(error) = self.terminal.write_input(text.as_bytes()) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Paste(text) => {
                if let Err(error) = self.terminal.write_paste(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Focus(focused) => {
                if let Err(error) = self.terminal.encode_focus(focused) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Key(input) => {
                if let Err(error) = self.terminal.encode_key(input) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Mouse(input) => {
                if let Err(error) = self.terminal.encode_mouse(input) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => {
                if let Err(error) = self.terminal.handle_mouse_wheel(input, scroll_delta) {
                    self.last_error = Some(error.to_string());
                }
            }
        }
    }

    fn apply_font_size_action(&mut self, action: FontSizeAction, effects: &mut Vec<AppEffect>) {
        let default_size = BoottyConfig::default().font.size;
        let current_size = self.config().font.size;
        let next_size = match action {
            FontSizeAction::Increase(delta) => current_size + delta,
            FontSizeAction::Decrease(delta) => current_size - delta,
            FontSizeAction::Reset => default_size,
            FontSizeAction::Set(size) => size,
        }
        .max(1.0);
        self.config_state.current_mut().font.size = next_size;
        effects.push(AppEffect::SetTerminalTextConfig(
            self.config().font.terminal_text_config(),
        ));
    }
}

fn should_toggle_native_fullscreen(window: &WindowConfig) -> bool {
    !window.non_native_fullscreen_enabled()
}

fn next_non_native_fullscreen_state(
    macos_handles_frame: bool,
    tracked_active: bool,
    viewport_maximized: bool,
) -> bool {
    if macos_handles_frame {
        !tracked_active
    } else {
        !viewport_maximized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MultiplexerBackendConfig, WindowFullscreen};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_test_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = TEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{nanos}-{sequence}")
    }

    #[test]
    fn remove_first_paste_event_removes_only_one_paste_event() {
        let mut events = vec![
            egui::Event::Text("before".to_owned()),
            egui::Event::Paste("first".to_owned()),
            egui::Event::Paste("second".to_owned()),
        ];

        assert!(remove_first_paste_event(&mut events));
        assert_eq!(
            events,
            vec![
                egui::Event::Text("before".to_owned()),
                egui::Event::Paste("second".to_owned())
            ]
        );
    }

    #[test]
    fn bootty_selection_drag_is_not_sent_to_terminal_input() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let mut active = false;
        let events = vec![
            egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerMoved(egui::Pos2::new(20.0, 10.0)),
            egui::Event::PointerButton {
                pos: egui::Pos2::new(20.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::Text("x".to_owned()),
        ];

        let (terminal_events, selection_actions) = route_terminal_selection_events(
            events,
            Some(surface),
            ViewTransform::IDENTITY,
            &mut active,
            false,
            egui::Modifiers::default(),
        );

        assert_eq!(terminal_events, vec![egui::Event::Text("x".to_owned())]);
        assert_eq!(selection_actions.len(), 3);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(matches!(
            selection_actions[1],
            TerminalSelectionAction::Update(_)
        ));
        assert!(matches!(
            selection_actions[2],
            TerminalSelectionAction::End(_)
        ));
        assert!(!active);
    }

    #[test]
    fn plain_mouse_drag_stays_available_for_terminal_mouse_reporting() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let mut active = false;
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }];
        let original = events.clone();

        let (terminal_events, selection_actions) = route_terminal_selection_events(
            events,
            Some(surface),
            ViewTransform::IDENTITY,
            &mut active,
            true,
            egui::Modifiers::default(),
        );

        assert_eq!(terminal_events, original);
        assert!(selection_actions.is_empty());
        assert!(!active);
    }

    #[test]
    fn shift_drag_overrides_mouse_reporting_for_bootty_selection() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let mut active = false;
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: shift,
        }];

        let (terminal_events, selection_actions) = route_terminal_selection_events(
            events,
            Some(surface),
            ViewTransform::IDENTITY,
            &mut active,
            true,
            shift,
        );

        assert!(terminal_events.is_empty());
        assert_eq!(selection_actions.len(), 1);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(active);
    }

    #[test]
    fn frame_shift_overrides_mouse_reporting_when_pointer_event_lacks_modifiers() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let mut active = false;
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }];

        let (terminal_events, selection_actions) = route_terminal_selection_events(
            events,
            Some(surface),
            ViewTransform::IDENTITY,
            &mut active,
            true,
            shift,
        );

        assert!(terminal_events.is_empty());
        assert_eq!(selection_actions.len(), 1);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(active);
    }

    #[test]
    fn command_c_is_detected_as_copy_shortcut_for_selection_override() {
        assert!(copy_shortcut_pressed(&egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                mac_cmd: true,
                ..Default::default()
            },
        }));
        assert!(!copy_shortcut_pressed(&egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        }));
    }

    #[test]
    fn direct_command_c_is_detected_as_copy_shortcut_for_selection_override() {
        assert!(direct_copy_shortcut_pressed(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        }));
        assert!(!direct_copy_shortcut_pressed(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        }));
    }

    #[test]
    fn mouse_shape_side_effect_maps_common_cursor_names() {
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("shape=pointing_hand"),
            Some(egui::CursorIcon::PointingHand)
        );
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("ew-resize"),
            Some(egui::CursorIcon::ResizeHorizontal)
        );
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("not-a-known-cursor"),
            None
        );
    }

    #[test]
    fn terminal_typing_hides_mouse_pointer_until_pointer_moves() {
        let mut state = test_state();
        state.terminal_cursor_icon = egui::CursorIcon::PointingHand;
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(egui::CursorIcon::None)]
        );

        effects.clear();
        state.restore_mouse_pointer_after_pointer_moved(
            &[egui::Event::PointerMoved(egui::Pos2::new(1.0, 1.0))],
            &mut effects,
        );

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(
                egui::CursorIcon::PointingHand
            )]
        );
    }

    #[test]
    fn hide_mouse_pointer_while_typing_setting_can_disable_typing_hide() {
        let mut state = test_state_with_config(|config| {
            config.input.hide_mouse_pointer_while_typing = false;
        });
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);

        assert!(effects.is_empty());
    }

    #[test]
    fn bell_side_effect_requests_host_bell() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_terminal_side_effect(TerminalSideEffect::Bell, &mut effects, 10.0, 20.0, 1.0);

        assert_eq!(effects, vec![AppEffect::Bell]);
    }

    #[test]
    fn report_variable_response_returns_selected_session_name() {
        assert_eq!(
            terminal_report_variable_response("session.name", Some("local")),
            Some(encode_iterm2_report_variable("local"))
        );
    }

    #[test]
    fn report_variable_response_ignores_unknown_variables() {
        assert_eq!(
            terminal_report_variable_response("user.missing", Some("local")),
            None
        );
    }

    #[test]
    fn default_fullscreen_config_toggles_native_fullscreen() {
        let config = BoottyConfig::default();

        assert!(should_toggle_native_fullscreen(&config.window));
    }

    #[test]
    fn appkit_handled_non_native_fullscreen_toggles_tracked_state() {
        assert!(!next_non_native_fullscreen_state(true, true, false));
        assert!(next_non_native_fullscreen_state(true, false, false));
    }

    #[test]
    fn viewport_handled_non_native_fullscreen_toggles_maximized_state() {
        assert!(!next_non_native_fullscreen_state(false, false, true));
        assert!(next_non_native_fullscreen_state(false, true, false));
    }

    #[test]
    fn non_native_fullscreen_config_toggles_non_native_fullscreen() {
        let mut config = BoottyConfig::default();
        config.window.fullscreen = WindowFullscreen::NonNative;

        assert!(!should_toggle_native_fullscreen(&config.window));
    }

    #[test]
    fn external_mux_backends_schedule_frequent_refresh_repaints() {
        let mut config = BoottyConfig::default();
        assert_eq!(mux_refresh_repaint_after(&config.multiplexer), None);

        config.multiplexer.backend = MultiplexerBackendConfig::Tmux;

        assert_eq!(
            mux_refresh_repaint_after(&config.multiplexer),
            Some(MUX_SESSION_REFRESH_INTERVAL)
        );
        assert!(MUX_SESSION_REFRESH_INTERVAL <= Duration::from_millis(250));
    }

    #[test]
    fn new_mux_session_request_uses_configured_working_directory() {
        let mut config = BoottyConfig::default();
        config.session.working_directory = Some("tmp/bootty-project".into());

        let request = new_mux_session_request_with_name(&config, "review-session");

        assert_eq!(request.session_id, "review-session");
        assert_eq!(request.cwd, "tmp/bootty-project");
    }

    #[test]
    fn new_window_action_opens_new_session_picker() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::NewWindow),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert!(state.take_dialog().is_some());
    }

    fn test_state() -> AppState {
        test_state_with_config(|_| {})
    }

    fn test_state_with_config(mutate: impl FnOnce(&mut BoottyConfig)) -> AppState {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-test-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create app state test config dir");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        mutate(&mut config);
        AppState::new(config, repaint, None, None).expect("state")
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn sidebar_keybinds_map_configured_navigation_without_default_escape() {
        let bindings =
            SidebarKeyBindings::from_keybinds(&BoottyConfig::default().input.sidebar_keybind)
                .expect("default sidebar keybinds");

        assert_eq!(
            bindings.action_for_key(egui::Key::J, egui::Modifiers::NONE),
            Some(SidebarAction::NextSession)
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::ArrowUp, egui::Modifiers::NONE),
            Some(SidebarAction::PreviousSession)
        );
        assert_eq!(
            bindings.action_for_key(
                egui::Key::N,
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            Some(SidebarAction::NextSession)
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Enter, egui::Modifiers::NONE),
            Some(SidebarAction::ActivateSession)
        );
        assert_eq!(
            bindings.action_for_key(egui::Key::Escape, egui::Modifiers::NONE),
            None
        );
    }

    #[test]
    fn sidebar_focus_consumes_keys_and_enter_returns_terminal_focus() {
        let mut state = test_state();
        state.input_focus = InputFocus::Sidebar;

        assert_eq!(
            state.handle_sidebar_input(vec![
                key_event(egui::Key::J, egui::Modifiers::NONE),
                egui::Event::Text("j".to_owned()),
            ]),
            2
        );
        assert_eq!(state.input_focus, InputFocus::Sidebar);

        assert_eq!(
            state.handle_sidebar_input(vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)]),
            1
        );
        assert_eq!(state.input_focus, InputFocus::Sidebar);

        assert_eq!(
            state.handle_sidebar_input(vec![key_event(egui::Key::Enter, egui::Modifiers::NONE)]),
            1
        );
        assert_eq!(state.input_focus, InputFocus::Terminal);
    }

    #[test]
    fn direct_input_suppression_tracks_terminal_ownership() {
        let mut state = test_state();

        assert!(state.direct_input_suppresses_egui_events());

        state.apply_keybind_action(
            KeybindAction::App(AppAction::ToggleSidebarFocus),
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        assert!(!state.direct_input_suppresses_egui_events());

        state.apply_keybind_action(
            KeybindAction::App(AppAction::SessionPicker),
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        assert!(!state.direct_input_suppresses_egui_events());
    }

    #[test]
    fn last_session_toggles_bootty_selected_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        state.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "project".to_owned(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        state.activate_session_from_ui("local");
        state.activate_session_from_ui("project");
        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.mux.selected_session(), Some("local"));

        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.mux.selected_session(), Some("project"));
    }

    #[test]
    fn move_session_reorders_bootty_owned_session_order() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let unique = unique_test_id();
        let alpha = format!("alpha-{unique}");
        let beta = format!("beta-{unique}");
        state.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: alpha.clone(),
                cwd: "repo/a".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: beta.clone(),
                cwd: "repo/b".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        assert!(
            state
                .session_order
                .move_session(&beta, -1, [alpha.as_str(), beta.as_str()],)
        );
        let ordered = state
            .session_order
            .sync_sessions([alpha.as_str(), beta.as_str()]);

        assert_eq!(ordered, vec![beta, alpha]);
    }

    #[test]
    fn close_action_emits_close_window_effect() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::Close),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(effects, vec![AppEffect::CloseWindow]);
    }

    #[test]
    fn new_tab_action_adds_a_window() {
        let mut state = test_state();
        let before = state.mux().selected_session_windows().len();
        let selected = state.mux().selected_session().map(str::to_owned);

        state.apply_mux_key_action(MuxKeyAction::NewTab);

        let after = state.mux().selected_session_windows().len();
        assert!(
            after > before,
            "before={before} after={after} selected={selected:?}"
        );
    }

    #[test]
    fn copy_action_emits_request_copy_effect() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::CopyToClipboard,
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(effects, vec![AppEffect::RequestCopy]);
    }

    #[test]
    fn toggle_sidebar_visibility_flips_config_and_requests_repaint() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::ToggleSidebarVisibility),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(state.config().chrome.sidebar, !before);
        assert_eq!(effects, vec![AppEffect::RequestRepaint]);
    }

    #[test]
    fn font_size_decrease_clamps_at_one_and_emits_text_config() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::Font(FontSizeAction::Decrease(10_000.0)),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(state.config().font.size, 1.0);
        assert!(matches!(
            effects.as_slice(),
            [AppEffect::SetTerminalTextConfig(_)]
        ));
    }

    #[test]
    fn reload_with_unreadable_config_rejects_and_keeps_previous_config() {
        let mut state = test_state();
        let previous_title = state.config().window.title.clone();
        let mut effects = Vec::new();

        // Default config_path points at a location the test never writes, so
        // the reload must take the rejection path.
        let reloaded = state.reload_config(&mut effects);

        if reloaded {
            // A real user config exists on this machine; the reload accepting
            // it is correct behavior, nothing to assert against.
            return;
        }
        assert!(state.last_error().is_some());
        assert_eq!(state.config().window.title, previous_title);
        assert!(effects.is_empty());
    }

    #[test]
    fn reload_applies_window_title_change_as_effect() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").expect("write empty config");

        let config = BoottyConfig {
            config_path: path.clone(),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");

        std::fs::write(&path, "[window]\ntitle = \"renamed\"\n").expect("write config");
        let mut effects = Vec::new();
        let reloaded = state.reload_config(&mut effects);

        assert!(reloaded);
        assert!(
            effects.contains(&AppEffect::SetWindowTitle("renamed".to_owned())),
            "{effects:?}"
        );
        assert_eq!(state.config().window.title, "renamed");
    }
}
