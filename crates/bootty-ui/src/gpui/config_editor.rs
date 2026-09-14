//! Shared GPUI Kit editor surface for settings and host-bound documents.

use std::path::Path;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::input::{Editor as ComponentEditor, EditorState, InputEvent};
use gpui_kit::{
    Context, Entity, EventEmitter, Focusable as _, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, prelude::*,
};

gpui_kit::actions!(
    file_editor,
    [
        #[derive(Eq)]
        Save
    ]
);

/// The pinned Kit input selects text on pointer-down but does not acquire focus.
/// Keep this adapter until Kit handles pointer focus in its input state.
pub fn focus_input<M: gpui_kit::base::input::InputModeKind>(
    state: &Entity<gpui_kit::base::input::InputBaseState<M>>,
    input: impl IntoElement,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let state = state.clone();
    div()
        .id(("input-focus", state.entity_id()))
        .w_full()
        .min_w_0()
        .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
            if !state.read(cx).presentation().is_disabled() {
                state.focus_handle(cx).focus(window, cx);
            }
        })
        .child(input)
}

#[must_use]
pub fn readonly_editor(editor: &Entity<EditorState>, label: &'static str) -> impl IntoElement {
    focus_input(
        editor,
        ComponentEditor::new(editor)
            .readonly(true)
            .bordered(false)
            .size_full()
            .aria_label(label),
    )
    .h_full()
}

pub(super) fn init(cx: &mut gpui_kit::App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        if cfg!(target_os = "macos") {
            "cmd-s"
        } else {
            "ctrl-s"
        },
        Save,
        Some("BoottyConfigEditor"),
    )]);
}

/// Persistence requests emitted to the application-owned file host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileEditorEvent {
    Save { contents: String },
}

/// Shared editor with host-neutral dirty and save state.
pub struct FileEditor {
    editor: Entity<EditorState>,
    saved_contents: String,
    save_in_flight: bool,
    save_allows_close: bool,
    status: Option<SaveStatus>,
    _editor_subscription: Subscription,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SaveStatus {
    Saved,
    Warning(String),
    Error(String),
}

impl FileEditor {
    pub fn new(contents: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_for_path(contents, Path::new("config.toml"), window, cx)
    }

    pub fn new_for_path(
        contents: String,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let language = language_for_path(path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .default_value(contents.clone())
                .language(language)
                .soft_wrap(false)
        });
        let editor_subscription = cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.status = None;
                cx.notify();
            }
        });
        Self {
            editor,
            saved_contents: contents,
            save_in_flight: false,
            save_allows_close: true,
            status: None,
            _editor_subscription: editor_subscription,
        }
    }

    pub fn cursor_position(&self, cx: &gpui_kit::App) -> (u32, u32) {
        let position = self.editor.read(cx).cursor_position();
        (position.line, position.character)
    }

    pub fn set_cursor_position(
        &self,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut gpui_kit::App,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.set_cursor_position(
                gpui_kit::component::input::Position::new(line, column),
                window,
                cx,
            );
        });
    }

    pub fn focus_handle(&self, cx: &gpui_kit::App) -> gpui_kit::FocusHandle {
        self.editor.focus_handle(cx)
    }

    pub fn focus(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        self.editor.focus_handle(cx).focus(window, cx);
    }

    pub fn contents(&self, cx: &gpui_kit::App) -> String {
        self.editor.read(cx).value().to_string()
    }

    pub fn is_dirty(&self, cx: &gpui_kit::App) -> bool {
        self.contents(cx) != self.saved_contents
    }

    #[must_use]
    pub const fn save_in_flight(&self) -> bool {
        self.save_in_flight
    }

    pub fn can_save(&self, cx: &gpui_kit::App) -> bool {
        self.is_dirty(cx) && !self.save_in_flight
    }

    #[must_use]
    pub const fn save_allows_close(&self) -> bool {
        self.save_allows_close
    }

    pub fn request_save(&mut self, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }
        self.save_in_flight = true;
        self.save_allows_close = false;
        self.status = None;
        cx.emit(FileEditorEvent::Save {
            contents: self.contents(cx),
        });
        cx.notify();
    }

    /// Confirm exactly the revision persisted by the host.
    pub fn mark_saved(
        &mut self,
        persisted_contents: String,
        durability_warning: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.saved_contents = persisted_contents;
        self.save_in_flight = false;
        self.save_allows_close = durability_warning.is_none();
        self.status = durability_warning.map_or(Some(SaveStatus::Saved), |warning| {
            Some(SaveStatus::Warning(warning))
        });
        cx.notify();
    }

    pub fn save_failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.save_in_flight = false;
        self.save_allows_close = false;
        self.status = Some(SaveStatus::Error(error));
        cx.notify();
    }

    /// Replace the document with an authoritative host revision.
    pub fn replace_snapshot(
        &mut self,
        contents: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saved_contents.clone_from(&contents);
        self.save_in_flight = false;
        self.save_allows_close = true;
        self.status = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value(contents, window, cx));
        cx.notify();
    }

    fn status_banner(&self) -> Option<impl IntoElement> {
        match self.status.as_ref()? {
            SaveStatus::Saved => None,
            SaveStatus::Warning(message) => {
                Some(Alert::warning("file-save-status", message.clone()).banner())
            }
            SaveStatus::Error(message) => {
                Some(Alert::error("file-save-status", message.clone()).banner())
            }
        }
    }
}

impl EventEmitter<FileEditorEvent> for FileEditor {}

impl Render for FileEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-config-editor")
            .debug_selector(|| "settings-config-editor".to_owned())
            .key_context("BoottyConfigEditor")
            .on_action(cx.listener(|this, _: &Save, _, cx| {
                this.request_save(cx);
                cx.stop_propagation();
            }))
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .when_some(self.status_banner(), |this, status| {
                this.child(div().w_full().px_3().pt_2().child(status))
            })
            .child(
                div().flex_1().min_h_0().child(
                    focus_input(
                        &self.editor,
                        ComponentEditor::new(&self.editor)
                            .bordered(false)
                            .size_full(),
                    )
                    .h_full(),
                ),
            )
    }
}

fn language_for_path(path: &Path) -> &'static str {
    let suffix = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("txt")
        .to_ascii_lowercase();
    match suffix.as_str() {
        "rs" => "rust",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "py" => "python",
        "go" => "go",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" | "zsh" => "bash",
        "lua" | "luau" => "lua",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        _ => "text",
    }
}
