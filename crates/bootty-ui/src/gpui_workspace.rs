//! GPUI presentation of the Bootty workspace.
//!
//! Projects accepted configuration, mux state, and native service facts into the workspace window.

mod dialogs;
mod native_conversations;
use native_conversations::NativeConversations;
mod settings_window;
use dialogs::WorkspaceDialogs;

use crate::gpui_keymap_editor::editor_snapshot as keymap_editor_snapshot;
use settings_window::{GpuiSettingsWindow, SettingsWindowTarget};

use bootty_mux::pane_layout::SplitDirection;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::terminal_text::{
    NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract, TerminalTextGeometry,
};
use anyhow::Result;
use bootty_config::config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandInvocation, ControlCatalog, ControlPlane,
};
use bootty_mux::provider::MuxBackendRegistry;
use bootty_terminal::geometry::{CellMetrics, SurfaceRect, TerminalPadding, TerminalSurface};
use gpui_kit::component::{
    ActiveTheme as _, ElementExt as _, WindowExt as _, notification::Notification,
};
use gpui_kit::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, CursorStyle, Entity, ExternalPaths,
    FocusHandle, Focusable, Hsla, IntoElement, MouseButton, ParentElement, Pixels, Render, Styled,
    Subscription, WeakEntity, Window, WindowDecorations, div, point, prelude::*, px, size,
};
use num_traits::ToPrimitive as _;

use crate::gpui::{
    GpuiKeymapEditor, GpuiPaneColors, GpuiPaneDividerSnapshot, GpuiPaneIntent, GpuiPaneSnapshot,
    GpuiPaneWorkspace, GpuiPaneWorkspaceSnapshot, GpuiSettings, GpuiTerminalInteraction,
    KeymapEditorIntent, ModuleIntegrationsSnapshot, PaneProgress, PaneProgressState, PaneRect,
    PaneSplitDirection, SettingsIntent,
    chrome::{ChromeIntent, ChromeSnapshot, GpuiChrome, SidebarPosition},
    setup_ui_font, terminal_cell_metrics,
};
use crate::{
    chrome_frame,
    error_catalog::ErrorNotice,
    frame_facts::RendererMetrics,
    gpui_input::GpuiFrameFacts,
    gpui_terminal_view::{
        CachedTerminalView, GpuiTerminalView, TerminalPresentation, TerminalScrollbarInput,
        TerminalViewInput,
    },
    keymap_runtime::KeymapFocus,
    settings_runtime::SettingsRuntime,
    state::{AppEffect, AppState, ViewportSnapshot},
    terminal_config::terminal_text_config,
};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Colors {
    mantle: Hsla,
    base: Hsla,
    pane: Hsla,
    surface: Hsla,
    hover: Hsla,
    border: Hsla,
    border_variant: Hsla,
    text: Hsla,
    subtext: Hsla,
    muted: Hsla,
    accent: Hsla,
    destructive: Hsla,
}

impl Colors {
    fn from_state(state: &AppState) -> Self {
        let palette = state.ui_theme().palette;
        Self {
            mantle: gpui_color(palette.mantle),
            base: gpui_color(palette.base),
            pane: gpui_color(palette.pane),
            surface: gpui_color(palette.surface),
            hover: gpui_color(palette.hover),
            border: gpui_color(palette.border),
            border_variant: gpui_color(palette.border_variant),
            text: gpui_color(palette.text),
            subtext: gpui_color(palette.subtext),
            muted: gpui_color(palette.muted),
            accent: gpui_color(palette.accent),
            destructive: gpui_color(palette.destructive),
        }
    }
}

fn gpui_color(color: crate::gpui::chrome::Rgba) -> Hsla {
    gpui_kit::rgba(
        u32::from(color.red) << 24
            | u32::from(color.green) << 16
            | u32::from(color.blue) << 8
            | u32::from(color.alpha),
    )
    .into()
}

struct BoottyErrorNotification;

fn schedule_focus(focus: FocusHandle, window: &Window, cx: &mut Context<GpuiWorkspace>) {
    cx.defer_in(window, move |_, window, cx| window.focus(&focus, cx));
}

fn terminal_area(chrome: &ChromeSnapshot, docked: bool) -> SurfaceRect {
    let sidebar_width = if chrome.layout.sidebar_visible && !docked {
        chrome.layout.effective_sidebar_width() + chrome.layout.gap
    } else {
        0.0
    };
    let min_x = if !docked
        && chrome.layout.sidebar_visible
        && chrome.layout.sidebar_position == SidebarPosition::Left
    {
        sidebar_width
    } else {
        0.0
    };
    let top_status = chrome.top_status.as_ref().map_or(0.0, |status| {
        status.rows.max(1).to_f32().unwrap_or(f32::MAX) * chrome.layout.status_height
    });
    let bottom_status = chrome.bottom_status.as_ref().map_or(0.0, |status| {
        status.rows.max(1).to_f32().unwrap_or(f32::MAX) * chrome.layout.status_height
    });
    let titlebar_height = if chrome.layout.titlebar_visible {
        chrome.layout.titlebar_height
    } else {
        0.0
    };
    let min_y = titlebar_height + chrome.layout.top_inset + if docked { 0.0 } else { top_status };
    let width = (chrome.layout.width - sidebar_width).max(1.0);
    let height = (chrome.layout.height - min_y - if docked { 0.0 } else { bottom_status }).max(1.0);
    SurfaceRect {
        min_x,
        min_y,
        max_x: min_x + width,
        max_y: min_y + height,
    }
}

#[derive(Clone)]
struct WorkspaceLaunch {
    native_chrome: Rc<RefCell<chrome_frame::NativeChrome>>,
    window_state_root: Arc<str>,
    next_window_id: Arc<AtomicU64>,
    backends: Arc<MuxBackendRegistry>,
    control_plane: ControlPlane,
}

impl WorkspaceLaunch {
    fn new(
        window_state_root: String,
        backends: Arc<MuxBackendRegistry>,
        control_plane: ControlPlane,
    ) -> Self {
        Self {
            native_chrome: Rc::new(RefCell::new(chrome_frame::NativeChrome::default())),
            window_state_root: window_state_root.into(),
            next_window_id: Arc::new(AtomicU64::new(1)),
            backends,
            control_plane,
        }
    }

    fn next_window_state_key(&self) -> String {
        let id = self.next_window_id.fetch_add(1, Ordering::Relaxed);
        format!("{}:window:{id}", self.window_state_root)
    }
}

// A cached pane and its subscriptions have exactly the same lifetime.
struct TerminalPaneView {
    view: Entity<GpuiTerminalView>,
    _subscriptions: [Subscription; 4],
}

// Retained panes may be hidden. Only views published into the current layout receive pointer input.
#[derive(Clone)]
struct VisibleTerminal {
    pane_id: Option<String>,
    // A failed publication remains a repaint target but cannot receive pointer input.
    view: Option<Entity<GpuiTerminalView>>,
}

struct TerminalHit {
    pane_id: Option<String>,
    view: Entity<GpuiTerminalView>,
    interaction: GpuiTerminalInteraction,
}

/// Root GPUI entity for one Bootty window.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Window focus, panel visibility, and modal lifetimes vary independently"
)]
pub struct GpuiWorkspace {
    state: AppState,
    workspace_bounds: Bounds<Pixels>,
    window_viewport: gpui_kit::Size<Pixels>,
    tools: Option<Entity<crate::gpui_dock::WorkspaceDock>>,
    document_close_prompt: bool,
    exit_pending: bool,
    tools_focus_subscription: Option<Subscription>,
    browser_overlay_task: Option<gpui_kit::Task<()>>,
    integration_rows: Vec<ModuleIntegrationsSnapshot>,
    launch: WorkspaceLaunch,
    terminal: TerminalPaneView,
    terminal_panes: HashMap<String, TerminalPaneView>,
    visible_terminals: Vec<VisibleTerminal>,
    terminal_mouse_buttons: HashSet<MouseButton>,
    pending_link_click: Option<(gpui_kit::Point<gpui_kit::Pixels>, Option<CommandInvocation>)>,
    visual_bell_until: Option<Instant>,
    last_locale: String,
    terminal_text_contract: Arc<TerminalTextContract>,
    terminal_base_cell: CellMetrics,
    terminal_display_scale: f32,
    terminal_cell: CellMetrics,
    keymap_context: String,
    input: crate::gpui::InputAccumulator,
    workspace: WeakEntity<Self>,
    focus: FocusHandle,
    focus_initialized: bool,
    _window_activation_subscription: Subscription,
    _window_appearance_subscription: Subscription,
    cursor: CursorStyle,
    terminal_cursor: CursorStyle,
    settings_runtime: SettingsRuntime,
    settings_started: Instant,
    last_settings_revision: Option<u64>,
    last_provider_revision: Option<u64>,
    settings_view: gpui_kit::Entity<GpuiSettings>,
    _settings_subscription: Subscription,
    settings_window: Option<WeakEntity<GpuiSettingsWindow>>,
    settings_window_opening: bool,
    keymap_editor: Entity<GpuiKeymapEditor>,
    _keymap_editor_subscription: Subscription,
    last_keymap: Option<(u64, KeymapFocus, MultiplexerBackendConfig, bool)>,
    last_keymap_editor_revision: Option<u64>,
    last_config_file_editor_revision: Option<u64>,
    dialogs: WorkspaceDialogs,
    native_conversations: NativeConversations,
    chrome_view: gpui_kit::Entity<GpuiChrome>,
    last_error_notification: Option<String>,
    last_ui_theme: crate::gpui::UiTheme,
    last_background_material: Option<gpui_kit::WindowBackgroundAppearance>,
    display_id: Option<u32>,
    _chrome_subscription: Subscription,
    pending_window_move: bool,
    pending_effects: Vec<AppEffect>,
    scheduled_repaint: Option<Instant>,
    scheduled_maintenance: Option<Instant>,
    frame_update_pending: bool,
    last_maintenance_chrome: Option<ChromeSnapshot>,
    last_pane_layouts: Vec<bootty_mux::pane_layout::PaneLayout>,
    repaint: bootty_mux::RepaintHandle,
}

impl GpuiWorkspace {
    pub(crate) fn open(
        config: BoottyConfig,
        window_state_key: String,
        backends: Arc<MuxBackendRegistry>,
        control_plane: ControlPlane,
        cx: &mut App,
    ) -> Result<(AnyWindowHandle, Entity<Self>)> {
        let launch = WorkspaceLaunch::new(window_state_key.clone(), backends, control_plane);
        Self::open_with_launch(config, window_state_key, launch, cx)
    }

    fn open_with_launch(
        config: BoottyConfig,
        window_state_key: String,
        launch: WorkspaceLaunch,
        cx: &mut App,
    ) -> Result<(AnyWindowHandle, Entity<Self>)> {
        let options = crate::platform::native_options_for_config(&config, cx);
        // Prepare fallible state before GPUI's infallible entity constructor publishes a view.
        // The bounded wake channel retains work that arrives before the window subscribes.
        let (repaint_tx, repaint_rx) = async_channel::bounded(1);
        let repaint: bootty_mux::RepaintHandle = Arc::new(move || {
            let _ = repaint_tx.try_send(());
        });
        let mut state = AppState::new_for_window_with_agents(
            config,
            window_state_key.clone(),
            Arc::clone(&launch.backends),
            repaint.clone(),
            None,
            None,
            Some(launch.control_plane.event_sender()),
        )?;
        gpui_kit::open_window(options, cx, move |window, cx| {
            crate::window::macos_enable_window_resizing(window);
            crate::window::macos_expose_text_target(window);
            state.native_computer_window = crate::window::native_computer_window_id(window);
            cx.new(|cx| {
                Self::new(
                    state,
                    &window_state_key,
                    launch,
                    repaint,
                    repaint_rx,
                    window,
                    cx,
                )
            })
        })
    }

    // Keep construction together until another child lifetime can move behind one owner.
    fn new(
        mut state: AppState,
        window_state_key: &str,
        launch: WorkspaceLaunch,
        repaint: bootty_mux::RepaintHandle,
        repaint_rx: async_channel::Receiver<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = state.config();
        Self::install_close_handler(window, cx);
        let startup_variant = config
            .appearance
            .mode
            .variant(crate::theme::appearance_variant(window.appearance()));
        let window_focused = window.is_window_active();
        let keymap_context = crate::gpui_actions::workspace_key_context(window_state_key);
        let terminal_text = terminal_text_config(&config.font);
        let terminal_text_contract = Arc::new(TerminalTextContract::new(
            terminal_text.clone(),
            NativeSymbolPolicy::default(),
        ));
        let terminal_cell = terminal_cell_metrics(&terminal_text, window);
        let font_families = Self::window_font_families(window);
        Self::watch_workspace_work(repaint_rx, window, cx);
        let mut input = crate::gpui::InputAccumulator::default();
        input.set_wake(repaint.clone());
        input.window_focused(window_focused);
        state.set_appearance_variant(startup_variant);
        let settings_runtime = SettingsRuntime::default();
        settings_runtime.request_catalog(&state.config().config_path, &repaint);
        let integration_rows = settings_runtime.current_catalog().integration_rows;

        let (settings_view, settings_subscription) =
            Self::create_settings_view(&state, &font_families, &integration_rows, window, cx);
        let (keymap_editor, keymap_editor_subscription) =
            Self::create_keymap_editor(&state, window, cx);
        let dialogs = WorkspaceDialogs::new(state.app_command_sender(Caller::Internal), window, cx);
        let (chrome_view, chrome_subscription) =
            Self::create_chrome_view(&state, &launch, &keymap_context, window, cx);
        let terminal = Self::create_terminal_view(None, window, cx);
        let focus = terminal.view.focus_handle(cx);
        let workspace = cx.weak_entity();
        let window_activation_subscription =
            Self::observe_workspace_activation(&terminal.view, window, cx);
        let (window_appearance_subscription, display_id) =
            Self::observe_workspace_window(&keymap_context, window, cx);
        let last_ui_theme = state.ui_theme();
        Self::schedule_workspace_ui(window, cx);
        Self {
            workspace_bounds: Bounds::new(point(px(0.0), px(0.0)), window.viewport_size()),
            window_viewport: window.viewport_size(),
            display_id,
            tools: None,
            document_close_prompt: false,
            exit_pending: false,
            tools_focus_subscription: None,
            browser_overlay_task: None,
            state,
            integration_rows,
            launch,
            terminal,
            terminal_panes: HashMap::new(),
            visible_terminals: Vec::new(),
            terminal_mouse_buttons: HashSet::new(),
            pending_link_click: None,
            visual_bell_until: None,
            last_locale: String::new(),
            terminal_text_contract,
            terminal_base_cell: terminal_cell,
            terminal_display_scale: window.scale_factor(),
            terminal_cell,
            keymap_context,
            input,
            workspace,
            focus,
            focus_initialized: false,
            _window_activation_subscription: window_activation_subscription,
            _window_appearance_subscription: window_appearance_subscription,
            cursor: CursorStyle::IBeam,
            terminal_cursor: CursorStyle::IBeam,
            settings_runtime,
            settings_started: Instant::now(),
            last_settings_revision: None,
            last_provider_revision: None,
            settings_view,
            _settings_subscription: settings_subscription,
            settings_window: None,
            settings_window_opening: false,
            keymap_editor,
            _keymap_editor_subscription: keymap_editor_subscription,
            last_keymap: None,
            last_keymap_editor_revision: None,
            last_config_file_editor_revision: None,
            dialogs,
            native_conversations: NativeConversations::default(),
            chrome_view,
            last_error_notification: None,
            last_ui_theme,
            last_background_material: None,
            _chrome_subscription: chrome_subscription,
            pending_window_move: false,
            pending_effects: Vec::new(),
            scheduled_repaint: None,
            scheduled_maintenance: None,
            frame_update_pending: true,
            last_maintenance_chrome: None,
            last_pane_layouts: Vec::new(),
            repaint,
        }
    }

    fn create_terminal_view(
        pane_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TerminalPaneView {
        let view = cx.new(GpuiTerminalView::new);
        view.update(cx, |terminal, cx| {
            terminal.set_window_focused(window.is_window_active(), cx);
        });
        let input = cx.subscribe_in(
            &view,
            window,
            |this, _, input: &TerminalViewInput, window, cx| {
                this.apply_terminal_view_input(input.0.clone(), window, cx);
            },
        );
        let scroll = cx.subscribe(&view, move |this, _, input: &TerminalScrollbarInput, cx| {
            this.scroll_terminal(pane_id.as_deref(), input, cx);
        });
        let [focus_in, focus_out] =
            Self::subscribe_terminal_focus(&view.focus_handle(cx), window, cx);
        TerminalPaneView {
            view,
            _subscriptions: [input, scroll, focus_in, focus_out],
        }
    }

    fn install_close_handler(window: &Window, cx: &Context<Self>) {
        let root = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            root.update(cx, |root, cx| {
                root.request_document_exit(false, window, cx);
                false
            })
            .unwrap_or(true)
        });
    }

    fn window_font_families(window: &Window) -> Arc<[String]> {
        let mut font_families = window.text_system().all_font_names();
        font_families.sort_unstable_by_key(|family| family.to_ascii_lowercase());
        font_families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        let font_families: Arc<[String]> = font_families.into();
        font_families
    }

    fn watch_workspace_work(
        repaint_rx: async_channel::Receiver<()>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let tray_window = cx.entity_id();
        cx.on_release(move |_, cx| crate::agent_tray::remove(tray_window, cx))
            .detach();
        cx.spawn_in(window, async move |weak, cx| {
            while repaint_rx.recv().await.is_ok() {
                if weak.update_in(cx, Self::process_work).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn create_settings_view(
        state: &AppState,
        font_families: &Arc<[String]>,
        integration_rows: &[ModuleIntegrationsSnapshot],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<GpuiSettings>, Subscription) {
        let settings_view = cx.new(|cx| {
            GpuiSettings::for_app(
                state,
                Arc::clone(font_families),
                integration_rows,
                window,
                cx,
            )
        });
        let settings_subscription =
            cx.subscribe(&settings_view, |this, _, intent: &SettingsIntent, cx| {
                this.apply_settings_intent(intent.clone(), cx);
            });
        (settings_view, settings_subscription)
    }

    fn create_keymap_editor(
        state: &AppState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<GpuiKeymapEditor>, Subscription) {
        let keymap_editor = cx
            .new(|cx| GpuiKeymapEditor::new_with_window(keymap_editor_snapshot(state), window, cx));
        let keymap_editor_subscription = cx.subscribe(
            &keymap_editor,
            |this, _, intent: &KeymapEditorIntent, cx| {
                this.apply_keymap_editor_intent(intent.clone(), cx);
            },
        );
        (keymap_editor, keymap_editor_subscription)
    }

    fn create_chrome_view(
        state: &AppState,
        launch: &WorkspaceLaunch,
        keymap_context: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<GpuiChrome>, Subscription) {
        let viewport = window.viewport_size();
        let projection = chrome_frame::prepare(
            state,
            &mut launch.native_chrome.borrow_mut(),
            true,
            window.is_window_active(),
        );
        let chrome_snapshot = chrome_frame::snapshot(
            state,
            &launch.native_chrome.borrow(),
            &projection,
            viewport.width.into(),
            viewport.height.into(),
        );
        let chrome_view = cx.new(|cx| {
            GpuiChrome::new(chrome_snapshot, window, cx)
                .with_keymap_context(keymap_context.to_owned())
        });
        let chrome_subscription =
            cx.subscribe(&chrome_view, |this, _, intent: &ChromeIntent, cx| {
                this.apply_chrome_intent(intent.clone(), cx);
            });
        (chrome_view, chrome_subscription)
    }

    fn observe_workspace_activation(
        terminal: &Entity<GpuiTerminalView>,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Subscription {
        let terminal_for_activation = terminal.clone();
        cx.observe_window_activation(window, move |this, window, cx| {
            let active = window.is_window_active();
            this.input.window_focused(active);
            this.frame_update_pending = true;
            terminal_for_activation.update(cx, |terminal, cx| {
                terminal.set_window_focused(active, cx);
            });
            for terminal in this.terminal_panes.values() {
                terminal.view.update(cx, |terminal, cx| {
                    terminal.set_window_focused(active, cx);
                });
            }
            if active
                && window.focused(cx).is_none()
                && !window.has_active_dialog(cx)
                && !this
                    .tools
                    .as_ref()
                    .is_some_and(|tools| tools.read(cx).browser_page_active(cx))
            {
                // Reactivation preserves the current component focus, including modal traps.
                schedule_focus(this.focus.clone(), window, cx);
            } else if !active {
                this.terminal_mouse_buttons.clear();
                this.pending_link_click = None;
            }
            cx.notify();
        })
    }

    fn observe_workspace_window(
        keymap_context: &str,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> (Subscription, Option<u32>) {
        let keymap_context_for_release = keymap_context.to_owned();
        cx.on_app_quit(|this, cx| {
            let owner = cx.entity();
            // Normal close already saved while the window and its workers were alive.
            let flush = this.state.take_session_checkpoint_flush(!this.exit_pending);
            let checkpoint = cx.background_executor().spawn(async move {
                flush();
            });
            let cleanup = this.close_link_forwards(cx);
            async move {
                checkpoint.await;
                cleanup.await;
                // GPUI clears windows before polling quit work; keep the admitted binding alive.
                drop(owner);
            }
        })
        .detach();
        cx.on_release(move |_, cx| {
            crate::gpui_actions::remove_workspace_key_bindings(&keymap_context_for_release, cx);
        })
        .detach();
        let window_appearance_subscription = cx.observe_window_appearance(window, |_, _, cx| {
            cx.notify();
        });
        let display_id = window
            .display(cx)
            .and_then(|display| u64::from(display.id()).try_into().ok());
        cx.observe_window_bounds(window, |this, window, cx| {
            // Preserve measured Root/title-bar insets while using the new viewport on
            // the resize's first frame, even when an occluded window cannot paint again.
            let viewport = window.viewport_size();
            this.workspace_bounds.size.width = px((f32::from(this.workspace_bounds.size.width)
                + f32::from(viewport.width)
                - f32::from(this.window_viewport.width))
            .max(0.0));
            this.workspace_bounds.size.height = px((f32::from(this.workspace_bounds.size.height)
                + f32::from(viewport.height)
                - f32::from(this.window_viewport.height))
            .max(0.0));
            this.window_viewport = viewport;
            cx.notify();
            // AppKit finishes rebuilding native controls after fullscreen bounds change.
            window.on_next_frame(|window, _| {
                crate::window::macos_enable_window_resizing(window);
            });
            // GPUI reports screen changes through this callback even if bounds stay equal.
            this.display_id = window
                .display(cx)
                .and_then(|display| u64::from(display.id()).try_into().ok());
        })
        .detach();
        (window_appearance_subscription, display_id)
    }

    fn schedule_initial_dock(window: &Window, cx: &mut Context<Self>) {
        cx.defer_in(window, |this, window, cx| {
            this.ensure_tools(window, cx);
            if let Some(dock) = &this.tools {
                dock.update(cx, |dock, cx| dock.set_inspector_visible(false, window, cx));
            }
        });
    }

    fn schedule_workspace_ui(window: &Window, cx: &mut Context<Self>) {
        Self::schedule_initial_dock(window, cx);
        Self::observe_browser_overlays(window, cx);
    }

    pub(crate) fn control_binding(
        &self,
    ) -> (BoundAppCommandSender, Arc<ControlCatalog>, ControlPlane) {
        let catalog = self.state.command_catalog();
        (
            self.state.app_command_sender(Caller::Socket),
            catalog.control_catalog(),
            self.launch.control_plane.clone(),
        )
    }

    fn open_workspace_window(&mut self, cx: &mut Context<Self>) {
        self.open_workspace_window_for_space(None, cx);
    }

    fn open_workspace_window_for_space(
        &mut self,
        space_id: Option<bootty_mux::controller::SpaceId>,
        cx: &mut Context<Self>,
    ) {
        let config = self.state.config().clone();
        let window_state_key = self.launch.next_window_state_key();
        if let Some(space_id) = space_id
            && !self
                .state
                .persist_space_selection_for_window(&window_state_key, space_id)
        {
            return;
        }
        let launch = self.launch.clone();
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            let window = Self::open_with_launch(config, window_state_key, launch, cx);
            match window {
                Ok((window, _)) => {
                    let _ = window.update(cx, |_, window, _| window.activate_window());
                }
                Err(error) => {
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace
                            .state
                            .record_error(format!("open Bootty window: {error}"));
                        cx.notify();
                    });
                }
            }
        });
    }

    pub(crate) fn open_setting_url(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scheme = bootty_config::ApplicationIdentity::for_process().namespace();
        let prefix = format!("{scheme}://settings/");
        let Some(id) = url.strip_prefix(&prefix) else {
            return;
        };
        let mut command = CommandInvocation::from_action("open_setting", Caller::Internal);
        command.arguments = vec![id.to_owned()];
        self.invoke_gpui_command(command, window, cx);
    }

    fn terminal_view_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.terminal.view.focus_handle(cx).is_focused(window)
            || self
                .terminal_panes
                .values()
                .any(|pane| pane.view.focus_handle(cx).is_focused(window))
    }

    fn subscribe_terminal_focus(
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> [Subscription; 2] {
        [
            cx.on_focus(focus, window, |this, window, cx| {
                this.state.apply_sidebar_action(
                    crate::app_actions::SidebarAction::FocusTerminal,
                    &mut Vec::new(),
                );
                this.sync_key_bindings(window, cx);
                cx.notify();
            }),
            cx.on_focus_out(focus, window, |this, _, window, cx| {
                this.sync_key_bindings(window, cx);
                cx.notify();
            }),
        ]
    }

    fn sync_key_bindings(&mut self, window: &Window, cx: &mut Context<Self>) {
        let snapshot = self.state.keymap_snapshot();
        let completion = self
            .dialogs
            .creation_view
            .read(cx)
            .completion_active(window, cx)
            || self
                .native_conversation_view(cx)
                .is_some_and(|view| view.read(cx).completion_active(window, cx));
        let focus = if completion {
            KeymapFocus::ComposerCompletion
        } else if self.state.modal_dialog().is_some() {
            self.state.keymap_focus()
        } else if self
            .tools
            .as_ref()
            .is_some_and(|tools| tools.read(cx).surface_chooser_focused(window, cx))
        {
            KeymapFocus::SurfaceChooser
        } else if self
            .tools
            .as_ref()
            .is_some_and(|tools| tools.read(cx).sessions_focused(window, cx))
        {
            self.state.focus_sidebar();
            KeymapFocus::Sidebar
        } else if self
            .tools
            .as_ref()
            .is_some_and(|tools| tools.focus_handle(cx).contains_focused(window, cx))
            && !self.terminal_view_focused(window, cx)
        {
            KeymapFocus::Other
        } else {
            self.state.keymap_focus()
        };
        let backend = self.state.multiplexer_backend();
        let native = !matches!(
            focus,
            KeymapFocus::SurfaceChooser | KeymapFocus::ComposerCompletion
        ) && ((self.state.modal_dialog().is_none()
            && self.selected_mux_pane_is_native())
            || matches!(
                self.state.modal_dialog(),
                Some(crate::state::ModalDialog::NewSession(dialog)) if dialog.is_creation_form()
            ));
        let keymap = (snapshot.revision, focus, backend, native);
        if self.last_keymap == Some(keymap) {
            return;
        }

        let bindings = if native {
            crate::gpui_actions::key_bindings_for_native_conversation(
                &snapshot,
                backend,
                &self.state.command_catalog(),
            )
        } else {
            crate::gpui_actions::key_bindings_for_snapshot(
                &snapshot,
                focus,
                backend,
                &self.state.command_catalog(),
            )
        };
        let navigation = bindings.navigation_hints();
        self.chrome_view
            .update(cx, |chrome, cx| chrome.set_navigation_hints(navigation, cx));
        let hints = bindings.command_hints(&self.state.command_catalog());
        self.dialogs.view.update(cx, |view, cx| {
            view.set_command_keybindings(Some(hints), cx);
        });
        if let Err(error) = crate::gpui_actions::replace_workspace_key_bindings_for_context(
            &self.keymap_context,
            bindings,
            cx,
        ) {
            self.state
                .record_error(format!("load GPUI workspace key bindings: {error:#}"));
        } else {
            // Native menu accelerators are built from the current GPUI keymap.
            #[cfg(target_os = "macos")]
            crate::menu::refresh(&self.state.localizer, cx);
        }
        self.last_keymap = Some(keymap);
    }

    fn apply_terminal_view_input(
        &mut self,
        input: crate::gpui::FrameInputSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = self.workspace_bounds.size;
        let cell = self.terminal_cell;
        let effects = self.state.update_frame(crate::gpui_input::frame_inputs(
            input,
            GpuiFrameFacts {
                now: Instant::now(),
                viewport: ViewportSnapshot {
                    fullscreen: window.is_fullscreen(),
                    maximized: window.is_maximized(),
                    content_height: viewport.height.into(),
                },
                display_id: self.display_id,
                renderer_metrics: self.renderer_metrics(cx),
                terminal_cell_width: cell.width,
                terminal_cell_height: cell.height,
                terminal_scale_factor: window.scale_factor(),
                terminal_view_transform: bootty_terminal::geometry::ViewTransform::default(),
            },
        ));
        self.apply_effects(effects, window, cx);
        cx.notify();
    }

    fn scroll_terminal(
        &mut self,
        pane: Option<&str>,
        input: &TerminalScrollbarInput,
        cx: &mut Context<Self>,
    ) {
        if self.state.modal_dialog().is_some() {
            return;
        }
        let expected_key = pane.map_or_else(
            || self.state.terminal_transition_key(),
            |pane| Some(self.state.pane_widget_key(pane)),
        );
        if input.transition_key != expected_key {
            return;
        }
        let result = match pane {
            Some(pane) if self.state.focused_pane().as_deref() == Some(pane) => {
                Some(self.state.terminal_mut().scroll_viewport_to(input.offset))
            }
            Some(pane) => self
                .state
                .terminal_runtime_for_pane(pane)
                .map(|runtime| runtime.scroll_viewport_to(input.offset)),
            None => Some(self.state.terminal_mut().scroll_viewport_to(input.offset)),
        };
        if let Some(Err(error)) = result {
            self.state.record_render_error(error);
        }
        cx.notify();
    }

    pub(crate) fn invoke_gpui_command(
        &mut self,
        invocation: CommandInvocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let displayed = self
            .chrome_view
            .read(cx)
            .displayed_sessions()
            .into_iter()
            .map(|target| {
                bootty_mux::workspace::ScopedSessionTarget::new(
                    bootty_mux::controller::SpaceId::from_persistence(target.scope.0),
                    target.session_id,
                )
            })
            .collect();
        self.state.set_displayed_sessions(displayed);
        if matches!(
            self.state
                .command_catalog()
                .resolve(invocation.clone())
                .map(|resolved| resolved.executor),
            Ok(crate::commands::CommandExecutor::Core(
                crate::commands::CoreCommandExecutor::Keybind(
                    crate::app_actions::KeybindAction::App(
                        crate::app_actions::AppAction::CommandPalette
                    )
                )
            ))
        ) {
            let parent = self
                .native_conversation_view(cx)
                .filter(|view| view.read(cx).contains_focused(window, cx))
                .and_then(|_| self.selected_native_conversation_target(cx));
            self.state.capture_command_palette_native_parent(parent);
        }
        let native_close = self
            .selected_native_conversation_target(cx)
            .and_then(|target| {
                crate::gpui_actions::native_conversation_close_invocation(
                    &invocation,
                    &self.state.command_catalog(),
                    &target,
                )
            });
        if native_close.is_some()
            && matches!(
                invocation.caller,
                Caller::Keybinding | Caller::BuiltinKeybinding
            )
            && !self
                .native_conversation_view(cx)
                .is_some_and(|view| view.read(cx).contains_focused(window, cx))
        {
            return;
        }
        let invocation = native_close.unwrap_or(invocation);
        let invocation = self
            .native_conversation_view(cx)
            .filter(|view| view.read(cx).contains_focused(window, cx))
            .and_then(|_| self.selected_native_conversation_target(cx))
            .and_then(|target| {
                crate::gpui_actions::native_surface_creation_invocation(
                    &invocation,
                    &self.state.command_catalog(),
                    &target,
                )
            })
            .unwrap_or(invocation);
        if crate::gpui_actions::conversation_terminal_navigation(
            &invocation,
            &self.state.command_catalog(),
        ) {
            self.close_native_conversation();
            if let Some(dock) = &self.tools {
                dock.update(cx, |dock, _| dock.cancel_restored_agent_destination());
            }
        }
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| tools.remember_focus(window, cx));
        }
        let viewport = self.workspace_bounds.size;
        let mut effects = Vec::new();
        let _ = self.state.dispatch_command(
            invocation,
            ViewportSnapshot {
                fullscreen: window.is_fullscreen(),
                maximized: window.is_maximized(),
                content_height: viewport.height.into(),
            },
            &mut effects,
        );
        self.apply_effects(effects, window, cx);
        cx.notify();
    }

    fn tools_match_binding(&self, cx: &gpui_kit::App) -> bool {
        self.state
            .current_command_target_for("git.open", bootty_control::ResourceKind::Binding)
            .is_some_and(|target| {
                self.tools.as_ref().is_some_and(|tools| {
                    tools.read(cx).matches_target(
                        &target,
                        self.state
                            .current_command_target_for(
                                "shell.prompt",
                                bootty_control::ResourceKind::Terminal,
                            )
                            .as_ref(),
                    )
                })
            })
    }

    fn ensure_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tools_match_binding(cx) {
            let cwd = self
                .state
                .terminal_mut()
                .current_working_directory()
                .ok()
                .flatten()
                .and_then(|cwd| {
                    bootty_mux::workspace::terminal_cwd_for_mux_command(Some(cwd), None)
                });
            let binding = &self.state.workspace.active.binding;
            let scope = binding.scope();
            if !binding.mux().has_session_snapshot() {
                return;
            }
            let Some(target) = self
                .state
                .current_command_target_for("git.open", bootty_control::ResourceKind::Binding)
            else {
                return;
            };
            let remote = binding.multiplexer().remote.as_ref();
            let host_identity = match bootty_host::files::host_identity(remote) {
                Ok(identity) => identity,
                Err(error) => {
                    self.state
                        .record_error(format!("identify file host: {error}"));
                    return;
                }
            };
            let context = crate::gpui_git_panel::GitPanelContext {
                terminal: self.state.current_command_target_for(
                    "shell.prompt",
                    bootty_control::ResourceKind::Terminal,
                ),
                target,
                directory: cwd
                    .or_else(|| {
                        binding
                            .mux()
                            .selected_session_anchor()
                            .and_then(|anchor| anchor.cwd.clone())
                    })
                    .unwrap_or_else(|| {
                        if remote.is_some() {
                            return "/".to_owned();
                        }
                        self.state
                            .config()
                            .session
                            .working_directory
                            .clone()
                            .or_else(bootty_config::config::default_working_directory)
                            .filter(|path| path.is_absolute())
                            .map_or_else(
                                || "/".to_owned(),
                                |path| path.to_string_lossy().into_owned(),
                            )
                    }),
                host: remote.map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label),
                host_identity,
                remote: remote.cloned(),
            };
            self.set_workspace_context(scope, context, false, window, cx);
        }
        cx.notify();
    }

    fn set_workspace_context(
        &mut self,
        scope: bootty_mux::controller::SpaceId,
        context: crate::gpui_git_panel::GitPanelContext,
        reveal_changes: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sender = self.state.app_command_sender(Caller::Internal);
        let path = self.state.config().config_path.clone();
        let key = self.state.window_state_key.clone();
        let local_git = self
            .state
            .workspace
            .binding(scope)
            .filter(|binding| binding.multiplexer().remote.is_none())
            .map(|_| self.launch.native_chrome.borrow().local_git.clone());
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.set_context(context.clone(), local_git, window, cx);
                if reveal_changes {
                    tools.browse_repository(context.directory, window, cx);
                }
            });
            return;
        }
        let owner = cx.weak_entity();
        let tools = cx.new(|cx| {
            crate::gpui_dock::WorkspaceDock::new(
                context,
                owner,
                self.terminal.view.clone(),
                self.chrome_view.clone(),
                scope,
                sender,
                &path,
                key,
                browser_profile_directory(),
                local_git,
                window,
                cx,
            )
        });
        if reveal_changes {
            tools.update(cx, |tools, cx| tools.refresh(window, cx));
        }
        self.tools_focus_subscription = Some(cx.subscribe_in(
            &tools,
            window,
            |this, _, _: &crate::gpui_dock::DockFocusChanged, window, cx| {
                this.sync_key_bindings(window, cx);
                cx.notify();
            },
        ));
        self.tools = Some(tools);
        cx.notify();
    }

    fn pending_documents(
        &self,
        quit: bool,
        cx: &gpui_kit::App,
    ) -> Vec<Entity<crate::gpui_document_panel::DocumentPanel>> {
        if quit {
            return crate::gpui_document_panel::Documents::pending(cx);
        }
        self.tools
            .iter()
            .flat_map(|tools| tools.read(cx).documents())
            .filter(|document| document.read(cx).needs_close_prompt(cx))
            .collect()
    }

    fn request_document_exit(&mut self, quit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.exit_pending {
            return;
        }
        let documents = self.pending_documents(quit, cx);
        if documents.is_empty() {
            self.finish_document_exit(quit, window, cx);
            return;
        }
        if self.document_close_prompt {
            return;
        }
        if documents
            .iter()
            .any(|document| document.read(cx).saving(cx))
        {
            self.state.record_error(
                "A document save is still running. Wait for its result before closing.",
            );
            return;
        }
        self.document_close_prompt = true;
        let detail = documents
            .iter()
            .map(|document| document.read(cx).path())
            .collect::<Vec<_>>()
            .join("\n");
        let answer = crate::gpui::prompt(
            "Unsaved documents",
            Some(&detail),
            &[
                "Save all and keep open".into(),
                "Discard and close".into(),
                "Cancel".into(),
            ],
            window,
            cx,
        );
        cx.spawn_in(window, async move |weak, cx| {
            let answer = answer.await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.document_close_prompt = false;
                match answer {
                    Ok(0) => {
                        for document in documents {
                            document.update(cx, super::gpui_document_panel::DocumentPanel::save);
                        }
                    }
                    Ok(1) => {
                        this.finish_document_exit(quit, window, cx);
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }

    fn close_link_forwards(&mut self, cx: &Context<Self>) -> gpui_kit::Task<()> {
        let forwards = self.state.take_link_forwards();
        cx.background_executor().spawn(async move {
            let now = Instant::now();
            let runner = bootty_host::CancellableCommandRunner::with_deadline(
                bootty_host::CommandCancellation::default(),
                now.checked_add(Duration::from_secs(1)).unwrap_or(now),
            );
            for forward in forwards {
                let _ = forward.close(&runner);
            }
        })
    }

    fn finish_document_exit(&mut self, quit: bool, window: &Window, cx: &mut Context<Self>) {
        self.exit_pending = true;
        let mut checkpoints = vec![Self::checkpoint_before_exit(cx)];
        if quit {
            // All workspace owners must save before GPUI's short final shutdown budget.
            let workspaces = cx
                .windows()
                .into_iter()
                .filter_map(|handle| handle.downcast::<gpui_kit::component::Root>())
                .filter_map(|handle| handle.read(cx).ok())
                .filter_map(|root| root.view().clone().downcast::<Self>().ok())
                .filter(|workspace| workspace.entity_id() != cx.entity_id())
                .collect::<Vec<_>>();
            for workspace in workspaces {
                let checkpoint = workspace.update(cx, |this, cx| {
                    this.exit_pending = true;
                    Self::checkpoint_before_exit(cx)
                });
                checkpoints.push(checkpoint);
            }
        }
        let cleanup = self.close_link_forwards(cx);
        cx.spawn_in(window, async move |weak, cx| {
            for checkpoint in checkpoints {
                checkpoint.await;
            }
            cleanup.await;
            _ = weak.update_in(cx, |_, window, cx| {
                if quit {
                    cx.quit();
                } else {
                    window.remove_window();
                }
            });
        })
        .detach();
    }

    fn checkpoint_before_exit(cx: &Context<Self>) -> gpui_kit::Task<()> {
        let owner = cx.entity();
        let timer = cx.background_executor().clone();
        cx.spawn(async move |weak, cx| {
            let deadline = Instant::now().checked_add(Duration::from_secs(5));
            let mut captured = false;
            loop {
                let pending = weak.update(cx, |this, _| {
                    this.state.poll_session_checkpoints();
                    if !captured && !this.state.session_checkpoint_pending() {
                        this.state
                            .checkpoint_sessions(crate::clock::ClockSnapshot::now().epoch);
                        captured = true;
                    }
                    this.state.session_checkpoint_pending()
                });
                if !matches!(pending, Ok(true)) {
                    break;
                }
                if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                    _ = weak.update(cx, |this, _| {
                        this.state
                            .record_error("Session checkpoint did not finish before closing");
                    });
                    break;
                }
                timer.timer(Duration::from_millis(1)).await;
            }
            drop(owner);
        })
    }

    fn apply_chrome_intent(&mut self, mut intent: ChromeIntent, cx: &mut Context<Self>) {
        let focus_terminal = matches!(
            &intent,
            ChromeIntent::Status(
                crate::gpui::chrome::StatusIntent::Action(
                    crate::gpui::chrome::NativeChromeAction::ActivateWindow { .. }
                ) | crate::gpui::chrome::StatusIntent::Context {
                    action: crate::gpui::chrome::TabContextAction::Activate,
                    ..
                }
            )
        );
        if (focus_terminal
            || matches!(
                &intent,
                ChromeIntent::Status(crate::gpui::chrome::StatusIntent::Action(
                    crate::gpui::chrome::NativeChromeAction::FocusConversation(_)
                ))
            ))
            && let Some(dock) = &self.tools
        {
            dock.update(cx, |dock, _| dock.cancel_restored_agent_destination());
        }
        if (focus_terminal
            || matches!(
                &intent,
                ChromeIntent::Status(crate::gpui::chrome::StatusIntent::Action(
                    crate::gpui::chrome::NativeChromeAction::FocusConversation(_)
                ))
            ))
            && let Some(id) = self
                .tools
                .as_ref()
                .and_then(|dock| dock.read(cx).outer_chooser_id())
        {
            self.state.commands.queue(CommandInvocation::new(
                "surface.cancel",
                vec![id.to_string()],
                Caller::Internal,
            ));
        }
        if let crate::gpui::chrome::ChromeIntent::Status(
            crate::gpui::chrome::StatusIntent::Action(
                crate::gpui::chrome::NativeChromeAction::CloseConversation(target),
            ),
        ) = &intent
        {
            let mut invocation =
                CommandInvocation::new("agents.native.close", Vec::new(), Caller::Internal);
            invocation.target = Some(target.clone());
            intent = ChromeIntent::Command(invocation);
        }
        if let ChromeIntent::Status(crate::gpui::chrome::StatusIntent::Action(
            crate::gpui::chrome::NativeChromeAction::CancelSurfaceChooser(id),
        )) = &intent
        {
            intent = ChromeIntent::Command(CommandInvocation::new(
                "surface.cancel",
                vec![id.to_string()],
                Caller::Internal,
            ));
        }
        if matches!(
            &intent,
            ChromeIntent::Status(
                crate::gpui::chrome::StatusIntent::Action(
                    crate::gpui::chrome::NativeChromeAction::ActivateWindow { .. }
                ) | crate::gpui::chrome::StatusIntent::Context { .. }
            )
        ) {
            self.close_native_conversation();
        }
        if intent == ChromeIntent::StartWindowDrag {
            self.pending_window_move = true;
        } else {
            self.pending_effects
                .extend(chrome_frame::apply(&mut self.state, intent));
            if focus_terminal {
                self.pending_effects.push(AppEffect::FocusTerminal);
            }
        }
        cx.notify();
    }

    fn apply_settings_intent(&mut self, intent: SettingsIntent, cx: &mut Context<Self>) {
        let prior_providers = self.state.config().agents.clone();
        let provider_terminal = match &intent {
            SettingsIntent::Invoke(id) => bootty_agents::AgentKind::ALL.iter().any(|provider| {
                ["account.login", "tab", "history.open", "provider.update"]
                    .iter()
                    .any(|operation| id == &format!("agents.{provider}.{operation}"))
            }),
            _ => false,
        };
        let close = matches!(intent, SettingsIntent::Close) || provider_terminal;
        let open_keymap = matches!(&intent, SettingsIntent::Invoke(id) if id == "keymap:open");
        let open_config = matches!(&intent, SettingsIntent::Invoke(id) if id == "config:edit");
        if matches!(intent, SettingsIntent::DiscardChanges) {
            self.state.reload_config(&mut self.pending_effects);
        }
        if let SettingsIntent::Invoke(id) = &intent {
            if provider_terminal || id.ends_with(".provider.status") {
                let mut invocation =
                    CommandInvocation::new(id, Vec::new(), bootty_control::Caller::Internal);
                if provider_terminal {
                    invocation.target = self
                        .state
                        .current_command_target_for(id, bootty_control::ResourceKind::Session);
                }
                self.state.commands.queue(invocation);
                (self.repaint)();
            } else if id == "config:reload" {
                self.state.reload_config(&mut self.pending_effects);
            } else if let Some(action) = id.strip_prefix("panel:show:")
                && let Some(action) = crate::commands::DockAction::PANELS
                    .into_iter()
                    .find(|candidate| candidate.command().action() == action)
            {
                self.pending_effects
                    .push(AppEffect::Dock(crate::commands::DockRequest::local(action)));
            }
        }
        self.settings_view
            .update(cx, |view, _| view.apply_edit(intent, &self.state));
        self.flush_settings_effects(cx);
        if prior_providers != self.state.config().agents {
            self.refresh_terminal_provider_statuses();
        }
        // Settings has its own window. Publish global font changes even when the workspace
        // is occluded and cannot render its queued effects yet. Consume them in order so an
        // older queued selection cannot overwrite the latest accepted font later.
        self.pending_effects.retain(|effect| match effect {
            AppEffect::SetUiFonts(families) => {
                crate::gpui::update_ui_font_families(families, cx);
                false
            }
            AppEffect::SetUiFontSize(size) => {
                crate::gpui::update_ui_font_size(*size, cx);
                false
            }
            AppEffect::SetUiFontWeights(weights) => {
                crate::gpui::update_ui_font_weights(weights, cx);
                false
            }
            _ => true,
        });
        let active_appearance_variant = self
            .state
            .config()
            .appearance
            .mode
            .variant(self.state.active_appearance_variant());
        if self.last_locale != self.state.localizer.locale() {
            self.last_locale = self.state.localizer.locale().to_owned();
            crate::i18n::publish(&self.state.localizer, cx);
        }
        self.sync_zed_theme(active_appearance_variant, cx);

        if open_keymap {
            self.request_keymap_window(None, cx);
        }
        if open_config {
            self.request_config_editor_tab(cx);
        }
        if close && self.settings_view.read(cx).draft.write_error().is_none() {
            self.close_settings_window(cx);
        } else {
            self.refresh_settings(cx);
        }
        cx.notify();
    }

    fn refresh_settings(&mut self, cx: &mut Context<Self>) {
        self.last_settings_revision = Some(self.state.config_revision());
        self.last_provider_revision = self
            .state
            .terminal_agent_service()
            .map(|service| service.revision());
        let changed = self.settings_view.update(cx, |view, cx| {
            view.reconcile(&self.state, &self.integration_rows, cx)
        });
        if changed && let Some(window) = self.settings_window.clone() {
            cx.defer(move |cx| {
                let _ = window.update_in(cx, |_, _, cx| cx.notify());
            });
        }
    }

    fn flush_settings_effects(&mut self, cx: &mut Context<Self>) {
        loop {
            let effects = self
                .settings_view
                .update(cx, |view, _| view.draft.take_effects());
            if effects.is_empty() {
                break;
            }
            for effect in effects {
                let (outcomes, effects) =
                    self.settings_runtime
                        .apply(effect, &mut self.state, &self.repaint);
                self.pending_effects.extend(effects);
                for outcome in outcomes {
                    self.settings_view
                        .update(cx, |view, _| view.draft.apply_outcome(outcome));
                }
            }
        }
    }

    fn apply_simple_fullscreen(enabled: bool, window: &Window) {
        if window.is_simple_fullscreen() != enabled {
            window.toggle_simple_fullscreen();
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Dispatch every application effect exhaustively through its owner"
    )]
    fn apply_effects(
        &mut self,
        effects: Vec<AppEffect>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for effect in effects {
            match effect {
                AppEffect::CloseWindow => self.request_document_exit(false, window, cx),
                AppEffect::OpenWindow => self.open_workspace_window(cx),
                AppEffect::OpenSpaceWindow(space_id) => {
                    self.open_workspace_window_for_space(Some(space_id), cx);
                }
                AppEffect::QuitApplication => self.request_document_exit(true, window, cx),
                AppEffect::SetWindowTitle(title) => window.set_window_title(&title),
                AppEffect::SetFullscreen(fullscreen) => {
                    if window.is_fullscreen() != fullscreen {
                        window.toggle_fullscreen();
                    }
                }
                AppEffect::SetMaximized(maximized) => {
                    if window.is_maximized() != maximized {
                        window.zoom_window();
                    }
                }
                AppEffect::SetDecorations(decorated) => {
                    self.set_window_decorations(decorated, window);
                }
                // GPUI performs the native copy key equivalent itself but exposes no
                // request-copy event equivalent for an application view.
                AppEffect::RequestCopy => {}
                // `update_frame` runs while GPUI is rendering this entity. Invalidating the
                // window synchronously from that render recursively extends `flush_effects` and
                // can prevent application launch from ever returning to AppKit. Cross the async
                // boundary first; one millisecond remains below a display frame.
                AppEffect::RequestRepaint => {
                    self.schedule_repaint_after(Duration::from_millis(1), cx);
                }
                AppEffect::Bell => self.ring_bell(window, cx),
                AppEffect::DesktopNotification { title, body } => {
                    Self::notify_desktop(title, body, cx);
                }
                AppEffect::RepaintAfter(after) => {
                    self.schedule_maintenance_after(after, window, cx);
                }
                AppEffect::SetTerminalTextConfig(config) => {
                    self.apply_terminal_text_config(config, window);
                }
                AppEffect::SetTerminalCursorIcon(icon) => {
                    self.terminal_cursor = gpui_cursor(icon);
                    self.cursor = self.terminal_cursor;
                }
                AppEffect::SetUiFonts(families) => {
                    crate::gpui::update_ui_font_families(&families, cx);
                }
                AppEffect::SetUiFontWeights(weights) => {
                    crate::gpui::update_ui_font_weights(&weights, cx);
                }
                AppEffect::SetUiFontSize(size) => {
                    crate::gpui::update_ui_font_size(size, cx);
                }
                AppEffect::FocusTerminal => self.focus_terminal_surface(window, cx),
                AppEffect::SetWindowFocus => window.activate_window(),
                AppEffect::ApplyMacosNonNativeFullscreen => {
                    Self::apply_simple_fullscreen(true, window);
                }
                AppEffect::RestoreMacosPresentation => {
                    Self::apply_simple_fullscreen(false, window);
                }
                AppEffect::OpenUrl(url) => cx.open_url(&url),
                AppEffect::NativeConversation(target) => {
                    self.open_native_conversation(target, window, cx);
                }
                AppEffect::OpenSurfaceChooser(request) => {
                    self.open_surface_chooser(request, window, cx);
                }
                AppEffect::OpenSurfaceAgentForm(request) => {
                    self.open_surface_agent_form(&request, cx);
                }
                AppEffect::NavigateSurfaceChooser { id, action } => {
                    self.navigate_surface_chooser(id, action, window, cx);
                }
                AppEffect::CloseSurfaceChooser(id) => self.close_surface_chooser(id, window, cx),
                AppEffect::AttachNewSurface { request_id, target } => {
                    self.attach_new_surface(request_id, target, window, cx);
                }
                AppEffect::CloseNativeConversation(target) => {
                    self.apply_native_close(&target, window, cx);
                }
                AppEffect::Dock(request) => self.apply_dock_request(request, window, cx),
                AppEffect::Browser(request) => self.apply_browser_request(request, window, cx),
                AppEffect::OpenGitChanges {
                    scope,
                    target,
                    directory,
                    host,
                } => self.open_git_changes(scope, target, directory, host, window, cx),
                AppEffect::OpenFiles(request) => self.open_files(request, window, cx),
                AppEffect::OpenSettings => self.open_settings_window(window, cx),
                AppEffect::OpenThemeSettings => {
                    self.open_settings_window_target(SettingsWindowTarget::Theme, window, cx);
                }
                AppEffect::OpenSetting(id) => {
                    self.open_settings_window_target(SettingsWindowTarget::Setting(id), window, cx);
                }
                AppEffect::ComposerAction(request) => {
                    if let Err(error) = request.begin() {
                        request.complete(crate::commands::runtime::command_outcome_for_mux_error(
                            error,
                        ));
                        continue;
                    }
                    let action = request.action;
                    if window.has_active_dialog(cx)
                        && matches!(
                            self.state.modal_dialog(),
                            None | Some(crate::state::ModalDialog::NewSession(_))
                        )
                    {
                        window.close_dialog(cx);
                    }
                    let performed = self
                        .dialogs
                        .creation_view
                        .update(cx, |view, cx| view.perform_completion(action, window, cx))
                        || self.native_conversation_view(cx).is_some_and(|view| {
                            view.update(cx, |view, cx| view.perform_completion(action, window, cx))
                        });
                    request.complete(if performed {
                        bootty_control::CommandOutcome::Success {
                            warnings: Vec::new(),
                            value: serde_json::Value::Null,
                        }
                    } else {
                        bootty_control::CommandOutcome::Unavailable {
                            message: "This composer action is not available".into(),
                        }
                    });
                }
                AppEffect::CommandAction(action) => {
                    self.dialogs
                        .view
                        .update(cx, |view, cx| view.perform(action, window, cx));
                }
                AppEffect::ConfigureKeybind(action) => {
                    self.open_keymap_window(Some(action), window, cx);
                }
            }
        }
    }

    fn set_window_decorations(&self, decorated: bool, window: &Window) {
        window.request_decorations(if decorated {
            crate::platform::window_decorations(&self.state.config().window)
        } else {
            WindowDecorations::Client
        });
    }

    fn focus_terminal_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sync_selected_native_pane(window, cx) {
            return;
        }
        self.close_native_conversation();
        let selected = self.state.workspace.active.binding.current_window_id();
        if let Some(dock) = self.tools.clone() {
            dock.update(cx, |dock, cx| {
                dock.select_terminal_panel(&selected, window, cx);
            });
        }
        cx.activate(true);
        window.activate_window();
        self.focus.focus(window, cx);
    }

    fn apply_native_close(
        &mut self,
        target: &bootty_control::CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_native_conversation_target(cx).as_ref() == Some(target) {
            self.close_native_conversation();
            self.focus_terminal_surface(window, cx);
        }
    }

    fn apply_terminal_text_config(&mut self, config: TerminalTextConfig, window: &mut Window) {
        self.terminal_base_cell = terminal_cell_metrics(&config, window);
        self.terminal_cell = self.terminal_base_cell;
        self.terminal_text_contract = Arc::new(TerminalTextContract::new(
            config,
            NativeSymbolPolicy::default(),
        ));
    }

    fn open_files(
        &mut self,
        request: crate::state::OpenFilesRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let crate::state::OpenFilesRequest {
            scope,
            target,
            host,
            path,
            document,
            line,
            column,
        } = request;
        let reuse = self.tools.as_ref().is_some_and(|tools| {
            tools.read(cx).matches_target(
                &target,
                self.state
                    .current_command_target_for(
                        "shell.prompt",
                        bootty_control::ResourceKind::Terminal,
                    )
                    .as_ref(),
            )
        });
        let remote = self
            .state
            .workspace
            .binding(scope)
            .and_then(|binding| binding.multiplexer().remote.as_ref());
        let host_identity = match bootty_host::files::host_identity(remote) {
            Ok(identity) => identity,
            Err(error) => {
                self.state
                    .record_error(format!("identify file host: {error}"));
                return;
            }
        };
        let directory = if !document {
            path.clone()
        } else if remote.is_some() {
            path.rsplit_once('/').map_or_else(
                || "/".to_owned(),
                |(parent, _)| {
                    if parent.is_empty() {
                        "/".to_owned()
                    } else {
                        parent.to_owned()
                    }
                },
            )
        } else {
            std::path::Path::new(&path).parent().map_or_else(
                || path.clone(),
                |parent| parent.to_string_lossy().into_owned(),
            )
        };
        let context = crate::gpui_git_panel::GitPanelContext {
            terminal: self
                .state
                .current_command_target_for("shell.prompt", bootty_control::ResourceKind::Terminal),
            target,
            directory,
            host,
            host_identity,
            remote: remote.cloned(),
        };
        if !reuse {
            self.set_workspace_context(scope, context, false, window, cx);
        }
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.resume(cx);
                if document {
                    tools.open_document(path, line, column, window, cx);
                } else {
                    tools.browse_files(path, window, cx);
                }
            });
        }
    }

    fn open_git_changes(
        &mut self,
        scope: bootty_mux::controller::SpaceId,
        target: bootty_control::CommandTarget,
        directory: String,
        host: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self
            .state
            .workspace
            .binding(scope)
            .and_then(|binding| binding.multiplexer().remote.as_ref());
        let host_identity = match bootty_host::files::host_identity(remote) {
            Ok(identity) => identity,
            Err(error) => {
                self.state
                    .record_error(format!("identify file host: {error}"));
                return;
            }
        };
        self.set_workspace_context(
            scope,
            crate::gpui_git_panel::GitPanelContext {
                terminal: self.state.current_command_target_for(
                    "shell.prompt",
                    bootty_control::ResourceKind::Terminal,
                ),
                target,
                directory,
                host,
                host_identity,
                remote: remote.cloned(),
            },
            true,
            window,
            cx,
        );
    }

    fn observe_browser_overlays(window: &Window, cx: &mut Context<Self>) {
        cx.observe_global_in::<gpui_kit::base::GlobalState>(window, |this, window, cx| {
            this.observe_browser_overlay(window, cx);
        })
        .detach();
    }

    fn observe_browser_overlay(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !gpui_kit::base::GlobalState::is_in_deferred_context(cx) {
            return;
        }
        let Some(tools) = &self.tools else {
            return;
        };
        if !tools.read(cx).browser_has_native_views(cx) {
            return;
        }
        tools.update(cx, |tools, cx| tools.set_browser_visible(false, cx));
        if self.browser_overlay_task.is_some() {
            return;
        }
        // The pinned Kit notifies when a popup opens, but token drop does not notify dismissal.
        // Poll only that open popup's lifetime; remove this task when Kit exposes dismissal events.
        self.browser_overlay_task = Some(cx.spawn_in(window, async move |owner, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                let keep_waiting = owner
                    .update_in(cx, |this, _, cx| {
                        let has_views = this
                            .tools
                            .as_ref()
                            .is_some_and(|tools| tools.read(cx).browser_has_native_views(cx));
                        if has_views && gpui_kit::base::GlobalState::is_in_deferred_context(cx) {
                            return true;
                        }
                        this.browser_overlay_task = None;
                        cx.notify();
                        false
                    })
                    .unwrap_or(false);
                if !keep_waiting {
                    break;
                }
            }
        }));
    }

    fn apply_browser_request(
        &mut self,
        request: crate::commands::BrowserRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tools(window, cx);
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| tools.execute_browser(request, window, cx));
        } else {
            request.complete(bootty_control::CommandOutcome::Unavailable {
                message: "This window has no workspace sidebar.".into(),
            });
        }
    }

    fn apply_dock_request(
        &mut self,
        mut request: crate::commands::DockRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            request.action.panel(),
            Some(
                bootty_config::config::PanelKind::Files | bootty_config::config::PanelKind::Changes
            )
        ) {
            request.directory = self
                .state
                .terminal_mut()
                .current_working_directory()
                .ok()
                .flatten()
                .and_then(|cwd| {
                    bootty_mux::workspace::terminal_cwd_for_mux_command(Some(cwd), None)
                })
                .or_else(|| {
                    self.state
                        .workspace
                        .active
                        .binding
                        .mux()
                        .selected_session_anchor()
                        .and_then(|anchor| anchor.cwd.clone())
                });
        }
        self.ensure_tools(window, cx);
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.update_agents(
                    self.state.agent_overview(),
                    self.state
                        .current_command_target(bootty_control::ResourceKind::Terminal),
                    cx,
                );
                tools.apply_request(request, window, cx);
            });
        } else {
            request.complete(bootty_control::CommandOutcome::Unavailable {
                message: "No workspace is available for this dock command".into(),
            });
        }
    }

    fn notify_desktop(title: String, body: String, cx: &Context<Self>) {
        let work = cx
            .background_executor()
            .spawn(async move { crate::platform::show_desktop_notification(&title, &body) });
        cx.spawn(async move |weak, cx| {
            if let Err(error) = work.await {
                _ = weak.update(cx, |this, cx| {
                    this.state.record_error(error);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn ring_bell(&mut self, window: &Window, cx: &mut Context<Self>) {
        let mode = self.state.config().session.bell;
        if mode.audio() {
            window.play_system_bell();
        }
        if mode.visual() {
            self.visual_bell_until = Instant::now().checked_add(Duration::from_millis(250));
            self.schedule_repaint_after(Duration::from_millis(250), cx);
            cx.notify();
        }
    }

    fn advance_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.frame_update_pending = false;
        let viewport = self.workspace_bounds.size;
        let cell = self.terminal_cell;
        let renderer_metrics = self.renderer_metrics(cx);
        let frame_inputs = crate::gpui_input::drain_frame_inputs(
            &mut self.input,
            GpuiFrameFacts {
                now: Instant::now(),
                viewport: ViewportSnapshot {
                    fullscreen: window.is_fullscreen(),
                    maximized: window.is_maximized(),
                    content_height: viewport.height.into(),
                },
                display_id: self.display_id,
                renderer_metrics,
                terminal_cell_width: cell.width,
                terminal_cell_height: cell.height,
                terminal_scale_factor: window.scale_factor(),
                terminal_view_transform: bootty_terminal::geometry::ViewTransform::default(),
            },
        );
        let mut effects = self.state.update_frame(frame_inputs);
        self.state.show_creation_for_empty_workspace();
        effects.append(&mut self.pending_effects);
        let changed = effects
            .iter()
            .any(|effect| !matches!(effect, AppEffect::RepaintAfter(_)));
        self.apply_effects(effects, window, cx);
        changed
    }

    fn maintenance_chrome(&self, window: &Window) -> ChromeSnapshot {
        let projection = chrome_frame::prepare(
            &self.state,
            &mut self.launch.native_chrome.borrow_mut(),
            true,
            window.is_window_active(),
        );
        let viewport = self.workspace_bounds.size;
        chrome_frame::snapshot(
            &self.state,
            &self.launch.native_chrome.borrow(),
            &projection,
            viewport.width.into(),
            viewport.height.into(),
        )
    }

    fn renderer_metrics(&self, cx: &App) -> RendererMetrics {
        let focused = self.state.focused_pane();
        self.visible_terminals
            .iter()
            .find(|terminal| terminal.pane_id.is_none() || terminal.pane_id == focused)
            .and_then(|terminal| terminal.view.as_ref())
            .map_or_else(RendererMetrics::default, |view| view.read(cx).metrics())
    }

    fn terminal_frames_changed(&mut self, cx: &App) -> bool {
        // Views own the presented frame; retained but hidden panes do not drive painting.
        for terminal in &self.visible_terminals {
            let Some(view) = &terminal.view else {
                return true;
            };
            let Some(runtime) = self
                .state
                .workspace
                .active
                .binding
                .visible_terminal_frame_source(terminal.pane_id.as_deref())
            else {
                return true;
            };
            match runtime.extract_frame() {
                Ok(frame) if view.read(cx).presents_frame(&frame) => {}
                Ok(_) => return true,
                Err(error) => {
                    self.state.record_error(error);
                    return true;
                }
            }
        }
        false
    }

    fn sync_agents(&self, cx: &mut Context<Self>) {
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.update_agents(
                    self.state.agent_overview(),
                    self.state
                        .current_command_target(bootty_control::ResourceKind::Terminal),
                    cx,
                );
            });
        }
        crate::agent_tray::update(
            cx.entity_id(),
            self.state.agent_overview(),
            self.state.app_command_sender(Caller::Internal),
            cx,
        );
    }

    fn sync_browser_configuration(&self, window: &Window, cx: &mut Context<Self>) {
        let Some(tools) = &self.tools else {
            return;
        };
        let conversation_target = self.shown_native_conversation_target(cx);
        let attachment = self.browser_attachment_context(conversation_target.as_ref());
        tools.update(cx, |tools, cx| {
            tools.configure_browser(
                self.state.config().browser,
                conversation_target,
                self.state
                    .current_command_target(bootty_control::ResourceKind::ApplicationWindow),
                attachment,
                window,
                cx,
            );
        });
    }

    fn poll_settings_runtime(&mut self, cx: &mut Context<Self>) -> bool {
        let mut settings_changed = false;
        if let Some(catalog) = self.settings_runtime.drain_catalog() {
            settings_changed = true;
            self.integration_rows = catalog.integration_rows;
        }
        for outcome in self.settings_runtime.drain_outcomes() {
            settings_changed = true;
            self.settings_view
                .update(cx, |view, _| view.draft.apply_outcome(outcome));
        }
        settings_changed
    }

    fn process_work(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let revision = self.state.config_revision();
        self.refresh_native_conversations(window, cx);
        let error = self.state.last_error();
        let frame_changed = self.advance_frame(window, cx);
        let mut changed = frame_changed;
        self.sync_agents(cx);
        let settings_changed = self.poll_settings_runtime(cx);
        let provider_revision = self
            .state
            .terminal_agent_service()
            .map(|service| service.revision());
        if (self.settings_window.is_some() || self.settings_window_opening)
            && (settings_changed
                || self.last_settings_revision != Some(self.state.config_revision())
                || self.last_provider_revision != provider_revision)
        {
            self.refresh_settings(cx);
            changed = true;
        }
        let binding = &self.state.workspace.active.binding;
        let layouts = binding
            .mux()
            .all_sessions()
            .iter()
            .flat_map(|session| {
                session.windows.iter().filter_map(|window| {
                    binding.window_pane_layout(&session.id, &window.id).cloned()
                })
            })
            .collect::<Vec<_>>();
        changed |= self.last_pane_layouts != layouts;
        self.last_pane_layouts = layouts;
        let usage_visible = self.state.config().chrome.sidebar
            && self
                .state
                .config()
                .sidebar
                .modules
                .iter()
                .any(|module| module == "codexbar");
        self.launch
            .native_chrome
            .borrow_mut()
            .refresh(Instant::now(), usage_visible);
        let terminal_changed = self.terminal_frames_changed(cx);
        let chrome = self.maintenance_chrome(window);
        let chrome_changed = self.last_maintenance_chrome.as_ref() != Some(&chrome);
        self.last_maintenance_chrome = Some(chrome);
        let changed = changed
            || terminal_changed
            || chrome_changed
            || revision != self.state.config_revision()
            || error != self.state.last_error();
        // The one-slot wake channel coalesces work. A frame callback can be missed after a
        // child-only chrome paint, so do not gate the next wake on a later root frame.
        if changed {
            cx.notify();
            window.refresh();
            true
        } else {
            false
        }
    }

    fn schedule_maintenance_after(&mut self, after: Duration, window: &Window, cx: &Context<Self>) {
        let now = Instant::now();
        let deadline = now.checked_add(after).unwrap_or(now);
        if self
            .scheduled_maintenance
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return;
        }
        self.scheduled_maintenance = Some(deadline);
        cx.spawn_in(window, async move |weak, cx| {
            cx.background_executor().timer(after).await;
            let _ = weak.update_in(cx, |this, window, cx| {
                if this.scheduled_maintenance != Some(deadline) {
                    return;
                }
                this.scheduled_maintenance = None;
                this.process_work(window, cx);
            });
        })
        .detach();
    }

    fn schedule_repaint_after(&mut self, after: std::time::Duration, cx: &Context<Self>) {
        let now = Instant::now();
        let deadline = now.checked_add(after).unwrap_or(now);
        if self
            .scheduled_repaint
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return;
        }
        self.scheduled_repaint = Some(deadline);
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(after).await;
            let _ = weak.update(cx, |this, cx| {
                if this.scheduled_repaint == Some(deadline) {
                    this.scheduled_repaint = None;
                    this.frame_update_pending = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn sync_zed_theme(
        &mut self,
        active_appearance_variant: AppearanceVariant,
        cx: &mut Context<Self>,
    ) {
        let appearance_changed =
            active_appearance_variant != self.state.active_appearance_variant();
        if appearance_changed {
            self.state.set_appearance_variant(active_appearance_variant);
        }

        let theme = self.state.ui_theme();
        // Font-size and other non-color changes must not rebuild both UI themes and
        // invalidate every application window.
        if theme != self.last_ui_theme {
            crate::gpui::update_ui_theme(theme, cx);
            self.last_ui_theme = theme;
        }
    }

    fn sync_error_notification(
        &mut self,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The find bar already presents this validation error. Command callers have received
        // their failure outcome; dismiss the duplicate host notice before it covers the field.
        let error = if error.is_some() && error.as_deref() == self.state.terminal_find_error() {
            self.state.clear_last_error();
            None
        } else {
            error
        };
        if self.last_error_notification == error {
            return;
        }
        self.last_error_notification.clone_from(&error);
        match error {
            Some(error) => {
                let workspace = cx.weak_entity();
                window.push_notification(
                    Notification::error(ErrorNotice::from_text(error).to_string())
                        .id::<BoottyErrorNotification>()
                        .autohide(false)
                        .on_close(move |_, cx| {
                            let _ = workspace.update(cx, |workspace, cx| {
                                workspace.state.clear_last_error();
                                cx.notify();
                            });
                        }),
                    cx,
                );
            }
            None => window.remove_notification::<BoottyErrorNotification>(cx),
        }
    }

    fn sync_dialog_overlay(
        &mut self,
        projection: Option<crate::presentation::dialogs::DialogProjection>,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_dismissed_surface_form(window, cx);
        let terminal_focus = (self.state.terminal_focused()
            && self.state.pending_new_surface().is_none())
        .then(|| {
            self.native_conversation_view(cx)
                .map_or_else(|| self.focus.clone(), |view| view.focus_handle(cx))
        });
        self.dialogs.present(
            projection,
            self.state.creation_underlay(),
            colors,
            terminal_focus,
            window,
            cx,
        );
    }

    pub(crate) fn prepare_terminal_window(
        &mut self,
        target: &bootty_control::CommandTarget,
        id: &bootty_mux::workspace::ScopedWindowId,
        geometry: bootty_terminal::geometry::TerminalGeometry,
        cx: &mut Context<Self>,
    ) {
        if !self.terminal_window_matches_binding(target, id) {
            return;
        }
        let binding = &mut self.state.workspace.active.binding;
        if let Err(error) = binding.prepare_window(id.session_id(), id.window_id(), geometry) {
            self.state.record_render_error(error);
        }
        cx.notify();
    }

    fn docked_terminal_surface(
        &mut self,
        dock: Entity<crate::gpui_dock::WorkspaceDock>,
        window: &mut Window,
        area: SurfaceRect,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.visible_terminals.clear();
        self.sync_dock_task_center(&dock, window, cx);
        if let Some(empty_terminal) = self.empty_terminal_state() {
            dock.update(cx, |dock, cx| {
                dock.reconcile_terminal_surfaces(&self.state.workspace.active.binding, window, cx);
                dock.clear_terminals(window, cx);
                dock.set_empty_terminal(Some(empty_terminal), cx);
            });
        } else {
            dock.update(cx, |dock, cx| dock.set_empty_terminal(None, cx));
            if self.state.uses_native_terminal_layout() {
                self.prepare_dock_terminals(&dock, window, colors, cx);
            } else {
                self.prepare_dock_attachment(&dock, window, colors, cx);
            }
        }

        div()
            .absolute()
            .left(px(area.min_x))
            .top(px(area.min_y))
            .w(px(area.width()))
            .h(px(area.height()))
            .child(dock)
            .into_any_element()
    }

    fn retain_live_terminal_panes(&mut self) {
        let live_keys = self
            .state
            .mux()
            .sessions()
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .filter_map(|pane| pane.pane_id.as_deref())
            .map(|pane| self.state.pane_widget_key(pane))
            .collect::<HashSet<_>>();
        self.terminal_panes.retain(|key, _| live_keys.contains(key));
    }

    fn empty_terminal_state(&self) -> Option<crate::workspace_composition::EmptyTerminalState> {
        let binding = &self.state.workspace.active.binding;
        let mux = binding.mux();
        let selected = binding.current_window_id();
        let session = mux.session_by_id_or_name(selected.session_id());
        let no_terminals = !crate::workspace_composition::has_selected_terminal(
            mux.sessions(),
            &selected,
            binding.uses_native_terminal_layout(),
        );
        no_terminals.then(|| {
            let operation = if session.is_none() {
                bootty_mux::capability::BindingOperation::CreateProjectSession
            } else {
                bootty_mux::capability::BindingOperation::CreateWindow
            };
            crate::workspace_composition::EmptyTerminalState::from_snapshot(
                mux.has_session_snapshot(),
                mux.unavailable_reason(),
                self.state
                    .workspace
                    .active
                    .binding
                    .capabilities()
                    .supports(operation),
            )
        })
    }

    fn prepare_dock_attachment(
        &mut self,
        dock: &Entity<crate::gpui_dock::WorkspaceDock>,
        window: &mut Window,
        colors: Colors,
        cx: &mut Context<Self>,
    ) {
        let panel = dock.update(cx, |dock, cx| dock.attachment_panel(window, cx));
        let (active, bounds) = {
            let data = panel.read(cx);
            (data.active, data.bounds)
        };
        let native_layout = bounds.map_or_else(Vec::new, |bounds| {
            let panel_area = SurfaceRect {
                min_x: bounds.left().into(),
                min_y: bounds.top().into(),
                max_x: bounds.right().into(),
                max_y: bounds.bottom().into(),
            };
            if active {
                self.terminal_element(window, panel_area, colors, cx);
            }
            self.state
                .workspace
                .active
                .binding
                .native_agent_pane_rects(panel_area, self.state.config().chrome.pane_divider_width)
        });
        panel.update(cx, |panel, cx| {
            panel.publish_native_layout(self.native_conversations.revision, native_layout, cx);
        });
        let mux = self.state.mux();
        let title = mux
            .sessions()
            .iter()
            .find(|session| {
                Some(session.id.as_str()) == mux.selected_session()
                    || Some(session.name.as_str()) == mux.selected_session()
            })
            .map_or_else(|| "Terminal".to_owned(), |session| session.name.clone());
        panel.update(cx, |panel, cx| panel.set_title(title, cx));
    }

    fn prepare_dock_panel(
        &mut self,
        panel: &Entity<crate::gpui_terminal_panel::TerminalPanel>,
        windows: &[crate::gpui_dock::TerminalWindowPresentation],
        window: &mut Window,
        colors: Colors,
        cx: &mut Context<Self>,
    ) {
        let data = panel.read(cx);
        if data.active
            && data.visible
            && let Some(bounds) = data.bounds
        {
            let id = data.window_id.clone();
            let panel_area = SurfaceRect {
                min_x: bounds.left().into(),
                min_y: bounds.top().into(),
                max_x: bounds.right().into(),
                max_y: bounds.bottom().into(),
            };
            let surface = self.fit_terminal_surface(panel_area, window);
            let pane_ids = self
                .state
                .mux()
                .sessions()
                .iter()
                .find(|session| session.id == id.session_id())
                .and_then(|session| {
                    session
                        .windows
                        .iter()
                        .find(|window| window.id == id.window_id())
                })
                .map_or_else(Default::default, |window| {
                    window
                        .panes
                        .iter()
                        .filter_map(|pane| pane.pane_id.clone())
                        .collect()
                });
            panel.update(cx, |panel, cx| {
                panel.prepare(surface.geometry(), pane_ids, window, cx);
            });
            let snapshot = self.native_terminal_snapshot(window, &id, surface, colors, cx);
            let title = windows
                .iter()
                .find(|candidate| candidate.id == id)
                .map_or_else(Default::default, |candidate| candidate.title.clone());
            panel.update(cx, |panel, cx| panel.publish(title, snapshot, cx));
        }
    }

    fn sync_dock_task_center(
        &self,
        dock: &Entity<crate::gpui_dock::WorkspaceDock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope = self.state.mux_scope();
        let binding = &self.state.workspace.active.binding;
        let binding_id = scope.persistence_value().to_string();
        let task = binding
            .mux()
            .selected_session()
            .and_then(|session| self.state.workspace.session_identity(scope, session))
            .or_else(|| binding.saved_selected_session_identity().map(str::to_owned))
            .unwrap_or_default();
        let selected = binding.current_window_id();
        let key = crate::workspace_composition::surface_center_key(
            &binding_id,
            &task,
            "window",
            &binding.saved_window_key(&task, selected.window_id()),
        );
        dock.update(cx, |dock, cx| dock.sync_task_center(key, window, cx));
    }

    fn prepare_dock_terminals(
        &mut self,
        dock: &Entity<crate::gpui_dock::WorkspaceDock>,
        window: &mut Window,
        colors: Colors,
        cx: &mut Context<Self>,
    ) {
        let binding = &self.state.workspace.active.binding;
        let windows = binding
            .mux()
            .sessions()
            .iter()
            .flat_map(|session| {
                session
                    .windows
                    .iter()
                    .map(|window| crate::gpui_dock::TerminalWindowPresentation {
                        id: binding.window_id(session.id.clone(), window.id.clone()),
                        title: window.name.clone(),
                    })
            })
            .collect::<Vec<_>>();
        let selected = dock
            .read(cx)
            .terminal_center_location()
            .and_then(|(task, key)| {
                windows
                    .iter()
                    .find(|candidate| {
                        self.state
                            .workspace
                            .session_identity(binding.scope(), candidate.id.session_id())
                            .as_ref()
                            == Some(&task)
                            && binding.saved_window_key(&task, candidate.id.window_id()) == key
                    })
                    .map(|candidate| candidate.id.clone())
            })
            .unwrap_or_else(|| binding.current_window_id());
        let origins = dock.read(cx).terminal_surface_origins(cx);
        for origin in origins {
            self.restore_terminal_surface(&origin, window, cx);
        }
        if self.state.mux().has_session_snapshot() {
            dock.update(cx, |dock, cx| {
                dock.reconcile_terminal_surfaces(binding, window, cx);
                dock.sync_terminals(&windows, &selected, window, cx);
            });
        }
        self.visible_terminals.clear();
        self.retain_live_terminal_panes();
        // Dock geometry belongs to the terminal surface; mux owns the split ratios.
        let panels = dock.read(cx).visible_terminal_surface_panels(cx);
        for panel in panels {
            self.prepare_dock_panel(&panel, &windows, window, colors, cx);
        }
    }

    fn move_terminal_pointer(
        &mut self,
        event: &gpui_kit::MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_pointer_cursor(event.position, event.modifiers, cx);
        if let Some((start, invocation)) = &mut self.pending_link_click {
            let dx = f32::from(event.position.x) - f32::from(start.x);
            let dy = f32::from(event.position.y) - f32::from(start.y);
            if f32::mul_add(dy, dy, dx * dx) > 16. {
                *invocation = None;
            }
            cx.notify();
            return;
        }
        self.record_mouse_input_target_at(event.position, cx);
        self.input.mouse_move(event);
        cx.notify();
    }

    fn scroll_terminal_pointer(
        &mut self,
        event: &gpui_kit::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.modal_dialog().is_some() || window.has_active_dialog(cx) {
            return;
        }
        let delta = event.delta.pixel_delta(px(self.terminal_cell.height));
        if !f32::from(delta.x).is_finite() || !f32::from(delta.y).is_finite() {
            return;
        }
        let zoom_modifier = if cfg!(target_os = "macos") {
            event.modifiers.platform
        } else {
            event.modifiers.control
        };
        if zoom_modifier && !event.modifiers.alt {
            let factor = (f32::from(delta.y) * 0.01).exp();
            let focal = bootty_terminal::geometry::SurfacePoint {
                x: event.position.x.into(),
                y: event.position.y.into(),
            };
            self.transform_terminal_at(
                event.position,
                |view, surface| view.pinched(factor, focal, surface),
                cx,
            );
            cx.stop_propagation();
            return;
        }
        if !event.modifiers.alt
            && !event.modifiers.control
            && !event.modifiers.platform
            && self
                .interaction_at(event.position, cx)
                .is_some_and(|hit| hit.interaction.view_transform().is_zoomed())
        {
            self.transform_terminal_at(
                event.position,
                |view, surface| view.panned(delta.x.into(), delta.y.into(), surface),
                cx,
            );
            cx.stop_propagation();
            return;
        }
        // Wheel and hover are semantic pointer targets, not focus changes. Keep their
        // presented pane geometry while letting keyboard focus remain where the user put
        // it (or where the preceding press selected it).
        self.record_mouse_input_target_at(event.position, cx);
        self.input.scroll(event);
        cx.notify();
    }

    fn pinch_terminal_pointer(
        &mut self,
        event: &gpui_kit::PinchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.modal_dialog().is_some()
            || window.has_active_dialog(cx)
            || !event.delta.is_finite()
        {
            return;
        }
        let focal = bootty_terminal::geometry::SurfacePoint {
            x: event.position.x.into(),
            y: event.position.y.into(),
        };
        self.transform_terminal_at(
            event.position,
            |view, surface| view.pinched((1.0 + event.delta).max(0.01), focal, surface),
            cx,
        );
        cx.stop_propagation();
    }

    fn drop_terminal_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = window.mouse_position();
        if self.state.modal_dialog().is_some()
            || window.has_active_dialog(cx)
            || self.interaction_at(position, cx).is_none()
        {
            return;
        }
        self.focus_pointer_target_at(position, window, cx);
        self.input.file_drop(paths, position);
        cx.notify();
    }

    fn begin_terminal_mouse_input(
        &mut self,
        event: &gpui_kit::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pointer_target_at(event.position, window, cx);
        if event.button == MouseButton::Left && self.begin_link_click(event, cx) {
            cx.stop_propagation();
            return;
        }
        self.terminal_mouse_buttons.insert(event.button);
        self.record_mouse_input_target_at(event.position, cx);
        self.input.mouse_down(event);
        cx.notify();
    }

    fn terminal_surface(
        &mut self,
        window: &mut Window,
        terminal_area: SurfaceRect,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let terminal = self.terminal_element(window, terminal_area, colors, cx);
        let content = self.terminal_panel_content(terminal, cx);
        div()
            .absolute()
            .left(px(terminal_area.min_x))
            .top(px(terminal_area.min_y))
            .w(px(terminal_area.width()))
            .h(px(terminal_area.height()))
            .child(content)
            .into_any_element()
    }

    pub(crate) fn terminal_panel_content(
        &self,
        terminal: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        if let Some(surface) = self.new_session_surface(cx) {
            return if self.tools.is_some() {
                // The dock owns the full creation surface across center and tools.
                div().size_full().into_any_element()
            } else {
                surface
            };
        }
        let mut surface = div()
            .id("terminal-surface")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(crate::gpui_background::layer(
                &self.state.config().window,
                &self.state.config().config_path,
            ))
            .cursor(self.cursor);
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            surface = surface
                .on_mouse_down(button, cx.listener(Self::begin_terminal_mouse_input))
                .on_mouse_up(
                    button,
                    cx.listener(|this, event, _, cx| this.finish_terminal_mouse_input(event, cx)),
                )
                .on_mouse_up_out(
                    button,
                    cx.listener(|this, event, _, cx| this.finish_terminal_mouse_input(event, cx)),
                );
        }
        surface
            .on_mouse_move(cx.listener(Self::move_terminal_pointer))
            .on_scroll_wheel(cx.listener(Self::scroll_terminal_pointer))
            .on_pinch(cx.listener(Self::pinch_terminal_pointer))
            .on_drop(cx.listener(Self::drop_terminal_files))
            .when_some(terminal, ParentElement::child)
            .into_any_element()
    }

    pub(crate) fn new_session_surface(&self, cx: &App) -> Option<AnyElement> {
        if self.surface_agent_form_presented() {
            return None;
        }
        self.dialogs.new_session_surface().map(|view| {
            div()
                .id("new-session-surface")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .overflow_y_scroll()
                .p_6()
                .bg(cx.theme().background)
                .child(div().w_full().max_w(gpui_kit::rems(44.0)).child(view))
                .into_any_element()
        })
    }

    fn fit_terminal_surface(&mut self, area: SurfaceRect, window: &Window) -> TerminalSurface {
        let padding = terminal_content_padding(window);
        let geometry = TerminalTextGeometry::fitted(
            &self.terminal_text_contract.config,
            area.width(),
            area.height(),
            self.terminal_base_cell,
            padding,
        );
        self.terminal_cell = geometry.grid_cell;
        TerminalSurface::new(area, geometry.grid_cell, padding)
    }

    fn terminal_element(
        &mut self,
        window: &mut Window,
        area: SurfaceRect,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let surface = self.fit_terminal_surface(area, window);
        if self.state.uses_native_terminal_layout() {
            return Some(self.native_terminal_element(window, surface, colors, cx));
        }

        // Native panes own their focus handles and painted facts. Drop them before
        // returning to the attached presentation so a backend switch cannot route the
        // next pointer event through a removed pane or leave keyboard focus on its child view.
        let terminal_focus = self.terminal.view.focus_handle(cx);
        let native_pointer_state_was_active =
            self.focus != terminal_focus || !self.terminal_panes.is_empty();
        if native_pointer_state_was_active {
            self.terminal_mouse_buttons.clear();
            self.pending_link_click = None;
        }
        if self.focus != terminal_focus {
            self.focus = terminal_focus.clone();
            schedule_focus(terminal_focus, window, cx);
        }
        self.terminal_panes.clear();
        self.visible_terminals.clear();
        self.present_terminal(&self.terminal.view.clone(), None, surface, window, cx)
            .map(IntoElement::into_any_element)
    }

    fn present_terminal(
        &mut self,
        view: &Entity<GpuiTerminalView>,
        pane_id: Option<&str>,
        surface: TerminalSurface,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<CachedTerminalView> {
        let terminal = self.prepare_terminal(view, pane_id, surface, window, cx);
        self.visible_terminals.push(VisibleTerminal {
            pane_id: pane_id.map(str::to_owned),
            view: terminal.as_ref().map(|terminal| terminal.0.clone()),
        });
        terminal
    }

    fn prepare_terminal(
        &mut self,
        view: &Entity<GpuiTerminalView>,
        pane_id: Option<&str>,
        surface: TerminalSurface,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<CachedTerminalView> {
        let transition_key = pane_id.map_or_else(
            || self.state.terminal_transition_key(),
            |pane| Some(self.state.pane_widget_key(pane)),
        );
        let focused = pane_id.is_none_or(|pane| self.state.focused_pane().as_deref() == Some(pane));
        let runtime = self
            .state
            .workspace
            .active
            .binding
            .visible_terminal_frame_source(pane_id)?;
        let frame = (|| {
            runtime.set_display_scale(window.scale_factor())?;
            runtime.set_render_cell_metrics(surface.cell)?;
            runtime.resize(surface.geometry())?;
            runtime.extract_frame()
        })();
        let frame = match frame {
            Ok(frame) => frame,
            Err(error) => {
                self.state.record_render_error(error);
                return None;
            }
        };
        let config = self.state.config();
        view.update(cx, |view, cx| {
            view.set_window_focused(window.is_window_active(), cx);
            view.set_scrollbar_mode(config.session.scrollbar, cx);
            view.set_option_as_alt(bootty_mux::terminal_config::terminal_macos_option_as_alt(
                config.input.macos_option_as_alt,
            ));
            view.set_background_opacity(config.window.background_opacity, cx);
        });
        GpuiTerminalView::publish(
            view,
            TerminalPresentation {
                transition_key,
                surface,
                frame,
                text_cell_height: self.terminal_base_cell.height,
                pixels_per_point: window.scale_factor(),
                text_contract: Arc::clone(&self.terminal_text_contract),
                animate_cursor: focused,
                dim_inactive_cursor: config.cursor.dim_inactive_pane,
            },
            cx,
        );
        if focused {
            let focus = view.focus_handle(cx);
            if self.focus != focus {
                self.focus = focus.clone();
                schedule_focus(focus, window, cx);
            }
            self.state.record_surface(surface);
        }
        Some(CachedTerminalView(view.clone()))
    }

    fn native_terminal_element(
        &mut self,
        window: &mut Window,
        surface: TerminalSurface,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.visible_terminals.clear();
        let window_id = self.state.workspace.active.binding.current_window_id();
        self.retain_live_terminal_panes();
        let snapshot = self.native_terminal_snapshot(window, &window_id, surface, colors, cx);
        let weak = cx.entity().downgrade();
        GpuiPaneWorkspace::new(snapshot, move |intent, window, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.apply_pane_intent(intent, window, cx);
                cx.notify();
            });
        })
        .into_any_element()
    }

    fn native_terminal_snapshot(
        &mut self,
        window: &mut Window,
        window_id: &bootty_mux::workspace::ScopedWindowId,
        surface: TerminalSurface,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> GpuiPaneWorkspaceSnapshot<crate::gpui_terminal_panel::WorkspacePaneView> {
        let area = surface.rect;
        let config = self.state.config();
        let gap = config.chrome.pane_divider_width;
        let focused = self.state.focused_pane();
        let layout = self
            .state
            .workspace
            .active
            .binding
            .window_pane_layout(window_id.session_id(), window_id.window_id())
            .cloned();
        let rects = layout
            .as_ref()
            .map_or_else(Default::default, |layout| layout.rects(area, gap));
        if *window_id == self.state.workspace.active.binding.current_window_id() {
            self.state.record_pane_area(area);
        }

        let pane_surfaces = rects
            .iter()
            .map(|(pane_id, rect)| {
                (
                    pane_id.clone(),
                    *rect,
                    TerminalSurface::new(*rect, surface.cell, surface.padding),
                )
            })
            .collect::<Vec<_>>();
        self.resize_native_pane_window(window_id, layout.as_ref(), &pane_surfaces);

        let mut panes = Vec::with_capacity(pane_surfaces.len());
        for (pane_id, rect, surface) in pane_surfaces {
            let is_focused = focused.as_deref() == Some(pane_id.as_str());
            if let Some(view) = self.native_mux_pane_content(&pane_id, window, cx) {
                // Native content still occupies a real backend pane. Keep its geometry in
                // the same resize path as terminal content so the backend retains the split.
                if let Some(runtime) = self
                    .state
                    .workspace
                    .active
                    .binding
                    .visible_terminal_frame_source(Some(&pane_id))
                    && let Err(error) = runtime.resize(surface.geometry())
                {
                    self.state.record_render_error(error);
                }
                panes.push(GpuiPaneSnapshot {
                    id: pane_id,
                    rect: pane_rect(rect),
                    terminal: view,
                    focused: is_focused,
                    progress: None,
                });
                continue;
            }
            let key = self.state.pane_widget_key(&pane_id);
            let terminal_view = self.ensure_terminal_pane(&pane_id, &key, window, cx);
            if let Some(terminal) =
                self.present_terminal(&terminal_view, Some(&pane_id), surface, window, cx)
            {
                panes.push(GpuiPaneSnapshot {
                    id: pane_id.clone(),
                    rect: pane_rect(rect),
                    terminal: crate::gpui_terminal_panel::WorkspacePaneView::Terminal(terminal),
                    focused: is_focused,
                    progress: self.state.pane_progress(&pane_id).map(pane_progress),
                });
            }
        }

        let dividers = self.pane_dividers(layout.as_ref(), area, gap);
        self.finish_pane_snapshot(area, panes, dividers, colors, window, cx)
    }

    fn pane_dividers(
        &mut self,
        layout: Option<&bootty_mux::pane_layout::PaneLayout>,
        area: SurfaceRect,
        gap: f32,
    ) -> Vec<GpuiPaneDividerSnapshot> {
        let dividers = layout.map_or_else(Default::default, |layout| layout.dividers(area, gap));
        dividers
            .into_iter()
            .map(|divider| {
                let handle = expanded_divider_hit_rect(divider.rect, divider.direction);
                self.state.register_chrome_handle(handle);
                GpuiPaneDividerSnapshot {
                    path: divider.path,
                    direction: pane_split_direction(divider.direction),
                    rect: pane_rect(divider.rect),
                    area: pane_rect(divider.area),
                }
            })
            .collect()
    }

    fn ensure_terminal_pane(
        &mut self,
        pane_id: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<GpuiTerminalView> {
        if let Some(pane) = self.terminal_panes.get(key) {
            return pane.view.clone();
        }
        let pane = Self::create_terminal_view(Some(pane_id.to_owned()), window, cx);
        let view = pane.view.clone();
        self.terminal_panes.insert(key.to_owned(), pane);
        view
    }

    fn resize_native_pane_window(
        &mut self,
        window_id: &bootty_mux::workspace::ScopedWindowId,
        layout: Option<&bootty_mux::pane_layout::PaneLayout>,
        pane_surfaces: &[(String, SurfaceRect, TerminalSurface)],
    ) {
        if let Some((cols, rows)) = layout.and_then(|layout| {
            layout.terminal_window_size(|pane_id| {
                pane_surfaces
                    .iter()
                    .find(|(candidate, _, _)| candidate == pane_id)
                    .map(|(_, _, surface)| {
                        let geometry = surface.geometry();
                        (geometry.cols, geometry.rows)
                    })
            })
        }) {
            let result = if *window_id == self.state.workspace.active.binding.current_window_id() {
                self.state.resize_native_layout_window(cols, rows)
            } else {
                self.state
                    .workspace
                    .active
                    .binding
                    .terminal_mut()
                    .resize_visible_window(Some(window_id.window_id()), cols, rows)
            };
            if let Err(error) = result {
                self.state.record_render_error(error);
            }
        }
    }

    fn finish_pane_snapshot(
        &mut self,
        area: SurfaceRect,
        panes: Vec<GpuiPaneSnapshot<crate::gpui_terminal_panel::WorkspacePaneView>>,
        dividers: Vec<GpuiPaneDividerSnapshot>,
        colors: Colors,
        window: &Window,
        cx: &Context<Self>,
    ) -> GpuiPaneWorkspaceSnapshot<crate::gpui_terminal_panel::WorkspacePaneView> {
        let config = self.state.config();
        let gap = config.chrome.pane_divider_width;
        let corner_radius = config.chrome.pane_corner_radius;
        let focus_border_width = config.chrome.pane_focus_border_width;
        let focus_border_color = config.chrome.pane_focus_border_color;
        let divider_color = config.chrome.pane_divider_color;
        let inactive_dim = config.chrome.unfocused_terminal_dim.clamp(0.0, 1.0);
        let window_dim = if self.state.terminal_focused() {
            0.0
        } else {
            inactive_dim
        };
        let palette = self.state.ui_theme().palette;
        let background_opacity = config.window.background_opacity;
        let empty_message = panes.is_empty().then(|| "No terminal pane".to_owned());
        let has_visible_indeterminate_progress = panes.iter().any(|pane| {
            pane.progress
                .is_some_and(|progress| progress.state == PaneProgressState::Indeterminate)
        });
        let arrangement_target = self
            .state
            .supports_pane_operation(bootty_mux::capability::BindingOperation::MovePane)
            .then(|| {
                self.state
                    .current_command_target_for("pane.move", bootty_control::ResourceKind::Session)
            })
            .flatten();
        let snapshot = GpuiPaneWorkspaceSnapshot {
            arrangement_target,
            area: pane_rect(area),
            panes,
            dividers,
            gap,
            corner_radius,
            focus_border_width,
            inactive_dim,
            window_dim,
            animation_seconds: self.settings_started.elapsed().as_secs_f64(),
            colors: GpuiPaneColors {
                // Terminal frames and the pane host are one visual surface. Using a chrome panel
                // color here exposed gutters and split margins whenever the terminal theme was
                // not identical to the UI theme.
                background: if background_opacity < 1.0 {
                    gpui_kit::Hsla::transparent_black()
                } else {
                    colors.base
                },
                divider: divider_color
                    .map(crate::theme::config_rgba)
                    .map_or(colors.border_variant, gpui_color),
                divider_hover: colors.accent,
                focus_border: focus_border_color
                    .map(crate::theme::config_rgba)
                    .map_or(colors.accent, gpui_color),
                progress_track: colors.border,
                progress_normal: colors.accent,
                progress_error: colors.destructive,
                progress_warning: gpui_color(palette.warning),
                empty_text: colors.muted,
            },
            empty_message,
        };
        if has_visible_indeterminate_progress && window.is_window_active() {
            self.schedule_repaint_after(
                bootty_terminal::scheduler::CURSOR_BLINK_REFRESH_INTERVAL,
                cx,
            );
        }
        snapshot
    }

    pub(crate) fn close_terminal_window(
        &mut self,
        target: &bootty_control::CommandTarget,
        id: &bootty_mux::workspace::ScopedWindowId,
        cx: &mut Context<Self>,
    ) {
        if !self.terminal_window_matches_binding(target, id) {
            return;
        }
        self.state.apply_exact_mux_action(
            crate::state::ExactMuxAction::CloseWindowPane,
            crate::commands::ExactMuxTarget::window(
                self.state.mux_scope(),
                id.session_id(),
                id.window_id(),
            ),
        );
        cx.notify();
    }

    pub(crate) fn focus_terminal_window(
        &mut self,
        target: &bootty_control::CommandTarget,
        id: &bootty_mux::workspace::ScopedWindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.terminal_window_matches_binding(target, id) {
            return;
        }
        self.close_native_conversation();
        if *id != self.state.workspace.active.binding.current_window_id() {
            self.state.apply_exact_mux_action(
                crate::state::ExactMuxAction::Activate,
                crate::commands::ExactMuxTarget::window(
                    self.state.mux_scope(),
                    id.session_id(),
                    id.window_id(),
                ),
            );
        }
        if self.sync_selected_native_pane(window, cx) {
            cx.notify();
            return;
        }
        if let Some(pane) = self.state.focused_pane() {
            let key = self.state.pane_widget_key(&pane);
            if let Some(pane) = self.terminal_panes.get(&key) {
                self.focus = pane.view.focus_handle(cx);
                self.focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn apply_terminal_window_intent(
        &mut self,
        target: &bootty_control::CommandTarget,
        id: &bootty_mux::workspace::ScopedWindowId,
        intent: GpuiPaneIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.terminal_window_matches_binding(target, id) {
            return;
        }
        match intent {
            GpuiPaneIntent::Resize {
                path,
                ratio,
                min_fraction,
            } => {
                self.state.workspace.active.binding.set_window_pane_ratio(
                    id,
                    &path,
                    ratio,
                    min_fraction,
                );
            }
            GpuiPaneIntent::Focus(pane) => {
                if let Some(record) = self.state.native_agent_service().and_then(|service| {
                    service.sessions().into_iter().find(|record| {
                        self.state
                            .native_panel_target(record)
                            .is_some_and(|(exact, _)| {
                                exact.scope() == self.state.mux_scope()
                                    && exact.ids().2 == Some(pane.as_str())
                            })
                    })
                }) {
                    if self
                        .selected_mux_native_record()
                        .as_ref()
                        .map(bootty_agents::NativeSessionRecord::target)
                        != Some(record.target())
                    {
                        let mut invocation =
                            CommandInvocation::from_action("agents.native.focus", Caller::Internal);
                        invocation.target = Some(record.target());
                        self.invoke_gpui_command(invocation, window, cx);
                    }
                    return;
                }
                self.focus_terminal_window(target, id, window, cx);
                self.state.focus_pane(&pane);
                self.sync_selected_native_pane(window, cx);
                let key = self.state.pane_widget_key(&pane);
                if let Some(pane) = self.terminal_panes.get(&key) {
                    self.focus = pane.view.focus_handle(cx);
                    self.focus.focus(window, cx);
                }
            }
            GpuiPaneIntent::Command(invocation) => self.invoke_gpui_command(invocation, window, cx),
        }
        cx.notify();
    }

    fn terminal_window_matches_binding(
        &self,
        target: &bootty_control::CommandTarget,
        id: &bootty_mux::workspace::ScopedWindowId,
    ) -> bool {
        self.state
            .current_command_target_for("git.open", bootty_control::ResourceKind::Binding)
            .is_some_and(|current| current == *target)
            && self
                .state
                .workspace
                .active
                .binding
                .window_id(id.session_id().to_owned(), id.window_id().to_owned())
                == *id
            && self.state.mux().sessions().iter().any(|session| {
                session.id == id.session_id()
                    && session
                        .windows
                        .iter()
                        .any(|window| window.id == id.window_id())
            })
    }

    fn apply_pane_intent(
        &mut self,
        intent: GpuiPaneIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            GpuiPaneIntent::Command(invocation) => self.invoke_gpui_command(invocation, window, cx),
            GpuiPaneIntent::Focus(pane_id) => self.state.focus_pane(&pane_id),
            GpuiPaneIntent::Resize {
                path,
                ratio,
                min_fraction,
            } => self.state.set_pane_ratio(&path, ratio, min_fraction),
        }
    }

    fn interaction_at(&self, position: gpui_kit::Point<Pixels>, cx: &App) -> Option<TerminalHit> {
        let point = bootty_terminal::geometry::SurfacePoint {
            x: position.x.into(),
            y: position.y.into(),
        };
        self.visible_terminals.iter().find_map(|terminal| {
            let view = terminal.view.as_ref()?;
            let interaction = view.read(cx).interaction()?;
            interaction
                .surface()
                .rect
                .contains(point)
                .then(|| TerminalHit {
                    pane_id: terminal.pane_id.clone(),
                    view: view.clone(),
                    interaction,
                })
        })
    }

    fn record_mouse_input_target_at(&mut self, position: gpui_kit::Point<Pixels>, cx: &App) {
        if let Some(hit) = self.interaction_at(position, cx) {
            self.state.record_mouse_input_target_for_pane(
                hit.pane_id,
                hit.interaction.surface(),
                hit.interaction.view_transform(),
                Some(crate::gpui::Point {
                    x: position.x.into(),
                    y: position.y.into(),
                }),
            );
        } else {
            self.state.record_mouse_input_target_none();
        }
    }

    fn transform_terminal_at(
        &self,
        position: gpui_kit::Point<Pixels>,
        transform: impl FnOnce(
            bootty_terminal::geometry::ViewTransform,
            SurfaceRect,
        ) -> bootty_terminal::geometry::ViewTransform,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(hit) = self.interaction_at(position, cx) else {
            return false;
        };
        let view = transform(
            hit.interaction.view_transform(),
            hit.interaction.surface().rect,
        );
        hit.view
            .update(cx, |terminal, cx| terminal.set_view_transform(view, cx));
        cx.notify();
        true
    }

    fn focus_pointer_target_at(
        &mut self,
        position: gpui_kit::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hit) = self.interaction_at(position, cx) else {
            return;
        };
        if let Some(pane) = hit.pane_id {
            self.state.focus_pane(&pane);
        }
        self.focus = hit.view.focus_handle(cx);
        window.focus(&self.focus, cx);
    }

    fn finish_terminal_mouse_input(
        &mut self,
        event: &gpui_kit::MouseUpEvent,
        cx: &mut Context<Self>,
    ) {
        if event.button == MouseButton::Left
            && let Some((start, invocation)) = self.pending_link_click.take()
        {
            let dx = f32::from(event.position.x) - f32::from(start.x);
            let dy = f32::from(event.position.y) - f32::from(start.y);
            if f32::mul_add(dy, dy, dx * dx) <= 16.
                && let Some(invocation) = invocation
            {
                self.state.dispatch_command(
                    invocation,
                    ViewportSnapshot::default(),
                    &mut self.pending_effects,
                );
            }
            cx.notify();
            return;
        }
        if !self.terminal_mouse_buttons.remove(&event.button) {
            return;
        }
        self.record_mouse_input_target_at(event.position, cx);
        self.input.mouse_up(event);
        cx.notify();
    }

    fn begin_link_click(&mut self, event: &gpui_kit::MouseDownEvent, cx: &App) -> bool {
        let activation = if cfg!(target_os = "macos") {
            event.modifiers.platform
        } else {
            event.modifiers.control
        };
        if !activation {
            return false;
        }
        let point = bootty_terminal::geometry::SurfacePoint {
            x: event.position.x.into(),
            y: event.position.y.into(),
        };
        let Some(hit) = self.interaction_at(event.position, cx) else {
            return false;
        };
        let Some(link) = hit.interaction.hyperlink_at(point) else {
            return false;
        };
        if let bootty_terminal::terminal_links::LinkTarget::File { path, .. } = &link.target {
            let absolute = path.starts_with(['/', '~']) || path.as_bytes().get(1) == Some(&b':');
            let binding = &self.state.workspace.active.binding;
            if !absolute
                && !binding.uses_native_terminal_layout()
                && binding.mux().selected_window_panes().len() > 1
            {
                self.state.record_error("Use an absolute file link in a multipane tmux attachment; the clicked pane's directory is not available.");
                self.pending_link_click = Some((event.position, None));
                return true;
            }
        }
        let Some(target) = self
            .state
            .current_command_target_for("link.open", bootty_control::ResourceKind::Terminal)
        else {
            return false;
        };
        let mut invocation =
            CommandInvocation::new("link.open", vec![link.url], Caller::Keybinding);
        invocation.target = Some(target);
        self.pending_link_click = Some((event.position, Some(invocation)));
        true
    }

    fn update_pointer_cursor(
        &mut self,
        position: gpui_kit::Point<gpui_kit::Pixels>,
        modifiers: gpui_kit::Modifiers,
        cx: &App,
    ) {
        let point = bootty_terminal::geometry::SurfacePoint {
            x: position.x.into(),
            y: position.y.into(),
        };
        self.cursor = self
            .interaction_at(position, cx)
            .and_then(|hit| {
                let activation_modifier = if cfg!(target_os = "macos") {
                    modifiers.platform
                } else {
                    modifiers.control
                };
                (activation_modifier && hit.interaction.hyperlink_at(point).is_some())
                    .then_some(CursorStyle::PointingHand)
            })
            .unwrap_or(self.terminal_cursor);
    }
}

fn pane_rect(rect: SurfaceRect) -> PaneRect {
    PaneRect::new(rect.min_x, rect.min_y, rect.width(), rect.height())
}

fn terminal_content_padding(window: &Window) -> TerminalPadding {
    // Keep terminal ink clear of pane edges without changing configured cell fitting.
    TerminalPadding::uniform(f32::from(window.rem_size()) * 0.25)
}

pub fn surface_bounds(rect: SurfaceRect) -> Bounds<Pixels> {
    Bounds::new(
        point(px(rect.min_x), px(rect.min_y)),
        size(px(rect.width()), px(rect.height())),
    )
}

const fn pane_split_direction(direction: SplitDirection) -> PaneSplitDirection {
    match direction {
        SplitDirection::Right => PaneSplitDirection::Right,
        SplitDirection::Down => PaneSplitDirection::Down,
    }
}

const fn pane_progress(progress: bootty_mux::workspace::TerminalProgress) -> PaneProgress {
    let state = match progress.state {
        bootty_mux::workspace::TerminalProgressState::Normal => PaneProgressState::Normal,
        bootty_mux::workspace::TerminalProgressState::Error => PaneProgressState::Error,
        bootty_mux::workspace::TerminalProgressState::Indeterminate => {
            PaneProgressState::Indeterminate
        }
        bootty_mux::workspace::TerminalProgressState::Warning => PaneProgressState::Warning,
    };
    PaneProgress {
        state,
        value: progress.value,
    }
}

fn expanded_divider_hit_rect(rect: SurfaceRect, direction: SplitDirection) -> SurfaceRect {
    const MIN_GRAB: f32 = 8.0;
    match direction {
        SplitDirection::Right => {
            let width = rect.width().max(MIN_GRAB);
            let inset = (width - rect.width()) / 2.0;
            SurfaceRect {
                min_x: rect.min_x - inset,
                max_x: rect.max_x + inset,
                ..rect
            }
        }
        SplitDirection::Down => {
            let height = rect.height().max(MIN_GRAB);
            let inset = (height - rect.height()) / 2.0;
            SurfaceRect {
                min_y: rect.min_y - inset,
                max_y: rect.max_y + inset,
                ..rect
            }
        }
    }
}

impl GpuiWorkspace {
    fn prepare_frame_metrics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_window_active() {
            crate::window::macos_sync_fullscreen_presentation(window.is_simple_fullscreen().then(
                || {
                    self.state
                        .config()
                        .window
                        .hides_macos_menu_bar_in_non_native_fullscreen()
                },
            ));
        }
        #[expect(
            clippy::float_cmp,
            reason = "An exact platform display-scale change invalidates cached cell metrics"
        )]
        let display_scale_changed = self.terminal_display_scale != window.scale_factor();
        if display_scale_changed {
            self.terminal_display_scale = window.scale_factor();
            self.terminal_base_cell =
                terminal_cell_metrics(&self.terminal_text_contract.config, window);
            self.terminal_cell = self.terminal_base_cell;
        }

        self.sync_agents(cx);
    }

    fn prepare_frame_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // `new` runs before this root's `track_focus` node exists. Focus only after the first
        // render has attached that node; an earlier native focus request leaves focus on NSWindow
        // and GPUI never dispatches terminal or global key events.
        if !self.focus_initialized {
            self.focus_initialized = true;
            schedule_focus(self.focus.clone(), window, cx);
        }
        // Child-only paints (for example cursor blink) do not advance application work. Queued
        // pointer input does: its handlers only notify, so otherwise wheel and motion input would
        // wait for the next maintenance tick, up to a full idle repaint backoff.
        if std::mem::take(&mut self.frame_update_pending)
            || !self.pending_effects.is_empty()
            || self.state.commands.has_queued()
            || self.input.has_queued()
        {
            self.advance_frame(window, cx);
        }
        self.sync_key_bindings(window, cx);
        let active_appearance_variant = self
            .state
            .config()
            .appearance
            .mode
            .variant(crate::theme::appearance_variant(window.appearance()));
        if self.last_locale != self.state.localizer.locale() {
            self.last_locale = self.state.localizer.locale().to_owned();
            crate::i18n::publish(&self.state.localizer, cx);
        }
        self.sync_zed_theme(active_appearance_variant, cx);
        let material = crate::gpui_background::material(&self.state.config().window);
        if self.last_background_material != Some(material) {
            window.set_background_appearance(material);
            self.last_background_material = Some(material);
        }
    }

    fn decorate_dock_chrome(&self, chrome_snapshot: &mut ChromeSnapshot, cx: &Context<Self>) {
        self.decorate_native_tabs(chrome_snapshot, cx);
        if self.tools.is_some() {
            let config = self.state.config();
            chrome_snapshot.layout.top_inset = self.state.window_chrome_facts().top_inset(
                config.window.fullscreen_tabs_in_notch,
                config
                    .chrome
                    .status_height
                    // Keep the tab strip's bottom border below the camera exclusion band.
                    .max(crate::gpui::UI_TAB_BAR_HEIGHT)
                    - 1.0,
                config.window.fullscreen_top_offset,
            );
        }
        if let Some(tools) = &self.tools {
            for bar in [
                &mut chrome_snapshot.top_status,
                &mut chrome_snapshot.bottom_status,
            ]
            .into_iter()
            .flatten()
            {
                for item in bar
                    .segments
                    .iter_mut()
                    .flat_map(|segment| &mut segment.items)
                {
                    if let Some(crate::gpui::chrome::NativeChromeAction::TogglePanel(kind)) =
                        item.action
                    {
                        item.active = tools.read(cx).panel_visible(kind, cx);
                    }
                }
            }
        }
    }

    fn prepare_chrome_frame(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ChromeSnapshot {
        let viewport = self.workspace_bounds.size;
        let viewport_height: f32 = viewport.height.into();
        let usage_visible = self.state.config().chrome.sidebar
            && self
                .state
                .config()
                .sidebar
                .modules
                .iter()
                .any(|module| module == "codexbar");
        self.launch
            .native_chrome
            .borrow_mut()
            .refresh(Instant::now(), usage_visible);
        let projection = chrome_frame::prepare(
            &self.state,
            &mut self.launch.native_chrome.borrow_mut(),
            true,
            window.is_window_active(),
        );
        let viewport_width: f32 = viewport.width.into();
        let dock_matches_binding = self.tools_match_binding(cx);
        if !dock_matches_binding {
            cx.defer_in(window, Self::ensure_tools);
        }
        // Context changes retarget the existing dock; unmounting it briefly resizes
        // the attached terminal through the legacy tools-overlay layout.
        let docked_terminals = self.tools.is_some();
        let mut chrome_snapshot = chrome_frame::snapshot(
            &self.state,
            &self.launch.native_chrome.borrow(),
            &projection,
            viewport_width,
            viewport_height,
        );
        self.last_maintenance_chrome = Some(chrome_snapshot.clone());
        self.decorate_dock_chrome(&mut chrome_snapshot, cx);
        self.sync_key_bindings(window, cx);
        self.chrome_view.update(cx, |chrome, cx| {
            chrome.set_docked_status(docked_terminals, cx);
        });
        self.chrome_view.update(cx, |chrome, cx| {
            chrome.update(&chrome_snapshot, window, cx);
        });
        chrome_snapshot
    }

    fn sync_frame_overlays(&mut self, colors: Colors, window: &mut Window, cx: &mut Context<Self>) {
        let error = self.state.last_error();
        self.sync_error_notification(error, window, cx);
        let projection = self.state.dialog_projection().or_else(|| {
            self.state
                .terminal_find_projection()
                .map(Box::new)
                .map(crate::presentation::dialogs::DialogProjection::Dialog)
        });
        cx.defer_in(window, move |this, window, cx| {
            this.sync_dialog_overlay(projection, colors, window, cx);
        });
    }

    fn capture_terminal_key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_mux_pane_is_native()
            || window.has_active_dialog(cx)
            || self.state.modal_dialog().is_some()
        {
            return;
        }
        if self
            .tools
            .as_ref()
            .is_some_and(|tools| tools.focus_handle(cx).contains_focused(window, cx))
            && !self.terminal_view_focused(window, cx)
        {
            return;
        }
        if let Some(input) = crate::gpui_input::direct_key_input(event) {
            self.state.queue_direct_input(input);
            cx.notify();
        }
    }

    fn dispatch_command_action(
        &mut self,
        action: &crate::gpui_actions::InvokeCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx)
            && !action.invocation().command.starts_with("ui.command.")
            && !action.invocation().command.starts_with("ui.composer.")
        {
            cx.stop_propagation();
            return;
        }
        self.invoke_gpui_command(action.invocation().clone(), window, cx);
        // GPUI actions and Bootty's terminal resolver share this focus path. Once a
        // configured binding has dispatched its typed action, do not let the same
        // physical key fall through and dispatch a second time from terminal input.
        cx.stop_propagation();
    }

    fn window_frame(
        &self,
        workspace: impl IntoElement,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        // The Root border and Linux title bar consume space outside the workspace. Use
        // the laid-out body bounds for terminal/chrome geometry, including after resize.
        let previous_bounds = self.workspace_bounds;
        let owner = cx.weak_entity();
        let title_bar = self
            .state
            .config()
            .window
            .decorations_enabled()
            .then(|| {
                crate::platform::client_title_bar(self.state.config().window.title.clone(), window)
            })
            .flatten()
            .map(|bar| {
                bar.on_close_window(cx.listener(|this, _, window, cx| {
                    this.request_document_exit(false, window, cx);
                }))
            });
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .children(title_bar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .on_prepaint(move |bounds, window, cx| {
                        if bounds != previous_bounds {
                            window.defer(cx, move |_, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.workspace_bounds = bounds;
                                    cx.notify();
                                });
                            });
                        }
                    })
                    .child(workspace),
            )
    }

    fn workspace_frame(
        &self,
        terminal_surface: AnyElement,
        terminal_area: SurfaceRect,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let ui_font = setup_ui_font(window, cx);
        let workspace_origin = self.workspace_bounds.origin;
        div()
            .id("bootty-workspace")
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .track_focus(&self.focus)
            .key_context(self.keymap_context.as_str())
            .capture_key_down(cx.listener(Self::capture_terminal_key))
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui_kit::ModifiersChangedEvent, _, cx| {
                    this.chrome_view.update(cx, |chrome, cx| {
                        chrome.set_hint_modifiers(event.modifiers, cx);
                    });
                },
            ))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    let point = bootty_terminal::geometry::SurfacePoint {
                        x: f32::from(event.position.x) - f32::from(workspace_origin.x),
                        y: f32::from(event.position.y) - f32::from(workspace_origin.y),
                    };
                    if this.terminal_mouse_buttons.is_empty() || terminal_area.contains(point) {
                        return;
                    }
                    this.update_pointer_cursor(event.position, event.modifiers, cx);
                    this.input.mouse_move(event);
                    cx.notify();
                }),
            )
            .on_action(cx.listener(Self::dispatch_command_action))
            .on_action(cx.listener(
                |_, _: &crate::gpui_actions::CycleApplicationWindow, window, cx| {
                    crate::gpui_actions::cycle_application_window(window, cx);
                },
            ))
            .bg(if self.state.black_notch_chrome() {
                // Match the physical notch without changing the theme of interior tabs.
                gpui_kit::black()
            } else if self.state.config().window.background_opacity < 1.0 {
                gpui_kit::Hsla::transparent_black()
            } else {
                colors.mantle
            })
            .font(ui_font)
            .text_color(colors.text)
            .child(terminal_surface)
            .when(self.tools.is_none(), |workspace| {
                workspace.child(div().absolute().size_full().child(self.chrome_view.clone()))
            })
            .child(self.dialogs.overlay.clone())
    }
}

impl Render for GpuiWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_frame_metrics(window, cx);
        self.prepare_frame_state(window, cx);
        let config_revision = self.state.config_revision();
        let settings_changed = self.poll_settings_runtime(cx);
        let chrome_snapshot = self.prepare_chrome_frame(window, cx);
        let docked_terminals = self.tools.is_some();
        let colors = Colors::from_state(&self.state);
        self.sync_browser_configuration(window, cx);
        self.sync_settings_window(settings_changed, config_revision, cx);
        self.sync_frame_overlays(colors, window, cx);
        let terminal_area = terminal_area(&chrome_snapshot, docked_terminals);
        crate::window::macos_set_window_shadow(
            &window.window_title(),
            !self.state.window_chrome_facts().fullscreen,
        );
        if self.pending_window_move {
            self.pending_window_move = false;
            window.start_window_move();
        }
        if let Some(tools) = &self.tools {
            let browser_visible = (self.state.dialog_projection().is_none()
                || self.state.surface_agent_form_request_id().is_some())
                && !window.has_active_dialog(cx)
                && !window.has_active_sheet(cx)
                && !window.has_active_prompt()
                && !gpui_kit::base::GlobalState::is_in_deferred_context(cx)
                && !cx.has_active_drag();
            tools.update(cx, |tools, cx| {
                tools.set_browser_visible(browser_visible, cx);
            });
        }
        let terminal_surface = if let Some(dock) = self.tools.clone() {
            self.docked_terminal_surface(dock, window, terminal_area, colors, cx)
        } else {
            self.terminal_surface(window, terminal_area, colors, cx)
        };
        let workspace = self.workspace_frame(terminal_surface, terminal_area, colors, window, cx);

        let workspace = workspace.when(
            self.visual_bell_until
                .is_some_and(|until| until > Instant::now()),
            |workspace| {
                workspace.child(
                    div()
                        .absolute()
                        .left(px(terminal_area.min_x))
                        .top(px(terminal_area.min_y))
                        .w(px(terminal_area.width()))
                        .h(px(terminal_area.height()))
                        .border_2()
                        .border_color(colors.accent),
                )
            },
        );
        self.window_frame(workspace, window, cx)
    }
}

const fn gpui_cursor(icon: crate::state::CursorIcon) -> CursorStyle {
    use crate::state::CursorIcon;
    match icon {
        CursorIcon::Text | CursorIcon::VerticalText => CursorStyle::IBeam,
        CursorIcon::PointingHand => CursorStyle::PointingHand,
        CursorIcon::Crosshair | CursorIcon::Cell => CursorStyle::Crosshair,
        CursorIcon::Grab => CursorStyle::OpenHand,
        CursorIcon::Grabbing | CursorIcon::Move | CursorIcon::AllScroll => CursorStyle::ClosedHand,
        CursorIcon::ResizeHorizontal => CursorStyle::ResizeLeftRight,
        CursorIcon::ResizeVertical => CursorStyle::ResizeUpDown,
        CursorIcon::ResizeNeSw => CursorStyle::ResizeUpLeftDownRight,
        CursorIcon::ResizeNwSe => CursorStyle::ResizeUpRightDownLeft,
        CursorIcon::ResizeEast => CursorStyle::ResizeRight,
        CursorIcon::ResizeWest => CursorStyle::ResizeLeft,
        CursorIcon::ResizeNorth => CursorStyle::ResizeUp,
        CursorIcon::ResizeSouth => CursorStyle::ResizeDown,
        // GPUI has no invisible, busy, forbidden, help, or copy cursor variants. Arrow is its
        // faithful neutral fallback rather than guessing a different interaction state.
        _ => CursorStyle::Arrow,
    }
}

/// Browser data follows the process identity, independently of config overrides.
pub fn browser_profile_directory() -> std::path::PathBuf {
    use bootty_config::identity::ApplicationIdentity;
    let identity = ApplicationIdentity::for_process();
    #[cfg(target_os = "windows")]
    let state = {
        let local = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
        let roaming = std::env::var_os("APPDATA").map(std::path::PathBuf::from);
        bootty_config::identity::windows_daemon_state_path(
            identity,
            None,
            local.as_deref(),
            roaming.as_deref(),
        )
    };
    #[cfg(not(target_os = "windows"))]
    let state = {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let xdg = std::env::var_os("XDG_STATE_HOME").map(std::path::PathBuf::from);
        bootty_config::identity::unix_daemon_state_path(
            identity,
            None,
            xdg.as_deref(),
            home.as_deref(),
        )
    };
    state
        .unwrap_or_else(|| identity.default_config_path())
        .with_file_name("browser")
}
