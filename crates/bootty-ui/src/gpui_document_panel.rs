//! Host-bound documents. The host owns bytes; each editor owns a draft and its saved revision.

use crate::{
    gpui::{FileEditor, FileEditorEvent},
    gpui_git_panel::GitPanelContext,
};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_host::files::{FileResponse, FileSnapshot, encode_document};
use gpui_kit::component::{
    Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState, TabGroup},
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Global, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, WeakEntity, Window, div, prelude::*,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

gpui_kit::actions!(
    document,
    [
        #[derive(Eq)]
        CloseDocument
    ]
);
pub struct DocumentClosed;

pub fn init(cx: &mut App) {
    cx.bind_keys([gpui_kit::KeyBinding::new(
        if cfg!(target_os = "macos") {
            "cmd-w"
        } else {
            "ctrl-w"
        },
        CloseDocument,
        Some("BoottyDocument"),
    )]);
}

#[derive(Default)]
pub struct Documents(pub Vec<WeakEntity<DocumentPanel>>);
impl Global for Documents {}

impl Documents {
    pub(crate) fn pending(cx: &App) -> Vec<Entity<DocumentPanel>> {
        cx.try_global::<Self>()
            .map(|documents| {
                documents
                    .0
                    .iter()
                    .filter_map(WeakEntity::upgrade)
                    .filter(|document| document.read(cx).needs_close_prompt(cx))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Visibility, loading, preview and close requests vary independently"
)]
pub struct DocumentPanel {
    context: GitPanelContext,
    host_identity: String,
    path: String,
    sender: BoundAppCommandSender,
    focus: FocusHandle,
    editor: Option<Entity<FileEditor>>,
    digest: Option<String>,
    error: Option<String>,
    external: Option<FileSnapshot>,
    pending: bool,
    active: bool,
    host_visible: bool,
    watch: Option<bootty_host::file_watch::FileWatch>,
    preview: bool,
    line: u32,
    column: u32,
    close_prompt: bool,
    close_after_save: bool,
    pub(crate) group: Option<WeakEntity<TabGroup>>,
    subscriptions: Vec<Subscription>,
}

impl DocumentPanel {
    pub(crate) fn new(
        context: GitPanelContext,
        host_identity: String,
        path: String,
        sender: BoundAppCommandSender,
        position: (u32, u32),
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (line, column) = position;
        if cx.try_global::<Documents>().is_none() {
            cx.set_global(Documents::default());
        }
        cx.global_mut::<Documents>()
            .0
            .retain(|document| document.upgrade().is_some());
        let weak = cx.weak_entity();
        cx.global_mut::<Documents>().0.push(weak);
        if context.host_identity == "local" && host_identity == "local" {
            let watch_path = std::path::PathBuf::from(&path);
            cx.spawn_in(window, async move |weak, cx| {
                let watch = cx
                    .background_executor()
                    .spawn(async move { bootty_host::file_watch::FileWatch::new(&watch_path).ok() })
                    .await;
                _ = weak.update_in(cx, |this, _, _| this.watch = watch);
            })
            .detach();
        }
        cx.spawn_in(window, async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if weak
                    .update_in(cx, |this, window, cx| {
                        if this.active
                            && this.host_visible
                            && this.group.is_some()
                            && (this.error.is_some()
                                || this
                                    .watch
                                    .as_ref()
                                    .is_none_or(bootty_host::file_watch::FileWatch::take_changed))
                        {
                            this.refresh(false, window, cx);
                        }
                        let position = this
                            .editor
                            .as_ref()
                            .map(|editor| editor.read(cx).cursor_position(cx));
                        if let Some((line, column)) = position
                            && (line, column) != (this.line, this.column)
                        {
                            this.line = line;
                            this.column = column;
                            cx.emit(PanelEvent::LayoutChanged);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let mut this = Self {
            context,
            host_identity,
            path,
            sender,
            focus: cx.focus_handle(),
            editor: None,
            digest: None,
            error: None,
            external: None,
            pending: false,
            active: false,
            host_visible: true,
            watch: None,
            preview: false,
            line: line.saturating_sub(1),
            column: column.saturating_sub(1),
            close_prompt: false,
            close_after_save: false,
            group: None,
            subscriptions: Vec::new(),
        };
        this.refresh(false, window, cx);
        this
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    pub(crate) fn host_identity(&self) -> &str {
        &self.host_identity
    }
    pub(crate) fn needs_close_prompt(&self, cx: &App) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).is_dirty(cx) || editor.read(cx).save_in_flight())
    }
    pub(crate) fn saving(&self, cx: &App) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).save_in_flight())
    }
    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            editor.update(cx, FileEditor::request_save);
        }
    }
    pub(crate) fn go_to(
        &mut self,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.line = line.saturating_sub(1);
        self.column = column.saturating_sub(1);
        self.focus_handle(cx).focus(window, cx);
        if let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| {
                editor.set_cursor_position(self.line, self.column, window, cx);
            });
        }
        cx.emit(PanelEvent::LayoutChanged);
    }

    fn request(
        &mut self,
        command: &str,
        args: Vec<String>,
        saved: Option<String>,
        reload: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mut invocation = CommandInvocation::from_action(command, Caller::Internal);
        invocation.target = Some(self.context.target.clone());
        invocation.arguments = args;
        let receiver = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        );
        let receiver = match receiver {
            Ok(receiver) => receiver,
            Err(error) => {
                self.fail(
                    format!("File command unavailable: {error:?}"),
                    saved.is_some(),
                    cx,
                );
                return;
            }
        };
        self.pending = true;
        cx.spawn_in(window, async move |weak, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.pending = false;
                let response = match outcome {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        serde_json::from_value::<FileResponse>(value)
                            .map_err(|error| error.to_string())
                    }
                    Ok(outcome) => Err(crate::commands::command_outcome_message(&outcome)
                        .unwrap_or_else(|| "File command failed".to_owned())),
                    Err(error) => Err(error.to_string()),
                };
                match response {
                    Ok(FileResponse::Document(snapshot)) => {
                        this.receive(snapshot, reload, window, cx);
                    }
                    Ok(FileResponse::Saved {
                        digest,
                        durability_warning,
                    }) => {
                        let Some(saved) = saved else {
                            this.fail("Unexpected save response".to_owned(), false, cx);
                            return;
                        };
                        this.digest = Some(digest);
                        this.error = None;
                        this.external = None;
                        if let Some(editor) = &this.editor {
                            editor.update(cx, |editor, cx| {
                                editor.mark_saved(saved, durability_warning, cx);
                            });
                        }
                        if this.close_after_save {
                            this.close_after_save = false;
                            if this.editor.as_ref().is_some_and(|editor| {
                                !editor.read(cx).is_dirty(cx) && editor.read(cx).save_allows_close()
                            }) {
                                Self::close(cx);
                            }
                        }
                    }
                    Ok(FileResponse::Directory(_) | FileResponse::Location { .. }) => this.fail(
                        "Unexpected directory response".to_owned(),
                        saved.is_some(),
                        cx,
                    ),
                    Err(error) => this.fail(error, saved.is_some(), cx),
                }
                cx.emit(PanelEvent::LayoutChanged);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn fail(&mut self, error: String, saving: bool, cx: &mut Context<Self>) {
        if saving && let Some(editor) = &self.editor {
            editor.update(cx, |editor, cx| editor.save_failed(error.clone(), cx));
        }
        self.close_after_save = false;
        self.error = (!saving).then_some(error);
        cx.notify();
    }

    fn refresh(&mut self, reload: bool, window: &Window, cx: &mut Context<Self>) {
        if self.host_identity != self.context.host_identity {
            self.error = Some(
                "This document belongs to a different host. Open it from that host’s Files panel."
                    .to_owned(),
            );
            return;
        }
        if !self.pending {
            self.request(
                "files.read",
                vec![self.path.clone()],
                None,
                reload,
                window,
                cx,
            );
        }
    }

    fn receive(
        &mut self,
        snapshot: FileSnapshot,
        reload: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !reload && self.digest.as_ref() == Some(&snapshot.digest) {
            self.external = None;
            self.error = None;
            return;
        }
        let contents = match snapshot.contents() {
            Ok(contents) => contents,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        if !reload && self.needs_close_prompt(cx) {
            self.external = Some(snapshot);
            return;
        }
        self.digest = Some(snapshot.digest);
        self.error = None;
        self.external = None;
        if let Some(editor) = &self.editor {
            let (line, column) = editor.read(cx).cursor_position(cx);
            editor.update(cx, |editor, cx| {
                editor.replace_snapshot(contents, window, cx);
            });
            let focus = window.focused(cx);
            editor.update(cx, |editor, cx| {
                editor.set_cursor_position(line, column, window, cx);
            });
            if let Some(focus) = focus {
                focus.focus(window, cx);
            } else {
                window.blur(cx);
            }
            self.line = line;
            self.column = column;
        } else {
            let path = Path::new(&self.path);
            let editor = cx.new(|cx| FileEditor::new_for_path(contents, path, window, cx));
            let subscription = cx.subscribe_in(
                &editor,
                window,
                |this, _, event: &FileEditorEvent, window, cx| {
                    let FileEditorEvent::Save { contents } = event;
                    if this.pending {
                        this.fail(
                            "A file operation is still running; retry Save when it finishes."
                                .to_owned(),
                            true,
                            cx,
                        );
                        return;
                    }
                    let Some(digest) = this.digest.clone() else {
                        this.fail(
                            "Document revision unavailable; reload before saving.".to_owned(),
                            true,
                            cx,
                        );
                        return;
                    };
                    match encode_document(contents) {
                        Ok(encoded) => this.request(
                            "files.save",
                            vec![this.path.clone(), digest, encoded],
                            Some(contents.clone()),
                            false,
                            window,
                            cx,
                        ),
                        Err(error) => this.fail(error.to_string(), true, cx),
                    }
                },
            );
            let observer = cx.observe(&editor, |_, _, cx| {
                cx.emit(PanelEvent::LayoutChanged);
                cx.notify();
            });
            self.subscriptions.extend([subscription, observer]);
            self.editor = Some(editor.clone());
            let focus = window.focused(cx);
            editor.update(cx, |editor, cx| {
                editor.set_cursor_position(self.line, self.column, window, cx);
            });
            if self.focus.is_focused(window) && self.active {
                let focus = editor.read(cx).focus_handle(cx);
                focus.focus(window, cx);
            } else if !self.active {
                if let Some(focus) = focus {
                    focus.focus(window, cx);
                } else {
                    window.blur(cx);
                }
            }
        }
    }

    pub(crate) fn retain_subscription(&mut self, subscription: Subscription) {
        self.subscriptions.push(subscription);
    }

    pub(crate) const fn set_host_visible(&mut self, visible: bool) {
        self.host_visible = visible;
    }

    pub(crate) const fn set_preview(&mut self, preview: bool) {
        self.preview = preview;
    }

    fn request_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.needs_close_prompt(cx) {
            self.refresh(true, window, cx);
            return;
        }
        if self.close_prompt || self.pending {
            return;
        }
        self.close_prompt = true;
        let answer = crate::gpui::prompt(
            crate::i18n::t(cx, "document-discard-reload").as_str(),
            Some(&self.path),
            &[
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "common-reload").into()),
                gpui_kit::PromptButton::Cancel(crate::i18n::t(cx, "common-cancel").into()),
            ],
            window,
            cx,
        );
        cx.spawn_in(window, async move |weak, cx| {
            let answer = answer.await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.close_prompt = false;
                if matches!(answer, Ok(0)) {
                    this.refresh(true, window, cx);
                }
            });
        })
        .detach();
    }

    fn close(cx: &mut Context<Self>) {
        cx.emit(DocumentClosed);
    }

    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving(cx) || self.close_prompt {
            return;
        }
        if !self.needs_close_prompt(cx) {
            Self::close(cx);
            return;
        }
        self.close_prompt = true;
        let answer = crate::gpui::prompt(
            crate::i18n::t(cx, "document-save-before-close").as_str(),
            Some(&self.path),
            &[
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "common-save").into()),
                gpui_kit::PromptButton::Other(crate::i18n::t(cx, "common-discard").into()),
                gpui_kit::PromptButton::Cancel(crate::i18n::t(cx, "common-cancel").into()),
            ],
            window,
            cx,
        );
        cx.spawn_in(window, async move |weak, cx| {
            let answer = answer.await;
            _ = weak.update_in(cx, |this, _, cx| {
                this.close_prompt = false;
                match answer {
                    Ok(0) => {
                        this.close_after_save = true;
                        this.save(cx);
                    }
                    Ok(1) => {
                        // Explicit discard permits the Dock close while retaining no draft on disk.
                        if let Some(editor) = &this.editor {
                            let contents = editor.read(cx).contents(cx);
                            editor.update(cx, |editor, cx| editor.mark_saved(contents, None, cx));
                        }
                        Self::close(cx);
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }
}

impl EventEmitter<PanelEvent> for DocumentPanel {}
impl EventEmitter<DocumentClosed> for DocumentPanel {}
impl Focusable for DocumentPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.as_ref().map_or_else(
            || self.focus.clone(),
            |editor| editor.read(cx).focus_handle(cx),
        )
    }
}
impl BasePanel for DocumentPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.document"
    }
    fn closable(&self, _: &App) -> bool {
        // Dock has no asynchronous close veto. Route every close through the document
        // request until its panel contract supports waiting for save/discard/cancel.
        false
    }
    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.group = Some(group);
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.group = None;
        self.active = false;
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.refresh(false, window, cx);
        }
    }
    fn dump(&self, cx: &App) -> PanelState {
        let (line, column) = self
            .editor
            .as_ref()
            .map_or((self.line, self.column), |editor| {
                editor.read(cx).cursor_position(cx)
            });
        PanelState {
            panel_name: self.panel_name().to_owned(),
            children: Vec::new(),
            info: PanelInfo::panel(
                serde_json::json!({"host":self.host_identity,"path":self.path,"line":line.saturating_add(1),"column":column.saturating_add(1),"preview":self.preview}),
            ),
        }
    }
}
impl Panel for DocumentPanel {
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        let name = self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path);
        Some(
            format!(
                "{name}{}",
                if self.needs_close_prompt(cx) {
                    " •"
                } else {
                    ""
                }
            )
            .into(),
        )
    }
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_default()
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn title_suffix(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        Some(
            Button::new("close-document")
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .size_4()
                .accessibility_label(crate::i18n::t(cx, "common-close"))
                .tooltip(crate::i18n::t(cx, "common-close"))
                .disabled(self.saving(cx))
                .on_click(cx.listener(|this, _, window, cx| this.request_close(window, cx))),
        )
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let save = cx.weak_entity();
        let revert = save.clone();
        menu.item(
            PopupMenuItem::new(crate::i18n::t(cx, "common-save"))
                .disabled(self.pending || !self.needs_close_prompt(cx))
                .on_click(move |_, _, cx| {
                    _ = save.update(cx, Self::save);
                }),
        )
        .item(
            PopupMenuItem::new(crate::i18n::t(cx, "document-revert"))
                .disabled(self.pending)
                .on_click(move |_, window, cx| {
                    _ = revert.update(cx, |this, cx| this.request_reload(window, cx));
                }),
        )
    }
}
impl Render for DocumentPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let markdown = self.path.to_ascii_lowercase().ends_with(".md")
            || self.path.to_ascii_lowercase().ends_with(".markdown");
        let mut body = div()
            .id("document-panel")
            .track_focus(&self.focus)
            .key_context("BoottyDocument")
            .on_action(cx.listener(|this, _: &CloseDocument, window, cx| {
                this.request_close(window, cx);
                cx.stop_propagation();
            }))
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0();
        if let Some(error) = &self.error {
            body = body.child(gpui_kit::component::alert::Alert::error(
                "document-error",
                error.clone(),
            ));
        }
        if self.external.is_some() {
            body = body.child(self.external_change_banner(cx));
        }
        if let Some(editor) = &self.editor {
            if self.preview {
                body = body.child(
                    div()
                        .id("document-preview")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(gpui_kit::component::text::markdown(
                            editor.read(cx).contents(cx),
                        )),
                );
            } else {
                body = body.child(div().flex_1().min_h_0().child(editor.clone()));
            }
        } else {
            body = body.child(if self.pending {
                "Loading…"
            } else {
                "Document unavailable"
            });
        }
        let position = self
            .editor
            .as_ref()
            .map(|editor| editor.read(cx).cursor_position(cx));
        body.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .text_sm()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("{} · {}", self.context.host, self.path)),
                )
                .when(markdown, |row| {
                    row.child(
                        Button::new("preview-document")
                            .label(crate::i18n::t(
                                cx,
                                if self.preview {
                                    "common-edit"
                                } else {
                                    "common-preview"
                                },
                            ))
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.preview = !this.preview;
                                cx.emit(PanelEvent::LayoutChanged);
                                cx.notify();
                            })),
                    )
                })
                .when_some(position, |row, (line, column)| {
                    row.child(format!(
                        "{}:{}",
                        line.saturating_add(1),
                        column.saturating_add(1)
                    ))
                }),
        )
    }
}

impl DocumentPanel {
    fn external_change_banner(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(crate::i18n::t(cx, "document-external-change")),
            )
            .child(
                Button::new("revert-document")
                    .label(crate::i18n::t(cx, "document-revert"))
                    .small()
                    .ghost()
                    .flex_shrink_0()
                    .disabled(self.pending)
                    .on_click(cx.listener(|this, _, window, cx| this.request_reload(window, cx))),
            )
    }
}
