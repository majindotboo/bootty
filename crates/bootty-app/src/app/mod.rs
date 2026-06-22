mod state;

use std::{path::PathBuf, sync::mpsc, time::Instant};

use anyhow::Result;
use eframe::egui::{
    self, FontData, FontDefinitions, FontFamily, Pos2, Rect, TextureHandle, UiBuilder, Vec2,
};

pub use state::{AppEffect, AppState, FrameInputs, ViewportSnapshot};

use crate::{
    config::BoottyConfig,
    direct_input::{DirectKeyInput, ModifierSideState, suppress_egui_events_for_direct_input},
    menu::AppMenu,
    mux::config::selected_backend,
    renderer::TerminalWidget,
    theme::theme_palette_from_config,
    ui::{
        chrome::{self, SidebarModel, StatusBarModel},
        settings::{self, SettingsWindow},
    },
};

/// Fallback layout offset (points) when a notched screen is detected but the exact band can't
/// be measured. This intentionally targets the physical notch, not the slightly lower menu-bar
/// drop-down line reported by macOS safe-area APIs.
const FALLBACK_NOTCH_LAYOUT_OFFSET: f32 = 24.0;
const MACOS_NOTCH_MENU_BAR_OVERSHOOT: f32 = 8.0;
/// Minimum sidebar width enforced while dragging the resize handle (matches the settings floor).
const MIN_SIDEBAR_WIDTH: f32 = 120.0;
/// Grab width of the invisible splitter painted at the sidebar's inner edge.
const SIDEBAR_RESIZE_HANDLE_WIDTH: f32 = 8.0;

fn status_segment_visible(segment: &crate::config::StatusSegment, sidebar_visible: bool) -> bool {
    !(sidebar_visible && segment.module == "session")
}

fn status_bar_left_padding(sidebar_visible: bool, sidebar_on_right: bool) -> f32 {
    if sidebar_visible && !sidebar_on_right {
        0.0
    } else {
        chrome::STATUS_EDGE_PAD
    }
}

fn fullscreen_notch_layout_offset(configured_offset: Option<f32>, measured_band: f32) -> f32 {
    if let Some(offset) = configured_offset {
        return offset.max(0.0);
    }

    if measured_band > 0.0 {
        (measured_band - MACOS_NOTCH_MENU_BAR_OVERSHOOT).max(0.0)
    } else {
        FALLBACK_NOTCH_LAYOUT_OFFSET
    }
}

pub struct BoottyApp {
    state: AppState,
    terminal_widget: TerminalWidget,
    app_icon_texture: Option<TextureHandle>,
    settings: Option<SettingsWindow>,
    // Held for the process lifetime so the native menu stays installed.
    _menu: Option<AppMenu>,
    /// Worker for built-in and user Lua/Luau status modules.
    status_modules: crate::status_module::StatusModuleHost,
    keep_awake: Option<keepawake::KeepAwake>,
}

impl BoottyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        Self::new_with_config(cc, BoottyConfig::default())
    }

    pub fn new_with_config(cc: &eframe::CreationContext<'_>, config: BoottyConfig) -> Result<Self> {
        Self::new_inner(cc, config, None, None)
    }

    pub fn new_with_direct_input(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: mpsc::Receiver<DirectKeyInput>,
        modifier_side_rx: mpsc::Receiver<ModifierSideState>,
    ) -> Result<Self> {
        Self::new_inner(cc, config, Some(direct_input_rx), Some(modifier_side_rx))
    }

    fn new_inner(
        cc: &eframe::CreationContext<'_>,
        config: BoottyConfig,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        if uses_custom_egui_fonts(&config) {
            configure_egui_fonts(&cc.egui_ctx, &config.font.family);
        } else {
            crate::ui::icons::install_icon_fonts(&cc.egui_ctx);
        }
        let repaint_ctx = cc.egui_ctx.clone();
        let repaint: crate::mux::RepaintHandle =
            std::sync::Arc::new(move || repaint_ctx.request_repaint());
        let text_config = config.font.terminal_text_config();
        let terminal_widget = TerminalWidget::new(
            cc.wgpu_render_state
                .as_ref()
                .map(|render_state| render_state.target_format),
        )
        .with_text_config(text_config);

        // User status modules live in `<config>/status/`; built-ins are Luau modules and user
        // `.lua` / `.luau` files override same-named defaults.
        let theme_tokens = crate::theme::theme_tokens(&config);
        let status_dir = config
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("status");
        let status_modules = crate::status_module::StatusModuleHost::spawn(
            status_dir,
            cc.egui_ctx.clone(),
            theme_tokens,
        );

        Ok(Self {
            state: AppState::new(config, repaint, direct_input_rx, modifier_side_rx)?,
            terminal_widget,
            app_icon_texture: None,
            settings: None,
            _menu: crate::menu::install(),
            status_modules,
            keep_awake: None,
        })
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        if self.settings.is_some() {
            // Already open: raise the existing window instead of spawning a second one.
            ctx.send_viewport_cmd_to(settings::viewport_id(), egui::ViewportCommand::Focus);
        } else {
            self.settings = Some(SettingsWindow::new(self.state.config().clone()));
        }
    }

    fn show_settings(&mut self, ctx: &egui::Context) {
        let theme = self.state.ui_theme();
        if let Some(settings) = self.settings.as_mut()
            && !settings.show(ctx, theme)
        {
            self.settings = None;
        }
    }

    fn apply_effects(&mut self, ctx: &egui::Context, effects: Vec<AppEffect>) {
        for effect in effects {
            match effect {
                AppEffect::CloseWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                AppEffect::SetWindowTitle(title) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
                }
                AppEffect::SetFullscreen(fullscreen) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
                }
                AppEffect::SetMaximized(maximized) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(maximized));
                }
                AppEffect::SetDecorations(decorations) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(decorations));
                }
                AppEffect::RequestCopy => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy);
                }
                AppEffect::RequestRepaint => ctx.request_repaint(),
                AppEffect::Bell => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                }
                AppEffect::RepaintAfter(after) => ctx.request_repaint_after(after),
                AppEffect::SetTerminalTextConfig(text_config) => {
                    self.terminal_widget.set_text_config(text_config);
                }
                AppEffect::SetTerminalCursorIcon(icon) => {
                    self.terminal_widget.set_terminal_cursor_icon(icon);
                }
                AppEffect::SetWindowFocus => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                AppEffect::OpenUrl(url) => {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
                AppEffect::OpenSettings => self.open_settings(ctx),
            }
        }
    }

    fn resolve_status_segments(&self, sidebar_visible: bool) -> Vec<chrome::ResolvedSegment> {
        self.state
            .config()
            .chrome
            .status_segments
            .iter()
            .filter(|segment| status_segment_visible(segment, sidebar_visible))
            .filter_map(|segment| {
                let seg_fg = segment.fg.map(crate::theme::config_color32);
                let seg_bg = segment.bg.map(crate::theme::config_color32);
                let items = self
                    .status_modules
                    .items(&segment.module)
                    .into_iter()
                    .map(|item| chrome::ResolvedItem {
                        text: item.text,
                        icon: item.icon.or_else(|| segment.icon.clone()),
                        stroke: item.stroke,
                        fg: item.fg.or(seg_fg),
                        bg: item.bg.or(seg_bg),
                        gauge: item.gauge,
                        primitives: item.primitives,
                        pad_left: item.pad_left,
                        pad_right: item.pad_right,
                        join: item.join,
                        gap: item.gap,
                        action: item.action,
                    })
                    .collect::<Vec<_>>();
                (!items.is_empty()).then_some(chrome::ResolvedSegment {
                    align: segment.align,
                    items,
                })
            })
            .collect()
    }

    fn current_status_mux_view(&self) -> crate::status_module::MuxView {
        let selected = self.state.mux().selected_window();
        let windows = self
            .state
            .mux()
            .selected_session_windows()
            .iter()
            .map(|window| crate::status_module::WindowView {
                id: window.id.clone(),
                index: window.index,
                name: window.name.clone(),
                active: selected == Some(window.id.as_str())
                    || (selected.is_none() && window.active),
            })
            .collect();
        let session = chrome::selected_session_name(
            self.state.mux().sessions(),
            self.state.mux().selected_session(),
        )
        .map(str::to_owned);
        let session_color = crate::ui::sidebar::session_accent_color(
            self.state.mux().sessions(),
            self.state.mux().selected_session(),
        )
        .map(|color| format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b()));
        crate::status_module::MuxView {
            windows,
            session,
            session_color,
            keep_awake: self.keep_awake.is_some(),
        }
    }

    /// Pushes the active session's windows and name to the status-module worker so modules
    /// (`bootty.windows()` / `bootty.session()`) can render them.
    fn publish_status_mux_view(&self) {
        let sidebar_visible = self.state.config().chrome.sidebar;
        self.status_modules.set_active(
            self.state
                .config()
                .chrome
                .status_segments
                .iter()
                .filter(|segment| status_segment_visible(segment, sidebar_visible))
                .map(|segment| segment.module.clone()),
        );

        self.status_modules
            .update_mux(self.current_status_mux_view());
    }

    fn toggle_keep_awake(&mut self) {
        if self.keep_awake.take().is_some() {
            self.publish_status_mux_view();
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
            Err(error) => self.state.record_render_error(error),
        }
        self.publish_status_mux_view();
    }

    fn show_fixed_layout(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let palette = theme_palette_from_config(self.state.config());
        let chrome_config = &self.state.config().chrome;
        let sidebar = chrome_config.sidebar;
        let status_bar = chrome_config.status_bar;
        let configured_sidebar_width = chrome_config.sidebar_width;
        let status_height_config = chrome_config.status_height;
        let chrome_gap = chrome_config.gap;
        let fullscreen_chrome = self.state.macos_non_native_fullscreen_active()
            || ui
                .ctx()
                .input(|input| input.viewport().fullscreen.unwrap_or(false));
        // Reserve a top offset in fullscreen to clear the notch and switch the sidebar to its
        // fullscreen background. The explicit override applies whenever fullscreen so it works even
        // when auto-detection can't read the notch (a hidden menu bar zeroes safeAreaInsets); the
        // safe-area auto value only fills in when the override is unset. No objc calls when windowed.
        if fullscreen_chrome {
            crate::platform::macos_disable_titlebar_separator();
        }
        // Drop the window shadow in fullscreen; its rim otherwise reads as a border around the
        // screen-filling window. Restored when windowed.
        crate::platform::macos_set_window_shadow(!fullscreen_chrome);
        // Detect the notch by display name (stable across fullscreen/menu-bar state) rather than the
        // safe-area inset, which zeroes out when the menu bar is hidden in non-native fullscreen.
        let notch_context = fullscreen_chrome && crate::platform::macos_active_screen_is_notched();
        // Pixel height for the layout offset: the config override, else the measured macOS band
        // calibrated to the physical notch, else a fallback when the band is unreadable.
        let measured_band = crate::platform::macos_active_screen_notch_height();
        let fullscreen_top_offset = if notch_context {
            fullscreen_notch_layout_offset(
                self.state.config().window.fullscreen_top_offset,
                measured_band,
            )
        } else {
            0.0
        };
        // When enabled, the terminal/tab bar sits inside the notch band instead of being pushed
        // entirely below it; the band reuses the sidebar's fullscreen background either way.
        let tabs_in_notch = notch_context && self.state.config().window.fullscreen_tabs_in_notch;
        let sidebar_fullscreen_bg = {
            let sidebar_cfg = &self.state.config().sidebar;
            sidebar_cfg
                .fullscreen_background
                .or(sidebar_cfg.background)
                .map(crate::theme::config_color32)
        };
        let notch_band_color = sidebar_fullscreen_bg.unwrap_or(palette.base);
        let sidebar_width = if sidebar {
            configured_sidebar_width
        } else {
            0.0
        };
        let gap = if sidebar && sidebar_width > 0.0 && !fullscreen_chrome {
            chrome_gap
        } else {
            0.0
        };
        let status_height = if status_bar {
            status_height_config
        } else {
            0.0
        };
        let sidebar_on_right = matches!(
            self.state.config().sidebar.position,
            crate::config::SidebarPosition::Right
        );
        let clamped_sidebar_width = sidebar_width.min(rect.width());
        let (sidebar_rect, right_rect) = if !sidebar {
            (
                Rect::from_min_size(rect.min, egui::vec2(0.0, rect.height())),
                rect,
            )
        } else if sidebar_on_right {
            let split = (rect.max.x - clamped_sidebar_width).max(rect.min.x);
            (
                Rect::from_min_max(Pos2::new(split, rect.min.y), rect.max),
                Rect::from_min_max(
                    rect.min,
                    Pos2::new((split - gap).max(rect.min.x), rect.max.y),
                ),
            )
        } else {
            let split = (rect.min.x + clamped_sidebar_width).min(rect.max.x);
            (
                Rect::from_min_max(rect.min, Pos2::new(split, rect.max.y)),
                Rect::from_min_max(
                    Pos2::new((split + gap).min(rect.max.x), rect.min.y),
                    rect.max,
                ),
            )
        };
        // When the sidebar is not on the left edge, macOS traffic-light buttons land over the
        // content's top-left instead of the sidebar, so inset the status bar to clear them.
        let status_left_inset = if (!sidebar || sidebar_on_right)
            && self
                .state
                .config()
                .window
                .reserves_macos_titlebar_button_area()
        {
            chrome::MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
        } else {
            0.0
        };
        // Paint the notch band with the sidebar's fullscreen background so the strip above the
        // content matches the sidebar (the sidebar fills its own band). Content draws on top.
        if notch_context && fullscreen_top_offset > 0.0 {
            let band = Rect::from_min_max(
                Pos2::new(right_rect.min.x, rect.min.y),
                Pos2::new(right_rect.max.x, rect.min.y + fullscreen_top_offset),
            );
            ui.painter().rect_filled(band, 0.0, notch_band_color);
        }
        // With tabs-in-notch the content rises into the notch band and the terminal drops by one
        // row less than the notch so the status line's bottom edge lines up with the bottom of the
        // notch. The terminal default background is overridden to the band color below so a tmux
        // `bg=default` status line matches the chrome.
        let terminal_cell_height = self.terminal_widget.cell_dimensions().1;
        let content_offset = if !tabs_in_notch {
            fullscreen_top_offset
        } else {
            (fullscreen_top_offset - terminal_cell_height).max(0.0)
        };
        let content_top = (right_rect.min.y + content_offset).min(right_rect.max.y);
        let status_rect = Rect::from_min_max(
            Pos2::new(
                (right_rect.min.x + status_left_inset).min(right_rect.max.x),
                content_top,
            ),
            Pos2::new(
                right_rect.max.x,
                (content_top + status_height).min(right_rect.max.y),
            ),
        );
        let terminal_rect = Rect::from_min_max(
            Pos2::new(right_rect.min.x, status_rect.max.y),
            right_rect.max,
        );

        if sidebar {
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(sidebar_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    let title_visible = self.state.config().window.custom_chrome_title_visible();
                    // Traffic lights stay at the window's top-left, so only reserve their space in
                    // the sidebar when it is on the left edge.
                    let reserve_titlebar_buttons = !sidebar_on_right
                        && self
                            .state
                            .config()
                            .window
                            .reserves_macos_titlebar_button_area();
                    // Shared with the content stack so the sidebar header clears the notch too.
                    let top_inset = fullscreen_top_offset;
                    // Resolve `[sidebar]` color overrides on top of the theme. `base`/`text` feed the
                    // derived hover/current/border tints; the notch background applies only in
                    // fullscreen on a notched screen.
                    let sidebar_cfg = self.state.config().sidebar.clone();
                    let sidebar_background = if notch_context {
                        sidebar_cfg.fullscreen_background.or(sidebar_cfg.background)
                    } else {
                        sidebar_cfg.background
                    };
                    let mut sidebar_palette = palette;
                    if let Some(color) = sidebar_background {
                        sidebar_palette.base = crate::theme::config_color32(color);
                    }
                    if let Some(color) = sidebar_cfg.foreground {
                        sidebar_palette.text = crate::theme::config_color32(color);
                    }
                    let title_icon = title_visible.then(|| {
                        chrome::load_app_icon_texture(ui.ctx(), &mut self.app_icon_texture)
                    });
                    if let Some(event) = chrome::show_sidebar(
                        ui,
                        sidebar_palette,
                        sidebar_rect.height(),
                        SidebarModel {
                            sessions: self.state.mux().sessions(),
                            selected_session: self.state.mux().selected_session(),
                            metadata: self.state.sidebar_metadata(),
                            title_visible,
                            reserve_titlebar_buttons,
                            title_icon: title_icon.as_ref(),
                            top_inset,
                            border_visible: !fullscreen_chrome,
                            separator_visible: !fullscreen_chrome,
                            focused: self.state.sidebar_focused(),
                            hovered_session: self.state.sidebar_hovered_session(),
                            unfocused_dim: self.state.config().chrome.unfocused_sidebar_dim,
                            hover_override: sidebar_cfg.hover.map(crate::theme::config_color32),
                            current_override: sidebar_cfg
                                .selected
                                .map(crate::theme::config_color32),
                            border_override: sidebar_cfg.border.map(crate::theme::config_color32),
                        },
                    ) {
                        match event {
                            chrome::SidebarEvent::ActivateSession(session_id) => {
                                self.state.activate_session_from_ui(&session_id);
                            }
                            chrome::SidebarEvent::Reorder { source, before } => {
                                self.state
                                    .reorder_session_before(&source, before.as_deref());
                            }
                        }
                    }
                },
            );

            // Drag the inner edge to resize. The handle lives in a foreground layer so it wins the
            // hit-test over the sidebar rows and the terminal beneath the gap.
            if clamped_sidebar_width > 0.0 {
                let handle_x = if sidebar_on_right {
                    sidebar_rect.min.x
                } else {
                    sidebar_rect.max.x
                };
                let handle_rect = Rect::from_center_size(
                    Pos2::new(handle_x, rect.center().y),
                    egui::vec2(SIDEBAR_RESIZE_HANDLE_WIDTH, rect.height()),
                );
                let response = egui::Area::new(egui::Id::new("bootty-sidebar-resize"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(handle_rect.min)
                    .show(ui.ctx(), |ui| {
                        let response = ui.allocate_rect(handle_rect, egui::Sense::drag());
                        if response.hovered() || response.dragged() {
                            ui.painter().line_segment(
                                [
                                    Pos2::new(handle_x, rect.min.y),
                                    Pos2::new(handle_x, rect.max.y),
                                ],
                                egui::Stroke::new(2.0, palette.primary),
                            );
                        }
                        response
                    })
                    .inner;
                if response.hovered() || response.dragged() {
                    ui.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if response.dragged()
                    && let Some(pos) = ui.ctx().pointer_interact_pos()
                {
                    let raw = if sidebar_on_right {
                        rect.max.x - pos.x
                    } else {
                        pos.x - rect.min.x
                    };
                    let max = (rect.width() - MIN_SIDEBAR_WIDTH).max(MIN_SIDEBAR_WIDTH);
                    self.state
                        .set_sidebar_width_live(raw.clamp(MIN_SIDEBAR_WIDTH, max));
                }
                if response.drag_stopped() {
                    let width = self.state.config().chrome.sidebar_width;
                    self.state.persist_sidebar_width(width);
                }
            }
        }

        if status_bar {
            // Tick once a second so the clock advances and module output refreshes when idle.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
            self.publish_status_mux_view();
            let status_background = if notch_context {
                notch_band_color
            } else {
                palette.base
            };
            let segments = self.resolve_status_segments(sidebar);
            let mut clicked_action = None;
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(status_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    clicked_action = chrome::show_status_bar(
                        ui,
                        palette,
                        StatusBarModel {
                            segments: &segments,
                            background: status_background,
                            left_padding: status_bar_left_padding(sidebar, sidebar_on_right),
                        },
                    );
                },
            );
            match clicked_action.as_deref() {
                Some("toggle-caffeinate") => self.toggle_keep_awake(),
                Some(action) => {
                    if let Some(window_id) = action.strip_prefix("activate-window:")
                        && let Some(session_id) =
                            self.state.mux().selected_session().map(str::to_owned)
                    {
                        self.state.activate_window_from_ui(&session_id, window_id);
                    }
                }
                None => {}
            }
        } else {
            // Bar hidden: no module is referenced, so none should keep running.
            self.status_modules.set_active(Vec::new());
        }

        let terminal_backend = selected_backend(&self.state.config().multiplexer);
        let terminal_transition_key = self.state.mux().selected_session_anchor().map(|anchor| {
            let pane_id = anchor.pane_id.as_deref().unwrap_or_default();
            format!("{terminal_backend:?}:{}:{pane_id}", anchor.session_id)
        });
        // A native session whose tabs have all been closed has no pane to attach. Paint an empty
        // state instead of the terminal widget, which would otherwise hold the closed terminal's
        // last frame.
        let native_backend = matches!(
            self.state.config().multiplexer.backend,
            crate::config::MultiplexerBackendConfig::Native
        );
        let has_terminal = !native_backend
            || self
                .state
                .mux()
                .selected_session_anchor()
                .is_some_and(|anchor| anchor.pane_id.is_some());
        if has_terminal {
            self.terminal_widget
                .set_transition_key(terminal_transition_key);
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(terminal_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    match self.terminal_widget.show(ui, self.state.terminal_mut()) {
                        Ok(surface) => self.state.record_surface(surface),
                        Err(error) => self.state.record_render_error(error),
                    }
                    if !self.state.terminal_focused() {
                        let dim = self.state.config().chrome.unfocused_terminal_dim;
                        ui.painter().rect_filled(
                            terminal_rect,
                            0.0,
                            egui::Color32::from_black_alpha((dim.clamp(0.0, 1.0) * 255.0) as u8),
                        );
                    }
                },
            );
        } else {
            self.terminal_widget.reset();
            ui.painter_at(terminal_rect).text(
                terminal_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!(
                    "No open tabs — press {} to open one",
                    crate::platform::new_tab_shortcut_hint()
                ),
                egui::FontId::proportional(13.0),
                palette.muted,
            );
        }
    }

    fn show_new_mux_session_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_dialog() else {
            return;
        };
        let event = dialog.show(ctx, self.state.ui_theme());
        self.state.apply_picker_event(dialog, event);
    }

    fn show_session_picker_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.state.take_session_picker_dialog() else {
            return;
        };
        let sessions = self.state.mux().sessions().to_vec();
        let selected_session = self.state.mux().selected_session().map(str::to_owned);
        let event = dialog.show(
            ctx,
            self.state.ui_theme(),
            &sessions,
            selected_session.as_deref(),
        );
        self.state.apply_session_picker_event(dialog, event);
    }
}

impl eframe::App for BoottyApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.state.drain_direct_input();
        if self.state.direct_input_suppresses_egui_events() {
            suppress_egui_events_for_direct_input(
                &mut raw_input.events,
                self.state.pending_direct_input(),
            );
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (
            mut events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            stable_dt,
            viewport,
            zoom_delta,
        ) = ctx.input(|input| {
            (
                input.events.clone(),
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.clone())
                    .collect::<Vec<PathBuf>>(),
                input.modifiers,
                input.pointer.hover_pos(),
                crate::input::pressed_mouse_button_from_egui(&input.pointer),
                input.stable_dt,
                ViewportSnapshot {
                    fullscreen: input.viewport().fullscreen.unwrap_or(false),
                    maximized: input.viewport().maximized.unwrap_or(false),
                    content_height: input.content_rect().height(),
                },
                input.zoom_delta(),
            )
        });
        let (terminal_cell_width, terminal_cell_height) = self.terminal_widget.cell_dimensions();

        if (zoom_delta - 1.0).abs() > f32::EPSILON {
            self.terminal_widget.apply_pinch(zoom_delta, hover_pos);
        }
        if self.terminal_widget.is_zoomed() {
            let pan = take_scroll_for_pan(&mut events, terminal_cell_height);
            if pan != Vec2::ZERO {
                self.terminal_widget.apply_pan(pan);
            }
        }

        let inputs = FrameInputs {
            now: Instant::now(),
            stable_dt_ms: stable_dt * 1000.0,
            events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            renderer_metrics: self.terminal_widget.metrics(),
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor: ctx.pixels_per_point(),
            terminal_view_transform: self.terminal_widget.view_transform(),
        };
        let effects = self.state.update_frame(inputs);
        self.apply_effects(ctx, effects);

        if crate::menu::settings_requested() {
            self.open_settings(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let palette = self.state.ui_theme().palette;
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            self.show_fixed_layout(ui);
        });
        self.show_new_mux_session_dialog(ui.ctx());
        self.show_session_picker_dialog(ui.ctx());
        self.show_settings(ui.ctx());
    }
}

fn uses_custom_egui_fonts(config: &BoottyConfig) -> bool {
    config.chrome.sidebar || config.chrome.status_bar
}

// egui's wheel `delta` already points the way content should move, so it is the pan delta as-is.
fn take_scroll_for_pan(events: &mut Vec<egui::Event>, line_height: f32) -> Vec2 {
    let mut pan = Vec2::ZERO;
    events.retain(|event| {
        let egui::Event::MouseWheel { unit, delta, .. } = event else {
            return true;
        };
        let scale = match unit {
            egui::MouseWheelUnit::Point => 1.0,
            egui::MouseWheelUnit::Line => line_height,
            egui::MouseWheelUnit::Page => line_height * 20.0,
        };
        pan += *delta * scale;
        false
    });
    pan
}

fn configure_egui_fonts(ctx: &egui::Context, families: &[String]) {
    let db = bootty_render::font_database::system_font_database();
    let mut fonts = FontDefinitions::default();
    for family in families.iter().rev() {
        let query_families = [fontdb::Family::Name(family)];
        let query = fontdb::Query {
            families: &query_families,
            ..fontdb::Query::default()
        };
        let Some(id) = db.query(&query) else {
            continue;
        };
        let Some((bytes, index)) = db.with_face_data(id, |data, index| (data.to_vec(), index))
        else {
            continue;
        };
        let name = format!("bootty-ui-{family}");
        let mut font_data = FontData::from_owned(bytes);
        font_data.index = index;
        fonts
            .font_data
            .insert(name.clone(), std::sync::Arc::new(font_data));
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, name.clone());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name);
    }
    crate::ui::icons::add_icon_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_egui_fonts_only_load_for_visible_chrome() {
        let mut config = BoottyConfig::default();
        assert!(uses_custom_egui_fonts(&config));

        config.chrome.sidebar = false;
        config.chrome.status_bar = false;
        assert!(!uses_custom_egui_fonts(&config));

        config.chrome.status_bar = true;
        assert!(uses_custom_egui_fonts(&config));
    }

    #[test]
    fn session_status_segment_tracks_sidebar_visibility() {
        let segment = crate::config::StatusSegment {
            module: "session".to_owned(),
            ..Default::default()
        };
        assert!(!status_segment_visible(&segment, true));
        assert!(status_segment_visible(&segment, false));
    }

    #[test]
    fn non_session_status_segments_remain_visible_with_sidebar() {
        let segment = crate::config::StatusSegment {
            module: "windows".to_owned(),
            ..Default::default()
        };
        assert!(status_segment_visible(&segment, true));
    }

    #[test]
    fn status_bar_left_padding_is_flush_next_to_left_sidebar() {
        assert_eq!(status_bar_left_padding(true, false), 0.0);
        assert_eq!(
            status_bar_left_padding(false, false),
            chrome::STATUS_EDGE_PAD
        );
        assert_eq!(status_bar_left_padding(true, true), chrome::STATUS_EDGE_PAD);
    }
}
