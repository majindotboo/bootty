//! Native settings window, file tabs, and their save/close lifecycle.

use super::{GpuiWorkspace, schedule_focus};
use crate::gpui::{
    FileEditor, FileEditorEvent, GpuiKeymapEditor, GpuiSettings, KeymapEditorIntent,
    SettingsTitleBar, setup_ui_font,
};
use crate::gpui_keymap_editor::{
    self, editor_snapshot as keymap_editor_snapshot, persisted_edit as persisted_keymap_edit,
};
use gpui_kit::component::{
    Disableable as _, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    tab::{Tab, TabBar},
};
use gpui_kit::{
    Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, TitlebarOptions,
    WeakEntity, Window, WindowBounds, WindowDecorations, WindowKind, WindowOptions, div, point,
    prelude::*, px, size,
};
use std::{path::PathBuf, sync::Arc};

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
pub(super) enum SettingsWindowTarget {
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
pub(super) struct GpuiSettingsWindow {
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
                root.request_close(window, cx);
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
        if let Some(kind) = self.first_dirty_editor(cx) {
            self.prompt_to_close_editor(kind, EditorCloseTarget::Window, window, cx);
        } else {
            window.remove_window();
        }
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
            .map_or_else(Default::default, |tab| {
                tab.reconcile_generation = tab.reconcile_generation.wrapping_add(1);
                tab.reconcile_generation
            });
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
                                root.accept_file_save(kind, &editor, &persisted_contents);
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
                        let _ = root.update(cx, |root, cx| root.reload_keymap(cx));
                    }
                    let _ = root.update(cx, |root, cx| {
                        root.finish_file_save(kind, window, cx);
                    });
                });
            })
            .detach();
    }

    fn accept_file_save(
        &mut self,
        kind: EditorFileKind,
        editor: &Entity<FileEditor>,
        contents: &str,
    ) {
        let Some(tab) = self.editor_tab_mut(kind) else {
            return;
        };
        if tab.editor != *editor {
            return;
        }
        contents.clone_into(&mut tab.original_contents);
        tab.reconcile_generation = tab.reconcile_generation.wrapping_add(1);
    }

    fn reload_keymap(&self, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            let _ = workspace.update_in(cx, |workspace, _, cx| {
                if let Err(error) =
                    gpui_keymap_editor::reload_saved_keymap_text(&mut workspace.state)
                {
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
        if let Some(editor) = active_editor {
            tabs = tabs.suffix(Self::save_editor_button(editor, cx));
        }
        tabs
    }

    fn save_editor_button(editor: &Entity<FileEditor>, cx: &Context<Self>) -> Button {
        let editor_saving = editor.read(cx).save_in_flight();
        let editor_can_save = editor.read(cx).can_save(cx);
        let editor = editor.clone();
        Button::new("settings-window-save-editor")
            .debug_selector(|| "settings-window-save-editor".to_owned())
            .ghost()
            .small()
            .label(if editor_saving { "Saving…" } else { "Save" })
            .disabled(!editor_can_save)
            .on_click(move |_, _, cx| {
                editor.update(cx, FileEditor::request_save);
            })
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

impl GpuiWorkspace {
    pub(super) fn open_settings_window(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.open_settings_window_target(SettingsWindowTarget::Settings, window, cx);
    }

    pub(super) fn open_settings_window_target(
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
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
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
                view
            });
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.settings_window = opened.ok().map(|(_, view)| view.downgrade());
                workspace.settings_window_opening = false;
                cx.notify();
            });
        });
    }

    pub(super) fn close_settings_window(&mut self, cx: &mut Context<Self>) {
        self.settings_window_opening = false;
        let Some(window) = self.settings_window.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = window.update_in(cx, GpuiSettingsWindow::request_close);
        });
    }

    pub(super) fn request_config_editor_tab(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn request_keymap_file_editor_tab(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn request_keymap_window(
        &self,
        requested_action: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_keymap_window(requested_action, window, cx);
            });
        });
    }

    pub(super) fn open_keymap_window(
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

    pub(super) fn close_keymap_window(&self, cx: &mut Context<Self>) {
        let Some(settings_window) = self.settings_window.clone() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = settings_window.update_in(cx, |root, window, cx| {
                root.activate_target(SettingsWindowTarget::Settings, window, cx);
            });
        });
    }

    pub(super) fn publish_keymap_editor_snapshot(&self, cx: &mut Context<Self>) {
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

    pub(super) fn apply_keymap_editor_intent(
        &mut self,
        intent: KeymapEditorIntent,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn sync_settings_window(
        &mut self,
        settings_changed: bool,
        config_revision: u64,
        cx: &mut Context<Self>,
    ) {
        if self.settings_window.is_none() && !self.settings_window_opening {
            self.last_config_file_editor_revision = None;
            self.last_settings_revision = None;
            self.last_keymap_editor_revision = None;
            return;
        }
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
        let revision = self.state.keymap_snapshot().revision;
        if self.last_keymap_editor_revision != Some(revision) {
            self.last_keymap_editor_revision = Some(revision);
            self.publish_keymap_editor_snapshot(cx);
        }
    }
}
