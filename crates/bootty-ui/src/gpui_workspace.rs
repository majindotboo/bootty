//! GPUI presentation of the Bootty workspace.
//!
//! Projects accepted configuration, mux state, and native service facts into the workspace window.

use bootty_mux::pane_layout::SplitDirection;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::terminal_text::{NativeSymbolPolicy, TerminalTextContract, TerminalTextGeometry};
use anyhow::Result;
use bootty_config::config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandInvocation, ControlCatalog, ControlPlane,
};
use bootty_mux::provider::MuxBackendRegistry;
use bootty_terminal::frame_source::TerminalFrameSource;
use bootty_terminal::geometry::{CellMetrics, SurfaceRect, TerminalPadding, TerminalSurface};
use gpui_kit::component::{
    Disableable as _, ElementExt as _, IconName, Root, Sizable as _, Size, WindowExt as _,
    button::{Button, ButtonVariants as _},
    notification::Notification,
    tab::{Tab, TabBar},
};
use gpui_kit::{
    AnyElement, App, Bounds, Context, CursorStyle, Entity, ExternalPaths, FocusHandle, Focusable,
    Hsla, IntoElement, MouseButton, ParentElement, Pixels, Render, Styled, Subscription,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowDecorations, WindowKind,
    WindowOptions, div, point, prelude::*, px, size,
};
use num_traits::ToPrimitive as _;

use crate::gpui::{
    DialogIntent, DialogView, FileEditor, FileEditorEvent, GpuiKeymapEditor, GpuiPaneColors,
    GpuiPaneDividerSnapshot, GpuiPaneIntent, GpuiPaneSnapshot, GpuiPaneWorkspace,
    GpuiPaneWorkspaceSnapshot, GpuiSettings, GpuiSpaceEditor, GpuiTerminalInteraction,
    KeymapEditorIntent, ModuleIntegrationsSnapshot, OverlayHost, PaneProgress, PaneProgressState,
    PaneRect, PaneSplitDirection, SettingsIntent, SettingsTitleBar, SpaceEditorColors,
    SpaceEditorIntent,
    chrome::{ChromeIntent, ChromeSnapshot, GpuiChrome, SidebarPosition},
    setup_ui_font, terminal_cell_metrics,
};
use crate::gpui_keymap_editor::{
    self, editor_snapshot as keymap_editor_snapshot, persisted_edit as persisted_keymap_edit,
};
use crate::{
    chrome_frame,
    frame_facts::RendererMetrics,
    gpui_input::GpuiFrameFacts,
    gpui_settings_catalog::UnsupportedModuleDiagnostic,
    gpui_terminal_view::{
        CachedTerminalView, GpuiTerminalView, TerminalScrollbarInput, TerminalViewInput,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayKind {
    Dialog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootDialogKind {
    Dialog,
    SpaceEditor,
}

struct BoottyErrorNotification;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsWindowTab {
    Settings,
    Keymap,
    File(EditorFileKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorFileKind {
    Config,
    Keymap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SettingsWindowTarget {
    Settings,
    Setting(String),
    Keymap(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorCloseTarget {
    Tab(EditorFileKind),
    Window,
}

struct EditorFileTab {
    editor: Entity<FileEditor>,
    path: PathBuf,
    original_contents: String,
    reconcile_generation: u64,
    _subscription: Subscription,
}

/// The native settings window is intentionally a thin host around the renderer-neutral settings
/// view. The workspace remains the settings session and writeback owner.
struct GpuiSettingsWindow {
    settings: Entity<GpuiSettings>,
    keymap: Entity<GpuiKeymapEditor>,
    active_tab: SettingsWindowTab,
    focus_initialized: bool,
    pending_target: Option<SettingsWindowTarget>,
    config_editor: Option<EditorFileTab>,
    keymap_file_editor: Option<EditorFileTab>,
    editor_close_after_save: Option<EditorCloseTarget>,
    workspace: WeakEntity<GpuiWorkspace>,
    close_prompt_pending: bool,
}

const SETTINGS_CONTENT_MIN_WIDTH_REMS: f32 = 25.0;
const SETTINGS_WINDOW_MIN_HEIGHT: f32 = 240.0;

fn keep_settings_windowed(window: &Window) {
    if window.is_fullscreen() {
        window.toggle_fullscreen();
    }
}

fn establish_settings_window_shadow(window: &Window) {
    window.on_next_frame(|window, _| {
        window.activate_window();
        crate::window::macos_set_window_shadow(&window.window_title(), true);
    });
}

fn activate_settings_window(
    root: &mut GpuiSettingsWindow,
    window: &mut Window,
    cx: &mut Context<GpuiSettingsWindow>,
) {
    // A settings window is never a fullscreen surface. In particular, macOS may restore a
    // previously-fullscreen auxiliary window when it rejoins the active Space.
    keep_settings_windowed(window);
    window.activate_window();
    establish_settings_window_shadow(window);
    match root.active_tab {
        SettingsWindowTab::Settings => root
            .settings
            .update(cx, |settings, cx| settings.focus(window, cx)),
        SettingsWindowTab::Keymap => root
            .keymap
            .update(cx, |keymap, cx| keymap.focus(window, cx)),
        SettingsWindowTab::File(kind) => {
            root.reconcile_file_editor(kind, window, cx);
            if let Some(tab) = root.editor_tab(kind) {
                tab.editor.update(cx, |editor, cx| editor.focus(window, cx));
            }
        }
    }
}

impl GpuiSettingsWindow {
    fn new(
        settings: Entity<GpuiSettings>,
        keymap: Entity<GpuiKeymapEditor>,
        workspace: WeakEntity<GpuiWorkspace>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Self {
        let root = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            root.update(cx, |root, cx| {
                if root.first_dirty_editor(cx).is_none() {
                    return true;
                }
                root.prompt_to_close(window, cx);
                false
            })
            .unwrap_or(true)
        });
        let workspace_for_release = workspace.clone();
        cx.on_release(move |_, cx| {
            let _ = workspace_for_release.update_in(cx, |workspace, window, cx| {
                workspace.settings_window = None;
                workspace.settings_window_opening = false;
                window.activate_window();
                schedule_focus(workspace.focus.clone(), window, cx);
                cx.notify();
            });
        })
        .detach();
        Self {
            settings,
            keymap,
            active_tab: SettingsWindowTab::Settings,
            focus_initialized: false,
            pending_target: None,
            config_editor: None,
            keymap_file_editor: None,
            editor_close_after_save: None,
            workspace,
            close_prompt_pending: false,
        }
    }

    fn activate_target(
        &mut self,
        target: SettingsWindowTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        keep_settings_windowed(window);
        window.activate_window();
        establish_settings_window_shadow(window);
        self.active_tab = match &target {
            SettingsWindowTarget::Settings | SettingsWindowTarget::Setting(_) => {
                SettingsWindowTab::Settings
            }
            SettingsWindowTarget::Keymap(_) => SettingsWindowTab::Keymap,
        };
        if !self.focus_initialized {
            self.pending_target = Some(target);
            cx.notify();
            return;
        }
        self.focus_target(target, window, cx);
    }

    fn focus_target(
        &mut self,
        target: SettingsWindowTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            SettingsWindowTarget::Settings => {
                self.active_tab = SettingsWindowTab::Settings;
                self.settings
                    .update(cx, |settings, cx| settings.focus(window, cx));
            }
            SettingsWindowTarget::Setting(id) => {
                self.active_tab = SettingsWindowTab::Settings;
                self.settings.update(cx, |settings, cx| {
                    settings.apply_search(&id, cx);
                    settings.focus(window, cx);
                });
            }
            SettingsWindowTarget::Keymap(requested_action) => {
                self.active_tab = SettingsWindowTab::Keymap;
                self.keymap.update(cx, |keymap, cx| {
                    if let Some(action) = requested_action {
                        keymap.focus_action(&action, window, cx);
                    } else {
                        keymap.focus(window, cx);
                    }
                });
            }
        }
        cx.notify();
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.first_dirty_editor(cx).is_some() {
            self.prompt_to_close(window, cx);
        } else {
            window.remove_window();
        }
    }

    fn prompt_to_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(kind) = self.first_dirty_editor(cx) {
            self.prompt_to_close_editor(kind, EditorCloseTarget::Window, window, cx);
            return;
        }
        window.remove_window();
    }

    fn open_file_editor(
        &mut self,
        kind: EditorFileKind,
        path: PathBuf,
        contents: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.editor_tab(kind).map(|tab| tab.editor.clone()) {
            self.active_tab = SettingsWindowTab::File(kind);
            self.reconcile_file_editor(kind, window, cx);
            editor.update(cx, |editor, cx| editor.focus(window, cx));
            cx.notify();
            return;
        }

        let editor_path = path.clone();
        let original_contents = contents.clone();
        let editor = cx.new(|cx| FileEditor::new_for_path(contents, &editor_path, window, cx));
        let subscription = cx.subscribe_in(
            &editor,
            window,
            move |this, _, event: &FileEditorEvent, window, cx| match event {
                FileEditorEvent::Save { contents } => {
                    this.save_file_editor(kind, contents.clone(), window, cx);
                }
            },
        );
        let tab = EditorFileTab {
            editor: editor.clone(),
            path,
            original_contents,
            reconcile_generation: 0,
            _subscription: subscription,
        };
        match kind {
            EditorFileKind::Config => self.config_editor = Some(tab),
            EditorFileKind::Keymap => self.keymap_file_editor = Some(tab),
        }
        self.active_tab = SettingsWindowTab::File(kind);
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        cx.notify();
    }

    const fn editor_tab(&self, kind: EditorFileKind) -> Option<&EditorFileTab> {
        match kind {
            EditorFileKind::Config => self.config_editor.as_ref(),
            EditorFileKind::Keymap => self.keymap_file_editor.as_ref(),
        }
    }

    /// Refresh a clean file tab from disk after accepted settings change. The read runs off the
    /// UI executor; the completion path rechecks cleanliness and the loaded baseline so a draft
    /// or a newer refresh can never be replaced by an older snapshot.
    fn reconcile_file_editor(&mut self, kind: EditorFileKind, window: &Window, cx: &Context<Self>) {
        let Some(tab) = self.editor_tab(kind) else {
            return;
        };
        if tab.editor.read(cx).is_dirty(cx) {
            return;
        }
        let path = tab.path.clone();
        let expected_baseline = tab.original_contents.clone();
        let editor = tab.editor.clone();
        let editor_id = editor.entity_id();
        let root = cx.weak_entity();
        let generation = self
            .editor_tab_mut(kind)
            .map(|tab| {
                tab.reconcile_generation = tab.reconcile_generation.wrapping_add(1);
                tab.reconcile_generation
            })
            .unwrap_or_default();
        let read = cx
            .background_executor()
            .spawn(async move { bootty_host::text_file::load_text_file(path) });
        window
            .spawn(cx, async move |cx| {
                let Ok(loaded) = read.await else {
                    return;
                };
                let _ = cx.update(|window, cx| {
                    let _ = root.update(cx, |root, cx| {
                        let Some(tab) = root.editor_tab_mut(kind) else {
                            return;
                        };
                        if tab.editor.entity_id() != editor_id
                            || tab.reconcile_generation != generation
                            || tab.original_contents != expected_baseline
                            || tab.editor.read(cx).is_dirty(cx)
                        {
                            return;
                        }
                        if tab.editor.read(cx).contents(cx) == loaded.contents {
                            return;
                        }
                        tab.original_contents.clone_from(&loaded.contents);
                        editor.update(cx, |editor, cx| {
                            editor.replace_snapshot(loaded.contents, window, cx);
                        });
                    });
                });
            })
            .detach();
    }

    const fn editor_tab_mut(&mut self, kind: EditorFileKind) -> Option<&mut EditorFileTab> {
        match kind {
            EditorFileKind::Config => self.config_editor.as_mut(),
            EditorFileKind::Keymap => self.keymap_file_editor.as_mut(),
        }
    }

    fn first_dirty_editor(&self, cx: &gpui_kit::App) -> Option<EditorFileKind> {
        [EditorFileKind::Config, EditorFileKind::Keymap]
            .into_iter()
            .find(|kind| {
                self.editor_tab(*kind)
                    .is_some_and(|tab| tab.editor.read(cx).is_dirty(cx))
            })
    }

    fn save_file_editor(
        &self,
        kind: EditorFileKind,
        contents: String,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let Some(tab) = self.editor_tab(kind) else {
            return;
        };
        let path = tab.path.clone();
        let original_contents = tab.original_contents.clone();
        let editor = tab.editor.clone();
        let persisted_contents = contents.clone();
        let root = cx.weak_entity();
        let write = cx.background_executor().spawn(async move {
            bootty_host::text_file::save_text_file_if_unchanged(path, &original_contents, &contents)
        });
        window
            .spawn(cx, async move |cx| {
                let result = write.await;
                let _ = cx.update(|window, cx| {
                    let saved = match result {
                        Ok(outcome) => {
                            editor.update(cx, |editor, cx| {
                                editor.mark_saved(
                                    persisted_contents.clone(),
                                    outcome.durability_warning,
                                    cx,
                                );
                            });
                            let _ = root.update(cx, |root, _| {
                                let tab = match kind {
                                    EditorFileKind::Config => root.config_editor.as_mut(),
                                    EditorFileKind::Keymap => root.keymap_file_editor.as_mut(),
                                };
                                if let Some(tab) = tab
                                    && tab.editor.entity_id() == editor.entity_id()
                                {
                                    tab.original_contents.clone_from(&persisted_contents);
                                    tab.reconcile_generation =
                                        tab.reconcile_generation.wrapping_add(1);
                                }
                            });
                            true
                        }
                        Err(error) => {
                            editor.update(cx, |editor, cx| {
                                editor.save_failed(error.to_string(), cx);
                            });
                            false
                        }
                    };
                    if saved && kind == EditorFileKind::Keymap {
                        let Some(workspace) =
                            root.read_with(cx, |root, _| root.workspace.clone()).ok()
                        else {
                            return;
                        };
                        cx.defer(move |cx| {
                            let _ = workspace.update_in(cx, |workspace, _, cx| {
                                if let Err(error) = gpui_keymap_editor::reload_saved_keymap_text(
                                    &mut workspace.state,
                                ) {
                                    workspace
                                        .state
                                        .record_error(format!("reload keymap: {error}"));
                                }
                                workspace.last_keymap_editor_revision =
                                    Some(workspace.state.keymap_snapshot().revision);
                                workspace.publish_keymap_editor_snapshot(cx);
                                cx.notify();
                            });
                        });
                    }
                    let _ = root.update(cx, |root, cx| {
                        root.finish_file_save(kind, window, cx);
                    });
                });
            })
            .detach();
    }

    fn finish_file_save(
        &mut self,
        kind: EditorFileKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.editor_tab(kind) else {
            self.editor_close_after_save = None;
            return;
        };
        if tab.editor.read(cx).save_in_flight()
            || tab.editor.read(cx).is_dirty(cx)
            || !tab.editor.read(cx).save_allows_close()
        {
            return;
        }
        match self.editor_close_after_save.take() {
            Some(EditorCloseTarget::Tab(close_kind)) if close_kind == kind => {
                self.close_file_tab(kind, window, cx);
            }
            Some(EditorCloseTarget::Window) => self.request_close(window, cx),
            Some(target) => self.editor_close_after_save = Some(target),
            None => {}
        }
        cx.notify();
    }

    fn request_close_file_tab(
        &mut self,
        kind: EditorFileKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .editor_tab(kind)
            .is_some_and(|tab| tab.editor.read(cx).is_dirty(cx))
        {
            self.prompt_to_close_editor(kind, EditorCloseTarget::Tab(kind), window, cx);
        } else {
            self.close_file_tab(kind, window, cx);
        }
    }

    fn close_file_tab(
        &mut self,
        kind: EditorFileKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab == SettingsWindowTab::File(kind) {
            self.active_tab = SettingsWindowTab::Settings;
            self.settings
                .update(cx, |settings, cx| settings.focus(window, cx));
        }
        match kind {
            EditorFileKind::Config => self.config_editor = None,
            EditorFileKind::Keymap => self.keymap_file_editor = None,
        }
        self.editor_close_after_save = None;
        cx.notify();
    }

    fn prompt_to_close_editor(
        &mut self,
        kind: EditorFileKind,
        target: EditorCloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_prompt_pending {
            return;
        }
        let Some(tab) = self.editor_tab(kind) else {
            return;
        };
        let detail = tab.path.display().to_string();
        let title = tab
            .path
            .file_name()
            .map_or_else(|| "file".to_owned(), |name| name.to_string_lossy().into());
        let answer = crate::gpui::prompt(
            &format!("Save changes to {title}?"),
            Some(&detail),
            &["Save".into(), "Discard".into(), "Cancel".into()],
            window,
            cx,
        );
        let editor = tab.editor.clone();
        self.close_prompt_pending = true;
        let root = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                let answer = answer.await;
                let _ = cx.update(|window, cx| {
                    let _ = root.update(cx, |root, _| root.close_prompt_pending = false);
                    match answer {
                        Ok(0) => {
                            let _ = root.update(cx, |root, _| {
                                root.editor_close_after_save = Some(target);
                            });
                            editor.update(cx, FileEditor::request_save);
                        }
                        Ok(1) => {
                            let _ = root.update(cx, |root, cx| match target {
                                EditorCloseTarget::Tab(close_kind) => {
                                    root.close_file_tab(close_kind, window, cx);
                                }
                                EditorCloseTarget::Window => {
                                    root.close_file_tab(kind, window, cx);
                                    root.request_close(window, cx);
                                }
                            });
                        }
                        Ok(2..) | Err(_) => {
                            editor.update(cx, |editor, cx| editor.focus(window, cx));
                        }
                    }
                });
            })
            .detach();
    }
}

impl GpuiSettingsWindow {
    fn render_tabs(
        &self,
        active_editor: Option<&Entity<FileEditor>>,
        cx: &Context<Self>,
    ) -> TabBar {
        let active_tab = self.active_tab;
        let file_kinds: Arc<[EditorFileKind]> = [
            self.config_editor.as_ref().map(|_| EditorFileKind::Config),
            self.keymap_file_editor
                .as_ref()
                .map(|_| EditorFileKind::Keymap),
        ]
        .into_iter()
        .flatten()
        .collect();
        let selected_index = match active_tab {
            SettingsWindowTab::Settings => 0,
            SettingsWindowTab::Keymap => 1,
            SettingsWindowTab::File(kind) => file_kinds
                .iter()
                .position(|candidate| *candidate == kind)
                .map_or(0, |index| index.saturating_add(2)),
        };
        let click_file_kinds = Arc::clone(&file_kinds);
        let mut tabs = TabBar::new("settings-window-tab-bar")
            .with_size(Size::Medium)
            .segmented()
            .w_full()
            .bg(gpui_kit::component::Theme::global(cx).colors.sidebar)
            .selected_index(selected_index)
            .on_click(cx.listener(move |this, index: &usize, window, cx| {
                this.active_tab = match *index {
                    0 => SettingsWindowTab::Settings,
                    1 => SettingsWindowTab::Keymap,
                    index => click_file_kinds
                        .get(index.saturating_sub(2))
                        .copied()
                        .map_or(SettingsWindowTab::Settings, SettingsWindowTab::File),
                };
                activate_settings_window(this, window, cx);
                cx.notify();
            }))
            .child(
                Tab::new()
                    .label("Settings")
                    .aria_label("Bootty settings")
                    .debug_selector(|| "settings-window-tab-settings".to_owned()),
            )
            .child(
                Tab::new()
                    .label("Keymap")
                    .aria_label("Bootty keymap editor")
                    .debug_selector(|| "settings-window-tab-keymap".to_owned()),
            );
        for kind in file_kinds.iter().copied() {
            if let Some(tab) = self.editor_tab(kind) {
                tabs = tabs.child(Self::render_editor_tab(kind, tab, cx));
            }
        }
        if active_editor.is_some() {
            tabs = tabs.suffix(Self::save_editor_button(active_editor, cx));
        }
        tabs
    }

    fn save_editor_button(
        active_editor: Option<&Entity<FileEditor>>,
        cx: &Context<Self>,
    ) -> Button {
        let editor_saving = active_editor.is_some_and(|editor| editor.read(cx).save_in_flight());
        let editor_can_save = active_editor.is_some_and(|editor| editor.read(cx).can_save(cx));
        Button::new("settings-window-save-editor")
            .debug_selector(|| "settings-window-save-editor".to_owned())
            .ghost()
            .small()
            .label(if editor_saving { "Saving…" } else { "Save" })
            .disabled(!editor_can_save)
            .on_click(cx.listener(|this, _, _, cx| {
                if let SettingsWindowTab::File(kind) = this.active_tab
                    && let Some(tab) = this.editor_tab(kind)
                {
                    tab.editor.update(cx, FileEditor::request_save);
                }
            }))
    }

    fn render_editor_tab(kind: EditorFileKind, tab: &EditorFileTab, cx: &Context<Self>) -> Tab {
        let dirty = tab.editor.read(cx).is_dirty(cx);
        let file_name = tab.path.file_name().map_or_else(
            || "Untitled".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let label = if dirty {
            format!("{file_name} ●")
        } else {
            file_name.clone()
        };
        let (close_id, close_selector, tab_selector) = match kind {
            EditorFileKind::Config => (
                "settings-window-close-config",
                "settings-window-close-config",
                "settings-window-tab-config",
            ),
            EditorFileKind::Keymap => (
                "settings-window-close-keymap-file",
                "settings-window-close-keymap-file",
                "settings-window-tab-keymap-file",
            ),
        };
        let close = Button::new(close_id)
            .debug_selector(move || close_selector.to_owned())
            .ghost()
            .xsmall()
            .icon(IconName::Close)
            .accessibility_label(format!("Close {file_name}"))
            .tooltip(format!("Close {file_name}"))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.request_close_file_tab(kind, window, cx);
            }));
        Tab::new()
            .label(label)
            .aria_label(format!("Edit {file_name}"))
            .suffix(close)
            .debug_selector(move || tab_selector.to_owned())
    }
}

impl Render for GpuiSettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::window::macos_set_window_shadow(&window.window_title(), true);
        keep_settings_windowed(window);
        // `new` and the initial target selection run before this view's tracked focus nodes exist.
        // Defer the first focus until this render attaches those nodes, matching the main window.
        if !self.focus_initialized {
            self.focus_initialized = true;
            let target = self
                .pending_target
                .take()
                .unwrap_or(SettingsWindowTarget::Settings);
            cx.defer_in(window, move |this, window, cx| {
                this.focus_target(target, window, cx);
            });
        }
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let font = setup_ui_font(window, cx);
        let active_tab = self.active_tab;
        let active_editor = match active_tab {
            SettingsWindowTab::File(kind) => self.editor_tab(kind).map(|tab| tab.editor.clone()),
            SettingsWindowTab::Settings | SettingsWindowTab::Keymap => None,
        };
        let tabs = self.render_tabs(active_editor.as_ref(), cx);
        let tab_strip = SettingsTitleBar::new(
            active_tab == SettingsWindowTab::Settings,
            // Keep one title surface across Settings, Keymap, and document tabs.
            tabs.bg(gpui_kit::component::Theme::global(cx).colors.sidebar),
        );

        let body = match (active_tab, active_editor) {
            (SettingsWindowTab::File(_), Some(editor)) => editor.into_any_element(),
            (SettingsWindowTab::Keymap, _) => self.keymap.clone().into_any_element(),
            _ => self.settings.clone().into_any_element(),
        };
        let title_bar = crate::platform::client_title_bar("Bootty — Settings", window).map(|bar| {
            bar.on_close_window(cx.listener(|this, _, window, cx| {
                this.request_close(window, cx);
            }))
        });
        div()
            .relative()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .font(font)
            .on_action(cx.listener(
                |_, _: &crate::gpui_actions::CycleApplicationWindow, window, cx| {
                    crate::gpui_actions::cycle_application_window(window, cx);
                },
            ))
            .children(title_bar)
            .child(tab_strip)
            .child(div().flex_1().min_h_0().child(body))
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
    }
}

fn settings_window_options(
    decorations: bootty_config::config::WindowDecoration,
    cx: &gpui_kit::App,
) -> WindowOptions {
    let minimum_width = px(f32::from(crate::gpui::ui_rem_size(cx))
        * (crate::gpui::SETTINGS_SIDEBAR_WIDTH_REMS + SETTINGS_CONTENT_MIN_WIDTH_REMS));
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Bootty — Settings".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.0), px(12.0))),
        }),
        focus: true,
        show: true,
        is_movable: true,
        app_owns_titlebar_drag: cfg!(target_os = "linux"),
        // Auxiliary windows stay decorated even when the workspace is borderless or fullscreen.
        window_decorations: Some(
            if decorations == bootty_config::config::WindowDecoration::Client {
                WindowDecorations::Client
            } else {
                WindowDecorations::Server
            },
        ),
        kind: WindowKind::Normal,
        app_id: Some(
            bootty_config::ApplicationIdentity::current()
                .bundle_identifier()
                .to_owned(),
        ),
        // Keep the navigation and content columns usable at the smallest size, as Zed does for
        // its settings window. Explicit centered windowed bounds also prevent inheriting the
        // workspace's fullscreen state when this auxiliary window is first opened.
        window_min_size: Some(size(minimum_width, px(SETTINGS_WINDOW_MIN_HEIGHT))),
        window_bounds: Some(WindowBounds::centered(
            gpui_kit::DEFAULT_ADDITIONAL_WINDOW_SIZE,
            cx,
        )),
        ..Default::default()
    }
}

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

// All child views and their subscriptions exist before the workspace is published.
struct WorkspaceInitialization {
    display_id: Option<u32>,
    unsupported_sources: Vec<UnsupportedModuleDiagnostic>,
    integration_rows: Vec<ModuleIntegrationsSnapshot>,
    terminal: gpui_kit::Entity<GpuiTerminalView>,
    terminal_subscription: Subscription,
    terminal_scroll_subscription: Subscription,
    terminal_focus_subscriptions: [Subscription; 2],
    terminal_text: crate::terminal_text::TerminalTextConfig,
    terminal_text_contract: Arc<TerminalTextContract>,
    terminal_cell: CellMetrics,
    keymap_context: String,
    input: crate::gpui::InputAccumulator,
    workspace: WeakEntity<GpuiWorkspace>,
    focus: FocusHandle,
    window_activation_subscription: Subscription,
    window_appearance_subscription: Subscription,
    settings_runtime: SettingsRuntime,
    settings_view: gpui_kit::Entity<GpuiSettings>,
    settings_subscription: Subscription,
    keymap_editor: Entity<GpuiKeymapEditor>,
    keymap_editor_subscription: Subscription,
    dialog_view: gpui_kit::Entity<DialogView>,
    dialog_subscription: Subscription,
    overlay_host: gpui_kit::Entity<OverlayHost>,
    overlay_subscription: Subscription,
    chrome_view: gpui_kit::Entity<GpuiChrome>,
    last_ui_theme: crate::gpui::UiTheme,
    chrome_subscription: Subscription,
}

/// Root GPUI entity for one Bootty window.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Window focus, panel visibility, and modal lifetimes vary independently"
)]
pub struct GpuiWorkspace {
    state: AppState,
    workspace_bounds: Bounds<Pixels>,
    tools: Option<Entity<crate::gpui_dock::WorkspaceDock>>,
    tools_scope: Option<bootty_mux::controller::SpaceId>,
    document_close_prompt: bool,
    tools_visible: bool,
    tools_focus_subscription: Option<Subscription>,
    unsupported_sources: Vec<UnsupportedModuleDiagnostic>,
    integration_rows: Vec<ModuleIntegrationsSnapshot>,
    launch: WorkspaceLaunch,
    terminal: gpui_kit::Entity<GpuiTerminalView>,
    _terminal_subscription: Subscription,
    _terminal_scroll_subscription: Subscription,
    _terminal_focus_subscriptions: [Subscription; 2],
    terminal_panes: HashMap<String, gpui_kit::Entity<GpuiTerminalView>>,
    terminal_pane_subscriptions: HashMap<String, Vec<Subscription>>,
    terminal_interactions: HashMap<String, GpuiTerminalInteraction>,
    terminal_mouse_buttons: HashSet<MouseButton>,
    pending_link_click: Option<(gpui_kit::Point<gpui_kit::Pixels>, Option<CommandInvocation>)>,
    visual_bell_until: Option<Instant>,
    last_locale: String,
    pane_hit_rects: Vec<(String, SurfaceRect)>,
    frame_metrics: RendererMetrics,
    terminal_text: crate::terminal_text::TerminalTextConfig,
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
    settings_view: gpui_kit::Entity<GpuiSettings>,
    _settings_subscription: Subscription,
    settings_window: Option<WeakEntity<GpuiSettingsWindow>>,
    settings_window_opening: bool,
    keymap_editor: Entity<GpuiKeymapEditor>,
    _keymap_editor_subscription: Subscription,
    last_keymap_revision: Option<u64>,
    last_keymap_focus: Option<KeymapFocus>,
    last_keymap_backend: Option<MultiplexerBackendConfig>,
    last_keymap_editor_revision: Option<u64>,
    last_config_file_editor_revision: Option<u64>,
    dialog_view: gpui_kit::Entity<DialogView>,
    _dialog_subscription: Subscription,
    overlay_host: gpui_kit::Entity<OverlayHost>,
    _overlay_subscription: Subscription,
    overlay_kind: Option<OverlayKind>,
    root_dialog_kind: Option<RootDialogKind>,
    /// Dialog id plus Root title. The title and chrome are baked in at `open_dialog`, so a
    /// multi-step dialog that keeps its id but changes role must still reopen the Root.
    root_dialog_key: Option<(String, Option<String>)>,
    space_editor_view: Option<gpui_kit::Entity<GpuiSpaceEditor>>,
    space_editor_subscription: Option<Subscription>,
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
    work_repaint_pending: bool,
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
    ) -> Result<gpui_kit::WindowHandle<Root>> {
        let launch = WorkspaceLaunch::new(window_state_key.clone(), backends, control_plane);
        Self::open_with_launch(config, window_state_key, launch, cx)
    }

    fn open_with_launch(
        config: BoottyConfig,
        window_state_key: String,
        launch: WorkspaceLaunch,
        cx: &mut App,
    ) -> Result<gpui_kit::WindowHandle<Root>> {
        let options = crate::platform::native_options_for_config(&config, cx);
        let bordered =
            config.window.window_decoration != bootty_config::config::WindowDecoration::None;
        // Prepare fallible state before GPUI's infallible entity constructor publishes a view.
        // The bounded wake channel retains work that arrives before the window subscribes.
        let (repaint_tx, repaint_rx) = async_channel::bounded(1);
        let repaint: bootty_mux::RepaintHandle = Arc::new(move || {
            let _ = repaint_tx.try_send(());
        });
        let state = AppState::new_for_window_with_agents(
            config,
            window_state_key.clone(),
            Arc::clone(&launch.backends),
            repaint.clone(),
            None,
            None,
            Some(launch.control_plane.event_sender()),
        )?;
        cx.open_window(options, move |window, cx| {
            let workspace = cx.new(|cx| {
                Self::new(
                    state,
                    &window_state_key,
                    launch,
                    repaint,
                    repaint_rx,
                    window,
                    cx,
                )
            });
            cx.new(|cx| Root::new(workspace, window, cx).bordered(bordered))
        })
    }

    fn new(
        mut state: AppState,
        window_state_key: &str,
        launch: WorkspaceLaunch,
        repaint: bootty_mux::RepaintHandle,
        repaint_rx: async_channel::Receiver<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = Self::initialize_workspace(
            &mut state,
            window_state_key,
            &launch,
            &repaint,
            repaint_rx,
            window,
            cx,
        );
        Self {
            workspace_bounds: Bounds::new(point(px(0.0), px(0.0)), window.viewport_size()),
            display_id: initial.display_id,
            tools: None,
            tools_scope: None,
            document_close_prompt: false,
            tools_visible: false,
            tools_focus_subscription: None,
            state,
            unsupported_sources: initial.unsupported_sources,
            integration_rows: initial.integration_rows,
            launch,
            terminal: initial.terminal,
            _terminal_subscription: initial.terminal_subscription,
            _terminal_scroll_subscription: initial.terminal_scroll_subscription,
            _terminal_focus_subscriptions: initial.terminal_focus_subscriptions,
            terminal_panes: HashMap::new(),
            terminal_pane_subscriptions: HashMap::new(),
            terminal_interactions: HashMap::new(),
            terminal_mouse_buttons: HashSet::new(),
            pending_link_click: None,
            visual_bell_until: None,
            last_locale: String::new(),
            pane_hit_rects: Vec::new(),
            frame_metrics: RendererMetrics::default(),
            terminal_text: initial.terminal_text,
            terminal_text_contract: initial.terminal_text_contract,
            terminal_base_cell: initial.terminal_cell,
            terminal_display_scale: window.scale_factor(),
            terminal_cell: initial.terminal_cell,
            keymap_context: initial.keymap_context,
            input: initial.input,
            workspace: initial.workspace,
            focus: initial.focus,
            focus_initialized: false,
            _window_activation_subscription: initial.window_activation_subscription,
            _window_appearance_subscription: initial.window_appearance_subscription,
            cursor: CursorStyle::IBeam,
            terminal_cursor: CursorStyle::IBeam,
            settings_runtime: initial.settings_runtime,
            settings_started: Instant::now(),
            last_settings_revision: None,
            settings_view: initial.settings_view,
            _settings_subscription: initial.settings_subscription,
            settings_window: None,
            settings_window_opening: false,
            keymap_editor: initial.keymap_editor,
            _keymap_editor_subscription: initial.keymap_editor_subscription,
            last_keymap_revision: None,
            last_keymap_focus: None,
            last_keymap_backend: None,
            last_keymap_editor_revision: None,
            last_config_file_editor_revision: None,
            dialog_view: initial.dialog_view,
            _dialog_subscription: initial.dialog_subscription,
            overlay_host: initial.overlay_host,
            _overlay_subscription: initial.overlay_subscription,
            overlay_kind: None,
            root_dialog_kind: None,
            root_dialog_key: None,
            space_editor_view: None,
            space_editor_subscription: None,
            chrome_view: initial.chrome_view,
            last_error_notification: None,
            last_ui_theme: initial.last_ui_theme,
            last_background_material: None,
            _chrome_subscription: initial.chrome_subscription,
            pending_window_move: false,
            pending_effects: Vec::new(),
            scheduled_repaint: None,
            scheduled_maintenance: None,
            frame_update_pending: true,
            work_repaint_pending: false,
            last_maintenance_chrome: None,
            last_pane_layouts: Vec::new(),
            repaint,
        }
    }

    fn initialize_workspace(
        state: &mut AppState,
        window_state_key: &str,
        launch: &WorkspaceLaunch,
        repaint: &bootty_mux::RepaintHandle,
        repaint_rx: async_channel::Receiver<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorkspaceInitialization {
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
        Self::watch_workspace_work(repaint, repaint_rx, window, cx);
        let mut input = crate::gpui::InputAccumulator::default();
        input.set_wake(repaint.clone());
        input.window_focused(window_focused);
        state.set_appearance_variant(startup_variant);
        let settings_runtime = SettingsRuntime::default();
        settings_runtime.request_catalog(&state.config().config_path, repaint);
        let native_settings = settings_runtime.current_catalog();
        let unsupported_sources = native_settings.unsupported_sources;
        let integration_rows = native_settings.integration_rows;

        let (settings_view, settings_subscription) = Self::create_settings_view(
            state,
            &font_families,
            &unsupported_sources,
            &integration_rows,
            window,
            cx,
        );
        let (keymap_editor, keymap_editor_subscription) =
            Self::create_keymap_editor(state, window, cx);
        let (dialog_view, dialog_subscription) = Self::create_dialog_view(window, cx);
        let (overlay_host, overlay_subscription) = Self::create_overlay_host(cx);
        let (chrome_view, chrome_subscription) =
            Self::create_chrome_view(state, launch, &keymap_context, window, cx);
        let terminal = cx.new(GpuiTerminalView::new);
        terminal.update(cx, |terminal, cx| {
            terminal.set_window_focused(window_focused, cx);
        });
        let focus = terminal.focus_handle(cx);
        let terminal_focus_subscriptions = Self::subscribe_terminal_focus(&focus, window, cx);
        let (terminal_subscription, terminal_scroll_subscription) =
            Self::subscribe_terminal_events(&terminal, window, cx);
        let workspace = cx.weak_entity();
        let window_activation_subscription =
            Self::observe_workspace_activation(&terminal, window, cx);
        let (window_appearance_subscription, display_id) =
            Self::observe_workspace_window(&keymap_context, window, cx);
        let last_ui_theme = state.ui_theme();
        Self::schedule_initial_dock(window, cx);
        WorkspaceInitialization {
            display_id,
            unsupported_sources,
            integration_rows,
            terminal,
            terminal_subscription,
            terminal_scroll_subscription,
            terminal_focus_subscriptions,
            terminal_text,
            terminal_text_contract,
            terminal_cell,
            keymap_context,
            input,
            workspace,
            focus,
            window_activation_subscription,
            window_appearance_subscription,
            settings_runtime,
            settings_view,
            settings_subscription,
            keymap_editor,
            keymap_editor_subscription,
            dialog_view,
            dialog_subscription,
            overlay_host,
            overlay_subscription,
            chrome_view,
            last_ui_theme,
            chrome_subscription,
        }
    }

    fn subscribe_terminal_events(
        terminal: &Entity<GpuiTerminalView>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> (Subscription, Subscription) {
        let terminal_subscription = cx.subscribe_in(
            terminal,
            window,
            |this, _, input: &TerminalViewInput, window, cx| {
                this.apply_terminal_view_input(input.0.clone(), window, cx);
            },
        );
        let terminal_scroll_subscription =
            cx.subscribe(terminal, |this, _, input: &TerminalScrollbarInput, cx| {
                this.scroll_terminal(None, input, cx);
            });
        (terminal_subscription, terminal_scroll_subscription)
    }

    fn install_close_handler(window: &Window, cx: &Context<Self>) {
        let root = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            root.update(cx, |root, cx| {
                if root.pending_documents(false, cx).is_empty() {
                    return true;
                }
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
        repaint: &bootty_mux::RepaintHandle,
        repaint_rx: async_channel::Receiver<()>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        crate::menu::set_wake(repaint);
        let tray_window = cx.entity_id();
        cx.on_release(move |_, cx| crate::agent_tray::remove(tray_window, cx))
            .detach();
        cx.spawn_in(window, async move |weak, cx| {
            while repaint_rx.recv().await.is_ok() {
                let (frame_tx, frame_rx) = async_channel::bounded(1);
                let repaint = weak.update_in(cx, |this, window, cx| {
                    let repaint = this.process_work(window, cx);
                    if repaint {
                        window.on_next_frame(move |_, _| {
                            let _ = frame_tx.try_send(());
                        });
                    }
                    repaint
                });
                match repaint {
                    Err(_) => break,
                    Ok(true) if frame_rx.recv().await.is_err() => break,
                    _ => {}
                }
            }
        })
        .detach();
    }

    fn create_settings_view(
        state: &AppState,
        font_families: &Arc<[String]>,
        unsupported_sources: &[UnsupportedModuleDiagnostic],
        integration_rows: &[ModuleIntegrationsSnapshot],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<GpuiSettings>, Subscription) {
        let settings_view = cx.new(|cx| {
            GpuiSettings::for_app(
                state,
                Arc::clone(font_families),
                unsupported_sources,
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

    fn create_dialog_view(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<DialogView>, Subscription) {
        let dialog_view = cx.new(|cx| DialogView::new(window, cx));
        let dialog_subscription =
            cx.subscribe(&dialog_view, |this, _, intent: &DialogIntent, cx| {
                let mut effects = Vec::new();
                if intent_dialog_id(intent) == crate::presentation::dialogs::TERMINAL_FIND_ID {
                    this.state.apply_terminal_find_dialog_intent(intent);
                } else {
                    this.state.apply_dialog_intent(intent, &mut effects);
                }
                this.pending_effects.extend(effects);
                cx.notify();
            });
        (dialog_view, dialog_subscription)
    }

    fn create_overlay_host(cx: &mut Context<Self>) -> (Entity<OverlayHost>, Subscription) {
        let overlay_host = cx.new(|_| OverlayHost::new());
        let overlay_subscription =
            cx.subscribe(&overlay_host, |this, _, _: &gpui_kit::DismissEvent, cx| {
                match this.overlay_kind.take() {
                    Some(OverlayKind::Dialog) => {
                        this.state.close_overlay_dialogs();
                    }
                    None => {}
                }
                cx.notify();
            });
        (overlay_host, overlay_subscription)
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
            terminal_for_activation.update(cx, |terminal, cx| {
                terminal.set_window_focused(active, cx);
            });
            for terminal in this.terminal_panes.values() {
                terminal.update(cx, |terminal, cx| {
                    terminal.set_window_focused(active, cx);
                });
            }
            if active && window.focused(cx).is_none() && !window.has_active_dialog(cx) {
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
        cx.on_app_quit(|this, cx| this.close_link_forwards(cx))
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
            this.tools_visible = false;
            if let Some(dock) = &this.tools {
                dock.update(cx, |dock, cx| dock.set_inspector_visible(false, window, cx));
            }
        });
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
                Ok(window) => {
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

    fn open_settings_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.open_settings_window_target(SettingsWindowTarget::Settings, window, cx);
    }

    fn open_settings_window_target(
        &mut self,
        target: SettingsWindowTarget,
        _window: &Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = self.settings_window.clone() {
            let existing_target = target.clone();
            if handle
                .update_in(cx, move |root, window, cx| {
                    root.activate_target(existing_target, window, cx);
                })
                .is_ok()
            {
                return;
            }
            self.settings_window = None;
        }
        if self.settings_window_opening {
            return;
        }
        self.settings_window_opening = true;

        let settings = self.settings_view.clone();
        let keymap = self.keymap_editor.clone();
        let workspace = cx.weak_entity();
        let workspace_owner = self.workspace.clone();
        let decorations = self.state.config().window.window_decoration;
        cx.defer(move |cx| {
            let options = settings_window_options(decorations, cx);
            let window = cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| {
                    GpuiSettingsWindow::new(
                        settings.clone(),
                        keymap.clone(),
                        workspace_owner,
                        window,
                        cx,
                    )
                });
                let target = target.clone();
                view.update(cx, |root, cx| {
                    root.activate_target(target, window, cx);
                });
                cx.new(|cx| Root::new(view, window, cx).bordered(true))
            });
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.settings_window = window.ok().and_then(|window| {
                    window
                        .read(cx)
                        .ok()?
                        .view()
                        .clone()
                        .downcast::<GpuiSettingsWindow>()
                        .ok()
                        .map(|view| view.downgrade())
                });
                workspace.settings_window_opening = false;
                cx.notify();
            });
        });
    }

    fn close_settings_window(&mut self, cx: &mut Context<Self>) {
        self.settings_window_opening = false;
        let Some(window) = self.settings_window.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = window.update_in(cx, GpuiSettingsWindow::request_close);
        });
    }

    fn request_config_editor_tab(&mut self, cx: &mut Context<Self>) {
        let path = self.state.config().config_path.clone();
        let loaded = match bootty_host::text_file::load_text_file(&path) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.state
                    .record_error(format!("open {}: {error}", path.display()));
                cx.notify();
                return;
            }
        };
        let Some(settings_window) = self.settings_window.clone() else {
            self.state.record_error(
                "open config.toml tab: Settings window is no longer available".to_owned(),
            );
            cx.notify();
            return;
        };
        let contents = loaded.contents;
        cx.defer(move |cx| {
            let _ = settings_window.update_in(cx, |root, window, cx| {
                root.open_file_editor(EditorFileKind::Config, path, contents, window, cx);
            });
        });
    }

    fn request_keymap_file_editor_tab(&mut self, cx: &mut Context<Self>) {
        let loaded = match gpui_keymap_editor::load_keymap_text_file(&self.state) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.state.record_error(format!("open keymap: {error}"));
                cx.notify();
                return;
            }
        };
        let path = loaded.path.clone();
        let contents = loaded.contents;
        let Some(settings_window) = self.settings_window.clone() else {
            self.state.record_error(
                "open keymap.json tab: Settings window is no longer available".to_owned(),
            );
            cx.notify();
            return;
        };
        cx.defer(move |cx| {
            let _ = settings_window.update_in(cx, |root, window, cx| {
                root.open_file_editor(EditorFileKind::Keymap, path, contents, window, cx);
            });
        });
    }

    fn request_keymap_window(&self, requested_action: Option<String>, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_keymap_window(requested_action, window, cx);
            });
        });
    }

    fn open_keymap_window(
        &mut self,
        requested_action: Option<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_window_target(
            SettingsWindowTarget::Keymap(requested_action),
            window,
            cx,
        );
    }

    fn close_keymap_window(&self, cx: &mut Context<Self>) {
        let Some(settings_window) = self.settings_window.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = settings_window.update_in(cx, |root, window, cx| {
                root.activate_target(SettingsWindowTarget::Settings, window, cx);
            });
        });
    }

    fn publish_keymap_editor_snapshot(&self, cx: &mut Context<Self>) {
        let editor = self.keymap_editor.clone();
        let snapshot = keymap_editor_snapshot(&self.state);
        let settings_window = self.settings_window.clone();
        cx.defer(move |cx| {
            editor.update(cx, |editor, cx| editor.set_snapshot(snapshot, cx));
            if let Some(settings_window) = settings_window {
                let _ = settings_window.update_in(cx, |root, window, cx| {
                    root.reconcile_file_editor(EditorFileKind::Keymap, window, cx);
                    cx.notify();
                });
            }
        });
    }

    fn apply_keymap_editor_intent(&mut self, intent: KeymapEditorIntent, cx: &mut Context<Self>) {
        match intent {
            KeymapEditorIntent::Close => {
                self.close_keymap_window(cx);
                return;
            }
            KeymapEditorIntent::OpenKeymapFile => {
                self.request_keymap_file_editor_tab(cx);
            }
            edit_intent => match persisted_keymap_edit(&edit_intent) {
                Ok(Some(edit)) => {
                    if let Err(error) = self.state.edit_keymap(&edit) {
                        self.state.record_error(format!("edit keymap: {error}"));
                    }
                }
                Ok(None) => {}
                Err(error) => self.state.record_error(format!("edit keymap: {error}")),
            },
        }
        self.last_keymap_editor_revision = Some(self.state.keymap_snapshot().revision);
        self.publish_keymap_editor_snapshot(cx);
        cx.notify();
    }

    fn terminal_view_focused(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.terminal.focus_handle(cx).is_focused(window)
            || self
                .terminal_panes
                .values()
                .any(|view| view.focus_handle(cx).is_focused(window))
    }

    fn subscribe_terminal_focus(
        focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> [Subscription; 2] {
        [
            cx.on_focus(focus, window, |this, window, cx| {
                this.state
                    .apply_sidebar_action(crate::app_actions::SidebarAction::FocusTerminal);
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
        let focus = if self.state.modal_dialog().is_some() {
            self.state.keymap_focus()
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
        if self.last_keymap_revision == Some(snapshot.revision)
            && self.last_keymap_focus == Some(focus)
            && self.last_keymap_backend == Some(backend)
        {
            return;
        }

        let bindings = crate::gpui_actions::key_bindings_for_snapshot(
            &snapshot,
            focus,
            backend,
            &self.state.command_catalog(),
        );
        let hints = bindings.command_hints(&self.state.command_catalog());
        self.dialog_view.update(cx, |view, cx| {
            view.set_command_keybindings(Some(hints), cx);
        });
        if let Err(error) = crate::gpui_actions::replace_workspace_key_bindings_for_context(
            &self.keymap_context,
            bindings,
            cx,
        ) {
            self.state
                .record_error(format!("load GPUI workspace key bindings: {error:#}"));
        }
        self.last_keymap_revision = Some(snapshot.revision);
        self.last_keymap_focus = Some(focus);
        self.last_keymap_backend = Some(backend);
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
                renderer_metrics: self.frame_metrics,
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

    fn invoke_gpui_command(
        &mut self,
        invocation: CommandInvocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.tools_scope == Some(self.state.mux_scope())
            && self
                .state
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
            };
            self.set_workspace_context(scope, context, false, window, cx);
        }
        self.tools_visible = true;
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
        self.tools_visible = true;
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
            self.tools_scope = Some(scope);
            return;
        }
        let owner = cx.weak_entity();
        let tools = cx.new(|cx| {
            crate::gpui_dock::WorkspaceDock::new(
                context,
                owner,
                self.terminal.clone(),
                self.chrome_view.clone(),
                scope,
                sender,
                &path,
                key,
                local_git,
                self.state.config().panels.clone(),
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
        self.tools_scope = Some(scope);
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

    fn finish_document_exit(&mut self, quit: bool, window: &Window, cx: &Context<Self>) {
        if quit {
            // Each workspace's quit observer awaits its own forwarding cleanup.
            cx.quit();
        } else {
            let cleanup = self.close_link_forwards(cx);
            cx.spawn_in(window, async move |weak, cx| {
                cleanup.await;
                _ = weak.update_in(cx, |_, window, _| window.remove_window());
            })
            .detach();
        }
    }

    fn apply_chrome_intent(&mut self, intent: ChromeIntent, cx: &mut Context<Self>) {
        if intent == ChromeIntent::StartWindowDrag {
            self.pending_window_move = true;
        } else {
            self.pending_effects
                .extend(chrome_frame::apply(&mut self.state, intent));
        }
        cx.notify();
    }

    fn apply_settings_intent(&mut self, intent: SettingsIntent, cx: &mut Context<Self>) {
        let close = matches!(intent, SettingsIntent::Close);
        let open_keymap = matches!(&intent, SettingsIntent::Invoke(id) if id == "keymap:open");
        let open_config = matches!(&intent, SettingsIntent::Invoke(id) if id == "config:edit");
        if let SettingsIntent::Invoke(id) = &intent {
            if id == "config:reload" {
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
        let changed = self.settings_view.update(cx, |view, cx| {
            view.reconcile(
                &self.state,
                &self.unsupported_sources,
                &self.integration_rows,
                cx,
            )
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
                AppEffect::SetDecorations(decorated) => window.request_decorations(if decorated {
                    crate::platform::window_decorations(&self.state.config().window)
                } else {
                    WindowDecorations::Client
                }),
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
                    self.terminal_base_cell = terminal_cell_metrics(&config, window);
                    self.terminal_cell = self.terminal_base_cell;
                    self.terminal_text_contract = Arc::new(TerminalTextContract::new(
                        config.clone(),
                        NativeSymbolPolicy::default(),
                    ));
                    self.terminal_text = config;
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
                AppEffect::FocusTerminal => {
                    cx.activate(true);
                    window.activate_window();
                    self.focus.focus(window, cx);
                }
                AppEffect::SetWindowFocus => window.activate_window(),
                AppEffect::ApplyMacosNonNativeFullscreen => {
                    Self::apply_simple_fullscreen(true, window);
                }
                AppEffect::RestoreMacosPresentation => {
                    Self::apply_simple_fullscreen(false, window);
                }
                AppEffect::OpenUrl(url) => cx.open_url(&url),
                AppEffect::Dock(request) => self.apply_dock_request(request, window, cx),
                AppEffect::OpenGitChanges {
                    scope,
                    target,
                    directory,
                    host,
                } => self.open_git_changes(scope, target, directory, host, window, cx),
                AppEffect::OpenFiles(request) => self.open_files(request, window, cx),
                AppEffect::OpenSettings => self.open_settings_window(window, cx),
                AppEffect::OpenSetting(id) => {
                    self.open_settings_window_target(SettingsWindowTarget::Setting(id), window, cx);
                }
                AppEffect::CommandAction(action) => {
                    self.dialog_view
                        .update(cx, |view, cx| view.perform(action, window, cx));
                }
                AppEffect::ConfigureKeybind(action) => {
                    self.open_keymap_window(Some(action), window, cx);
                }
            }
        }
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
        };
        if reuse {
            self.tools_visible = true;
        } else {
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
            },
            true,
            window,
            cx,
        );
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
                renderer_metrics: self.frame_metrics,
                terminal_cell_width: cell.width,
                terminal_cell_height: cell.height,
                terminal_scale_factor: window.scale_factor(),
                terminal_view_transform: bootty_terminal::geometry::ViewTransform::default(),
            },
        );
        let mut effects = self.state.update_frame(frame_inputs);
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

    fn terminal_frames_changed(&mut self, cx: &App) -> bool {
        if self.state.uses_native_terminal_layout() {
            for (pane_id, _) in &self.pane_hit_rects {
                let key = self.state.pane_widget_key(pane_id);
                let Some(runtime) = self
                    .state
                    .workspace
                    .active
                    .binding
                    .visible_terminal_runtime(pane_id)
                else {
                    return true;
                };
                match runtime.extract_frame() {
                    Ok(frame) => {
                        if self
                            .terminal_panes
                            .get(&key)
                            .is_none_or(|view| !view.read(cx).presents_frame(&frame))
                        {
                            return true;
                        }
                    }
                    Err(error) => {
                        self.state.record_error(error);
                        return true;
                    }
                }
            }
            false
        } else {
            match self.state.terminal_mut().extract_frame() {
                Ok(frame) => !self.terminal.read(cx).presents_frame(&frame),
                Err(error) => {
                    self.state.record_error(error);
                    true
                }
            }
        }
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

    fn poll_settings_runtime(&mut self, cx: &mut Context<Self>) -> bool {
        let mut settings_changed = false;
        if let Some(catalog) = self.settings_runtime.drain_catalog() {
            settings_changed = true;
            self.unsupported_sources = catalog.unsupported_sources;
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
        let error = self.state.last_error();
        let frame_changed = self.advance_frame(window, cx);
        let mut changed = if crate::menu::settings_requested(window.is_window_active()) {
            self.open_settings_window(window, cx);
            true
        } else {
            frame_changed
        };
        self.sync_agents(cx);
        if self.poll_settings_runtime(cx) {
            changed = true;
            if self.settings_window.is_some() || self.settings_window_opening {
                self.refresh_settings(cx);
            }
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
        let usage_visible = self.tools.as_ref().is_some_and(|tools| {
            tools
                .read(cx)
                .panel_visible(bootty_config::config::PanelKind::Agents, cx)
        });
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
        // Read-only control requests and unchanged backend results still complete while a
        // window is occluded. Only changed presentation needs a frame, coalesced until paint.
        if changed && !self.work_repaint_pending {
            self.work_repaint_pending = true;
            cx.notify();
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
                    Notification::error(error)
                        .id::<BoottyErrorNotification>()
                        .title("Something went wrong")
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
        match projection {
            Some(crate::presentation::dialogs::DialogProjection::Dialog(spec)) => {
                self.present_dialog(*spec, window, cx);
            }
            Some(crate::presentation::dialogs::DialogProjection::SpaceEditor(snapshot)) => {
                self.present_space_editor(*snapshot, colors, window, cx);
            }
            None => {
                self.dialog_view
                    .update(cx, |view, cx| view.present(None, window, cx));
                if self.root_dialog_kind.is_some() && window.has_active_dialog(cx) {
                    let focus_terminal = self.state.terminal_focused();
                    window.close_dialog(cx);
                    if focus_terminal {
                        // A completed picker can select a new terminal while Root still remembers
                        // the old trigger. Restore focus to the current terminal after dismissal.
                        schedule_focus(self.focus.clone(), window, cx);
                    }
                }
                if self.overlay_kind.is_some() {
                    self.overlay_host.update(cx, |host, cx| {
                        host.clear(window, cx);
                    });
                    self.overlay_kind = None;
                }
                self.root_dialog_kind = None;
                self.root_dialog_key = None;
            }
        }
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
        if let Some(empty_terminal) = self.empty_terminal_state() {
            self.pane_hit_rects.clear();
            self.terminal_interactions.clear();
            dock.update(cx, |dock, cx| {
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

    fn present_dialog(
        &mut self,
        spec: crate::gpui::DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dialog_view.update(cx, |view, cx| {
            view.present(Some(spec), window, cx);
        });
        if self.dialog_view.read(cx).is_non_modal() {
            if self.root_dialog_kind.is_some() && window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
            self.root_dialog_kind = None;
            self.root_dialog_key = None;
            if self.overlay_kind != Some(OverlayKind::Dialog) {
                self.overlay_host.update(cx, |host, cx| {
                    host.present(self.dialog_view.clone(), window, cx);
                });
                self.overlay_kind = Some(OverlayKind::Dialog);
            }
            return;
        }
        // Modal dialogs use gpui-component's Root so scrim, focus trapping, backdrop
        // dismissal, and Escape all share one implementation. The legacy overlay host
        // remains only for anchored TerminalFind.
        if self.overlay_kind.is_some() {
            self.overlay_host.update(cx, |host, cx| {
                host.clear(window, cx);
            });
            self.overlay_kind = None;
        }
        let root_title = self.dialog_view.read(cx).root_title();
        let dialog_key = self
            .dialog_view
            .read(cx)
            .root_id()
            .map(|id| (id.0, root_title.clone()));
        if self.root_dialog_kind != Some(RootDialogKind::Dialog)
            || self.root_dialog_key != dialog_key
            || !window.has_active_dialog(cx)
        {
            if self.root_dialog_kind.is_some() && window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
            let dialog_view = self.dialog_view.clone();
            let show_root_chrome = root_title.is_some();
            let workspace = cx.weak_entity();
            window.open_dialog(cx, move |dialog, window, _| {
                let content_view = dialog_view.clone();
                dialog
                    .w(px(f32::from(window.rem_size()) * 37.5))
                    .max_w(px(f32::from(window.rem_size()) * 45.0))
                    .when(!show_root_chrome, |dialog| dialog.p_0().gap_0())
                    .when_some(
                        root_title.clone(),
                        gpui_kit::component::dialog::Dialog::title,
                    )
                    .close_button(show_root_chrome)
                    .on_cancel({
                        let workspace = workspace.clone();
                        move |_, _, cx| {
                            let _ = workspace.update(cx, |workspace, cx| {
                                workspace.state.close_overlay_dialogs();
                                cx.notify();
                            });
                            true
                        }
                    })
                    .content(move |content, _, _| {
                        content
                            .when(!show_root_chrome, gpui_kit::Styled::p_0)
                            .child(content_view.clone())
                    })
            });
            self.root_dialog_kind = Some(RootDialogKind::Dialog);
            self.root_dialog_key = dialog_key;
            schedule_focus(self.dialog_view.focus_handle(cx), window, cx);
        }
    }

    fn present_space_editor(
        &mut self,
        snapshot: crate::gpui::SpaceEditorSnapshot,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dialog_view
            .update(cx, |view, cx| view.present(None, window, cx));
        if self.overlay_kind.is_some() {
            self.overlay_host.update(cx, |host, cx| {
                host.clear(window, cx);
            });
            self.overlay_kind = None;
        }
        let opening_root_dialog = self.root_dialog_kind != Some(RootDialogKind::SpaceEditor)
            || !window.has_active_dialog(cx);
        if opening_root_dialog && self.root_dialog_kind.is_some() && window.has_active_dialog(cx) {
            window.close_dialog(cx);
        }
        let mut snapshot = snapshot;
        let root_title = snapshot.title.clone();
        snapshot.colors = space_editor_colors(colors);
        let entity = if let Some(entity) = &self.space_editor_view {
            entity.update(cx, |view, cx| view.set_snapshot(snapshot, cx));
            entity.clone()
        } else {
            let entity = cx.new(|cx| GpuiSpaceEditor::new_with_window(snapshot, window, cx));
            let subscription = cx.subscribe(&entity, |this, _, intent: &SpaceEditorIntent, cx| {
                this.state.apply_space_editor_ui_intent(intent.clone());
                cx.notify();
            });
            self.space_editor_view = Some(entity.clone());
            self.space_editor_subscription = Some(subscription);
            entity
        };
        if opening_root_dialog {
            let workspace = cx.weak_entity();
            let editor = entity.clone();
            window.open_dialog(cx, move |dialog, window, app| {
                let editor = editor.clone();
                let workspace = workspace.clone();
                dialog
                    .w(px(f32::from(window.rem_size()) * 37.5))
                    .max_w(px(f32::from(window.rem_size()) * 45.0))
                    .title(root_title.clone())
                    .footer(GpuiSpaceEditor::render_dialog_footer(editor.clone(), app))
                    .on_cancel(move |_, _, cx| {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.state.close_overlay_dialogs();
                            cx.notify();
                        });
                        true
                    })
                    .content(move |content, _, _| content.p_0().child(editor.clone()))
            });
            let editor = entity;
            cx.defer_in(window, move |_, window, cx| {
                editor.update(cx, |editor, cx| editor.focus(window, cx));
            });
            self.root_dialog_kind = Some(RootDialogKind::SpaceEditor);
            self.root_dialog_key = None;
        }
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
        self.terminal_pane_subscriptions
            .retain(|key, _| live_keys.contains(key));
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
        let data = panel.read(cx);
        if data.active
            && let Some(bounds) = data.bounds
        {
            let panel_area = SurfaceRect {
                min_x: bounds.left().into(),
                min_y: bounds.top().into(),
                max_x: bounds.right().into(),
                max_y: bounds.bottom().into(),
            };
            self.terminal_element(window, panel_area, colors, cx);
        }
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
            let geometry = TerminalTextGeometry::fitted(
                &self.terminal_text,
                panel_area.width(),
                panel_area.height(),
                self.terminal_base_cell,
                terminal_content_padding(window),
            );
            self.terminal_cell = geometry.grid_cell;
            let surface = TerminalSurface::new(
                panel_area,
                geometry.grid_cell,
                terminal_content_padding(window),
            );
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
                .map(|window| {
                    window
                        .panes
                        .iter()
                        .filter_map(|pane| pane.pane_id.clone())
                        .collect()
                })
                .unwrap_or_default();
            panel.update(cx, |panel, cx| {
                panel.prepare(surface.geometry(), pane_ids, window, cx);
            });
            let contract = self.terminal_text_contract.clone();
            let snapshot = self.native_terminal_snapshot(
                window,
                &id,
                panel_area,
                geometry,
                self.terminal_text.font_size,
                &contract,
                colors,
                cx,
            );
            let title = windows
                .iter()
                .find(|candidate| candidate.id == id)
                .map(|candidate| candidate.title.clone())
                .unwrap_or_default();
            panel.update(cx, |panel, cx| panel.publish(title, snapshot, cx));
        }
    }

    fn prepare_dock_terminals(
        &mut self,
        dock: &Entity<crate::gpui_dock::WorkspaceDock>,
        window: &mut Window,
        colors: Colors,
        cx: &mut Context<Self>,
    ) {
        let binding = &self.state.workspace.active.binding;
        let selected = binding.current_window_id();
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
        if binding.mux().has_session_snapshot() {
            dock.update(cx, |dock, cx| {
                dock.sync_terminals(&windows, &selected, window, cx);
            });
        }
        self.pane_hit_rects.clear();
        self.terminal_interactions.clear();
        self.retain_live_terminal_panes();
        // Dock geometry belongs to the terminal surface; mux owns the split ratios.
        let panel = dock.read(cx).terminal_panel();
        self.prepare_dock_panel(&panel, &windows, window, colors, cx);
    }

    fn move_terminal_pointer(
        &mut self,
        event: &gpui_kit::MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_pointer_cursor(event.position, event.modifiers);
        if let Some((start, invocation)) = &mut self.pending_link_click {
            let dx = f32::from(event.position.x) - f32::from(start.x);
            let dy = f32::from(event.position.y) - f32::from(start.y);
            if f32::mul_add(dy, dy, dx * dx) > 16. {
                *invocation = None;
            }
            cx.notify();
            return;
        }
        self.record_mouse_input_target_at(event.position);
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
                .interaction_at(event.position)
                .is_some_and(|(_, interaction)| interaction.view_transform().is_zoomed())
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
        self.record_mouse_input_target_at(event.position);
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
            || self.interaction_at(position).is_none()
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
        if event.button == MouseButton::Left && self.begin_link_click(event) {
            cx.stop_propagation();
            return;
        }
        self.terminal_mouse_buttons.insert(event.button);
        self.record_mouse_input_target_at(event.position);
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

    fn terminal_element(
        &mut self,
        window: &mut Window,
        area: SurfaceRect,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let padding = terminal_content_padding(window);
        let geometry = TerminalTextGeometry::fitted(
            &self.terminal_text,
            area.width(),
            area.height(),
            self.terminal_base_cell,
            padding,
        );
        self.terminal_cell = geometry.grid_cell;
        let font_size = self.terminal_text.font_size;
        let contract = Arc::clone(&self.terminal_text_contract);
        if self.state.uses_native_terminal_layout() {
            return Some(self.native_terminal_element(
                window, area, geometry, font_size, &contract, colors, cx,
            ));
        }

        // Native panes own one focus handle and one interaction map per pane. Drop both before
        // returning to the attached/runtime presentation so a backend switch cannot route the
        // next pointer event through a removed pane or leave keyboard focus on its child view.
        let terminal_focus = self.terminal.focus_handle(cx);
        let native_pointer_state_was_active = self.focus != terminal_focus
            || !self.pane_hit_rects.is_empty()
            || !self.terminal_panes.is_empty();
        if native_pointer_state_was_active {
            self.terminal_mouse_buttons.clear();
            self.pending_link_click = None;
        }
        if self.focus != terminal_focus {
            self.focus = terminal_focus.clone();
            schedule_focus(terminal_focus, window, cx);
        }
        self.pane_hit_rects.clear();
        self.terminal_panes.clear();
        self.terminal_pane_subscriptions.clear();
        self.terminal_interactions.clear();
        let surface = TerminalSurface::new(area, geometry.grid_cell, padding);
        let transition_key = self.state.terminal_transition_key();
        let dim_inactive_cursor = self.state.config().cursor.dim_inactive_pane;
        let scrollbar_mode = self.state.config().session.scrollbar;
        let background_opacity = self.state.config().window.background_opacity;
        self.terminal.update(cx, |terminal, cx| {
            terminal.set_scrollbar_mode(scrollbar_mode, cx);
            terminal.set_background_opacity(background_opacity, cx);
        });
        let result = terminal_element_for_runtime(
            &self.terminal,
            self.state.terminal_mut(),
            transition_key,
            surface,
            window.scale_factor(),
            font_size,
            geometry.ink_cell.height,
            Arc::clone(&contract),
            true,
            dim_inactive_cursor,
            cx,
        );
        match result {
            Ok((element, interaction, metrics)) => {
                self.frame_metrics = metrics;
                self.state.record_surface(surface);
                if let Some(interaction) = interaction {
                    self.terminal_interactions
                        .insert(String::new(), interaction);
                }
                Some(element.into_any_element())
            }
            Err(error) => {
                self.state.record_render_error(error);
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn native_terminal_element(
        &mut self,
        window: &mut Window,
        area: SurfaceRect,
        geometry: TerminalTextGeometry,
        font_size: f32,
        contract: &Arc<TerminalTextContract>,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.pane_hit_rects.clear();
        self.terminal_interactions.clear();
        let window_id = self.state.workspace.active.binding.current_window_id();
        self.retain_live_terminal_panes();
        let snapshot = self.native_terminal_snapshot(
            window, &window_id, area, geometry, font_size, contract, colors, cx,
        );
        let weak = cx.entity().downgrade();
        GpuiPaneWorkspace::new(snapshot, move |intent, window, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.apply_pane_intent(intent, window, cx);
                cx.notify();
            });
        })
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn native_terminal_snapshot(
        &mut self,
        window: &mut Window,
        window_id: &bootty_mux::workspace::ScopedWindowId,
        area: SurfaceRect,
        geometry: TerminalTextGeometry,
        font_size: f32,
        contract: &Arc<TerminalTextContract>,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> GpuiPaneWorkspaceSnapshot<CachedTerminalView> {
        let cell = geometry.grid_cell;
        let config = self.state.config();
        let gap = config.chrome.pane_divider_width;
        let focused = self.state.focused_pane();
        let window_focused = window.is_window_active();
        let dim_inactive_cursor = config.cursor.dim_inactive_pane;
        let scrollbar_mode = config.session.scrollbar;
        let background_opacity = config.window.background_opacity;
        let layout = self
            .state
            .workspace
            .active
            .binding
            .window_pane_layout(window_id.session_id(), window_id.window_id())
            .cloned();
        let rects = layout
            .as_ref()
            .map(|layout| layout.rects(area, gap))
            .unwrap_or_default();
        if *window_id == self.state.workspace.active.binding.current_window_id() {
            self.state.record_pane_area(area);
        }
        self.pane_hit_rects.extend(rects.iter().cloned());

        let pane_surfaces = rects
            .iter()
            .map(|(pane_id, rect)| {
                (
                    pane_id.clone(),
                    *rect,
                    TerminalSurface::new(*rect, cell, terminal_content_padding(window)),
                )
            })
            .collect::<Vec<_>>();
        self.resize_native_pane_window(window_id, layout.as_ref(), &pane_surfaces);

        let mut panes = Vec::with_capacity(pane_surfaces.len());
        self.frame_metrics = RendererMetrics::default();
        for (pane_id, rect, surface) in pane_surfaces {
            let is_focused = focused.as_deref() == Some(pane_id.as_str());
            let key = self.state.pane_widget_key(&pane_id);
            let terminal_view = self.ensure_terminal_pane(&pane_id, &key, window, cx);
            terminal_view.update(cx, |terminal, cx| {
                terminal.set_window_focused(window_focused, cx);
                terminal.set_scrollbar_mode(scrollbar_mode, cx);
                terminal.set_background_opacity(background_opacity, cx);
            });
            if is_focused {
                let pane_focus = terminal_view.focus_handle(cx);
                if self.focus != pane_focus {
                    self.focus = pane_focus.clone();
                    schedule_focus(pane_focus, window, cx);
                }
            }
            // A visible Dock pane can differ from the binding's current input target.
            let Some(runtime) = self
                .state
                .workspace
                .active
                .binding
                .visible_terminal_runtime(&pane_id)
            else {
                continue;
            };
            let result = terminal_element_for_runtime(
                &terminal_view,
                runtime,
                Some(key),
                surface,
                window.scale_factor(),
                font_size,
                geometry.ink_cell.height,
                Arc::clone(contract),
                is_focused,
                dim_inactive_cursor,
                cx,
            );
            match result {
                Ok((terminal, interaction, metrics)) => {
                    if is_focused {
                        self.frame_metrics = metrics;
                        self.state.record_surface(surface);
                    }
                    if let Some(interaction) = interaction {
                        self.terminal_interactions
                            .insert(pane_id.clone(), interaction);
                    }
                    panes.push(GpuiPaneSnapshot {
                        id: pane_id.clone(),
                        rect: pane_rect(rect),
                        terminal,
                        focused: is_focused,
                        progress: self.state.pane_progress(&pane_id).map(pane_progress),
                    });
                }
                Err(error) => self.state.record_render_error(error),
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
        let dividers = layout
            .map(|layout| layout.dividers(area, gap))
            .unwrap_or_default();
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
        if let Some(view) = self.terminal_panes.get(key) {
            view.clone()
        } else {
            let view = cx.new(GpuiTerminalView::new);
            let subscription = cx.subscribe_in(
                &view,
                window,
                |this, _, input: &TerminalViewInput, window, cx| {
                    this.apply_terminal_view_input(input.0.clone(), window, cx);
                },
            );
            self.terminal_panes.insert(key.to_owned(), view.clone());
            let scroll_pane = pane_id.to_owned();
            let scroll_subscription =
                cx.subscribe(&view, move |this, _, input: &TerminalScrollbarInput, cx| {
                    this.scroll_terminal(Some(&scroll_pane), input, cx);
                });
            let [focus_in, focus_out] =
                Self::subscribe_terminal_focus(&view.focus_handle(cx), window, cx);
            self.terminal_pane_subscriptions.insert(
                key.to_owned(),
                vec![subscription, scroll_subscription, focus_in, focus_out],
            );
            view
        }
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
        panes: Vec<GpuiPaneSnapshot<CachedTerminalView>>,
        dividers: Vec<GpuiPaneDividerSnapshot>,
        colors: Colors,
        window: &Window,
        cx: &Context<Self>,
    ) -> GpuiPaneWorkspaceSnapshot<CachedTerminalView> {
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
        if let Some(pane) = self.state.focused_pane() {
            let key = self.state.pane_widget_key(&pane);
            if let Some(view) = self.terminal_panes.get(&key) {
                self.focus = view.focus_handle(cx);
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
                self.focus_terminal_window(target, id, window, cx);
                self.state.focus_pane(&pane);
                let key = self.state.pane_widget_key(&pane);
                if let Some(view) = self.terminal_panes.get(&key) {
                    self.focus = view.focus_handle(cx);
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

    fn interaction_at(
        &self,
        position: gpui_kit::Point<gpui_kit::Pixels>,
    ) -> Option<(String, GpuiTerminalInteraction)> {
        let point = bootty_terminal::geometry::SurfacePoint {
            x: position.x.into(),
            y: position.y.into(),
        };
        let key = self
            .pane_hit_rects
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map_or("", |(pane_id, _)| pane_id.as_str());
        self.terminal_interactions
            .get(key)
            .cloned()
            .map(|interaction| (key.to_owned(), interaction))
    }

    fn record_mouse_input_target_at(&mut self, position: gpui_kit::Point<gpui_kit::Pixels>) {
        if let Some((pane_id, interaction)) = self.interaction_at(position) {
            let pane_id = (!pane_id.is_empty()).then_some(pane_id);
            self.state.record_mouse_input_target_for_pane(
                pane_id,
                interaction.surface(),
                interaction.view_transform(),
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
        &mut self,
        position: gpui_kit::Point<Pixels>,
        transform: impl FnOnce(
            bootty_terminal::geometry::ViewTransform,
            SurfaceRect,
        ) -> bootty_terminal::geometry::ViewTransform,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((pane_id, interaction)) = self.interaction_at(position) else {
            return false;
        };
        let view = transform(interaction.view_transform(), interaction.surface().rect);
        let terminal = if pane_id.is_empty() {
            Some(self.terminal.clone())
        } else {
            self.terminal_panes
                .get(&self.state.pane_widget_key(&pane_id))
                .cloned()
        };
        if let Some(terminal) = terminal {
            terminal.update(cx, |terminal, cx| terminal.set_view_transform(view, cx));
            self.terminal_interactions
                .insert(pane_id, interaction.with_view_transform(view));
            cx.notify();
        }
        true
    }

    fn focus_pointer_target_at(
        &mut self,
        position: gpui_kit::Point<gpui_kit::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.uses_native_terminal_layout() {
            window.focus(&self.focus, cx);
            return;
        }

        let Some((pane_id, _)) = self.interaction_at(position) else {
            return;
        };
        self.state.focus_pane(&pane_id);
        let key = self.state.pane_widget_key(&pane_id);
        let Some(terminal_view) = self.terminal_panes.get(&key).cloned() else {
            return;
        };
        let focus = terminal_view.focus_handle(cx);
        self.focus = focus.clone();
        window.focus(&focus, cx);
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
        self.record_mouse_input_target_at(event.position);
        self.input.mouse_up(event);
        cx.notify();
    }

    fn begin_link_click(&mut self, event: &gpui_kit::MouseDownEvent) -> bool {
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
        let Some((_, interaction)) = self.interaction_at(event.position) else {
            return false;
        };
        let Some(link) = interaction.hyperlink_at(point) else {
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
    ) {
        let point = bootty_terminal::geometry::SurfacePoint {
            x: position.x.into(),
            y: position.y.into(),
        };
        self.cursor = self
            .interaction_at(position)
            .and_then(|(_, interaction)| {
                let activation_modifier = if cfg!(target_os = "macos") {
                    modifiers.platform
                } else {
                    modifiers.control
                };
                (activation_modifier && interaction.hyperlink_at(point).is_some())
                    .then_some(CursorStyle::PointingHand)
            })
            .unwrap_or(self.terminal_cursor);
    }
}

#[allow(clippy::too_many_arguments)]
fn terminal_element_for_runtime<T: TerminalFrameSource + ?Sized>(
    terminal_view: &gpui_kit::Entity<GpuiTerminalView>,
    terminal: &mut T,
    transition_key: Option<String>,
    surface: TerminalSurface,
    display_scale: f32,
    font_size: f32,
    text_cell_height: f32,
    contract: Arc<TerminalTextContract>,
    animate_cursor: bool,
    dim_inactive_cursor: bool,
    cx: &mut Context<GpuiWorkspace>,
) -> Result<(
    CachedTerminalView,
    Option<GpuiTerminalInteraction>,
    RendererMetrics,
)> {
    terminal.set_display_scale(display_scale)?;
    terminal.set_render_cell_metrics(surface.cell)?;
    terminal.resize(surface.geometry())?;
    let frame = terminal.extract_frame()?;
    if !terminal_view.read(cx).matches_presentation(
        transition_key.as_deref(),
        surface,
        &frame,
        font_size,
        text_cell_height,
        display_scale,
        &contract,
        animate_cursor,
        dim_inactive_cursor,
    ) {
        let terminal_view = terminal_view.clone();
        cx.defer(move |cx| {
            terminal_view.update(cx, |view, cx| {
                view.update(
                    transition_key,
                    surface,
                    frame,
                    font_size,
                    text_cell_height,
                    display_scale,
                    contract,
                    animate_cursor,
                    dim_inactive_cursor,
                    cx,
                );
            });
        });
    }
    Ok((
        CachedTerminalView(terminal_view.clone()),
        terminal_view.read(cx).interaction(),
        terminal_view.read(cx).metrics(),
    ))
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
        self.work_repaint_pending = false;
        #[expect(
            clippy::float_cmp,
            reason = "An exact platform display-scale change invalidates cached cell metrics"
        )]
        let display_scale_changed = self.terminal_display_scale != window.scale_factor();
        if display_scale_changed {
            self.terminal_display_scale = window.scale_factor();
            self.terminal_base_cell = terminal_cell_metrics(&self.terminal_text, window);
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
        if crate::menu::settings_requested(window.is_window_active()) {
            self.open_settings_window(window, cx);
        }
        // Child-only paints (for example cursor blink) do not advance application work.
        if std::mem::take(&mut self.frame_update_pending)
            || !self.pending_effects.is_empty()
            || self.state.commands.has_queued()
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

    fn decorate_dock_chrome(
        &self,
        chrome_snapshot: &mut ChromeSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tools.is_some() {
            if let Some(tools) = &self.tools {
                tools.update(cx, |tools, cx| {
                    tools.sync_panel_settings(&self.state.config().panels, window, cx);
                });
            }
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
    ) -> (ChromeSnapshot, f32) {
        let viewport = self.workspace_bounds.size;
        let viewport_height: f32 = viewport.height.into();
        let usage_visible = self.tools.as_ref().is_some_and(|tools| {
            tools
                .read(cx)
                .panel_visible(bootty_config::config::PanelKind::Agents, cx)
        });
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
        let tools_visible = self.tools_visible && !docked_terminals;
        let tools_width = if tools_visible {
            (f32::from(window.rem_size()) * 20.).min(viewport_width * 0.45)
        } else {
            0.0
        };
        let mut chrome_snapshot = chrome_frame::snapshot(
            &self.state,
            &self.launch.native_chrome.borrow(),
            &projection,
            viewport_width - tools_width,
            viewport_height,
        );
        self.last_maintenance_chrome = Some(if tools_width == 0.0 {
            chrome_snapshot.clone()
        } else {
            chrome_frame::snapshot(
                &self.state,
                &self.launch.native_chrome.borrow(),
                &projection,
                viewport_width,
                viewport_height,
            )
        });
        self.decorate_dock_chrome(&mut chrome_snapshot, window, cx);
        self.sync_key_bindings(window, cx);
        self.chrome_view.update(cx, |chrome, cx| {
            chrome.set_docked_status(docked_terminals, cx);
        });
        self.chrome_view.update(cx, |chrome, cx| {
            chrome.update(&chrome_snapshot, window, cx);
        });
        (chrome_snapshot, tools_width)
    }

    fn sync_settings_window(
        &mut self,
        settings_changed: bool,
        config_revision: u64,
        cx: &mut Context<Self>,
    ) {
        let settings_open = self.settings_window.is_some() || self.settings_window_opening;
        if settings_open {
            if self.last_config_file_editor_revision != Some(config_revision) {
                self.last_config_file_editor_revision = Some(config_revision);
                if let Some(settings_window) = self.settings_window.clone() {
                    cx.defer(move |cx| {
                        let _ = settings_window.update_in(cx, |root, window, cx| {
                            root.reconcile_file_editor(EditorFileKind::Config, window, cx);
                        });
                    });
                }
            }
            if settings_changed || self.last_settings_revision != Some(config_revision) {
                self.last_settings_revision = Some(config_revision);
                self.refresh_settings(cx);
            }
        } else {
            self.last_config_file_editor_revision = None;
            self.last_settings_revision = None;
        }
        let keymap_open = self.settings_window.is_some() || self.settings_window_opening;
        if keymap_open {
            let revision = self.state.keymap_snapshot().revision;
            if self.last_keymap_editor_revision != Some(revision) {
                self.last_keymap_editor_revision = Some(revision);
                self.publish_keymap_editor_snapshot(cx);
            }
        } else {
            self.last_keymap_editor_revision = None;
        }
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
        if window.has_active_dialog(cx) || self.state.modal_dialog().is_some() {
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
        if window.has_active_dialog(cx) && !action.invocation().command.starts_with("ui.command.") {
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
        tools_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let tools_visible = self.tools_visible && self.tools.is_none();
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
            .on_mouse_move(
                cx.listener(move |this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    let point = bootty_terminal::geometry::SurfacePoint {
                        x: f32::from(event.position.x) - f32::from(workspace_origin.x),
                        y: f32::from(event.position.y) - f32::from(workspace_origin.y),
                    };
                    if this.terminal_mouse_buttons.is_empty() || terminal_area.contains(point) {
                        return;
                    }
                    this.update_pointer_cursor(event.position, event.modifiers);
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
                workspace.child(
                    self.chrome_view
                        .clone()
                        .cached(gpui_kit::StyleRefinement::default().absolute().size_full()),
                )
            })
            .when(tools_visible, |workspace| {
                workspace.children(self.tools.clone().map(|tools| {
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .w(px(tools_width))
                        .h_full()
                        .overflow_hidden()
                        .bg(colors.mantle)
                        .child(tools)
                }))
            })
            .child(self.overlay_host.clone())
    }
}

impl Render for GpuiWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_frame_metrics(window, cx);
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        self.prepare_frame_state(window, cx);
        let config_revision = self.state.config_revision();
        let settings_changed = self.poll_settings_runtime(cx);
        let (chrome_snapshot, tools_width) = self.prepare_chrome_frame(window, cx);
        let docked_terminals = self.tools.is_some();
        let colors = Colors::from_state(&self.state);
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
        let terminal_surface = if let Some(dock) = self.tools.clone() {
            self.docked_terminal_surface(dock, window, terminal_area, colors, cx)
        } else {
            self.terminal_surface(window, terminal_area, colors, cx)
        };
        let workspace = self.workspace_frame(
            terminal_surface,
            terminal_area,
            colors,
            tools_width,
            window,
            cx,
        );

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
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
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

const fn space_editor_colors(colors: Colors) -> SpaceEditorColors {
    SpaceEditorColors {
        pane: colors.pane,
        surface: colors.surface,
        hover: colors.hover,
        border: colors.border,
        text: colors.text,
        muted: colors.muted,
        accent: colors.accent,
        destructive: colors.destructive,
    }
}

fn intent_dialog_id(intent: &DialogIntent) -> &str {
    let id = match intent {
        DialogIntent::Dismiss { dialog }
        | DialogIntent::Activate { dialog, .. }
        | DialogIntent::Preview { dialog, .. }
        | DialogIntent::TextChanged { dialog, .. }
        | DialogIntent::SelectionChanged { dialog, .. }
        | DialogIntent::CycleScope { dialog }
        | DialogIntent::ToggleFavorite { dialog, .. }
        | DialogIntent::Find { dialog, .. }
        | DialogIntent::FocusTerminal { dialog }
        | DialogIntent::FieldChanged { dialog, .. } => dialog,
    };
    &id.0
}
