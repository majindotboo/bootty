//! Read-only Git diff editor and line decorations.

use gpui_kit::component::{
    ActiveTheme as _,
    dock::{BasePanel, Panel, PanelEvent},
    input::{EditorState, TextDecoration, TextDecorationCollection},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    Hsla, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window, div,
    prelude::*,
};

pub struct GitDiffPanel {
    pub(super) active: bool,
    pub(crate) group: Option<gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>>,
    editor: Entity<EditorState>,
    title: Option<String>,
    decorations: TextDecorationCollection,
    decoration_colors: Option<[Hsla; 3]>,
    added: u64,
    removed: u64,
    binary: bool,
}

impl GitDiffPanel {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("diff")
                .soft_wrap(false)
        });
        let decorations = editor.update(cx, |editor, cx| {
            editor.create_decorations_collection(Vec::new(), cx)
        });
        Self {
            active: false,
            group: None,
            editor,
            decorations,
            decoration_colors: None,
            title: None,
            added: 0,
            removed: 0,
            binary: false,
        }
    }

    pub(super) fn show(
        &mut self,
        title: String,
        contents: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title.as_ref() == Some(&title) && self.editor.read(cx).value().as_str() == contents
        {
            return;
        }
        self.title = Some(title);
        self.added = 0;
        self.removed = 0;
        self.binary = false;
        let mut in_hunk = false;
        for line in contents.lines() {
            if line.starts_with("diff --git ") {
                in_hunk = false;
            }
            if line.starts_with("@@") {
                in_hunk = true;
            }
            if in_hunk && line.starts_with('+') {
                self.added = self.added.saturating_add(1);
            }
            if in_hunk && line.starts_with('-') {
                self.removed = self.removed.saturating_add(1);
            }
            self.binary |=
                line.starts_with("Binary files ") || line.starts_with("GIT binary patch");
        }
        self.decoration_colors = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value(contents, window, cx));
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    fn refresh_decorations(&mut self, cx: &mut Context<Self>) {
        let colors = [
            cx.theme().success,
            cx.theme().danger,
            cx.theme().muted_foreground,
        ];
        if self.decoration_colors != Some(colors) {
            self.decoration_colors = Some(colors);
            let contents = self.editor.read(cx).value();
            let mut offset = 0_usize;
            let mut in_hunk = false;
            let decorations = contents
                .split_inclusive('\n')
                .filter_map(|line| {
                    let range = offset..offset.saturating_add(line.len());
                    offset = range.end;
                    if line.starts_with("diff --git ") {
                        in_hunk = false;
                    }
                    let hunk_header = line.starts_with("@@");
                    if hunk_header {
                        in_hunk = true;
                    }
                    let color = if hunk_header || !in_hunk {
                        colors[2]
                    } else if line.starts_with('+') {
                        colors[0]
                    } else if line.starts_with('-') {
                        colors[1]
                    } else {
                        return None;
                    };
                    Some(TextDecoration::new(
                        range,
                        HighlightStyle {
                            color: Some(color),
                            background_color: if in_hunk && line.starts_with('+') {
                                Some(colors[0].opacity(0.08))
                            } else if in_hunk && line.starts_with('-') {
                                Some(colors[1].opacity(0.08))
                            } else {
                                None
                            },
                            ..Default::default()
                        },
                    ))
                })
                .collect();
            self.decorations.set(decorations, cx);
        }
    }
}

impl EventEmitter<PanelEvent> for GitDiffPanel {}
impl Focusable for GitDiffPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}
impl BasePanel for GitDiffPanel {
    fn visible(&self, _: &App) -> bool {
        self.title.is_some()
    }

    fn set_active(&mut self, active: bool, _: &mut Window, _: &mut Context<Self>) {
        self.active = active;
    }
    fn on_added_to(
        &mut self,
        group: gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.group = Some(group);
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.group = None;
        self.active = false;
    }
    fn panel_name(&self) -> &'static str {
        "bootty.diff"
    }
}
impl Panel for GitDiffPanel {
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        Some(
            self.title
                .clone()
                .unwrap_or_else(|| crate::i18n::t(cx, "panel-diff"))
                .into(),
        )
    }
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_default()
    }
}
impl Render for GitDiffPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_decorations(cx);
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.title.clone().unwrap_or_else(|| "Diff".to_owned())),
            )
            .when(self.added > 0 || self.removed > 0, |header| {
                header.child(super::diff_stat_element(
                    bootty_git::changes::DiffStat::Text {
                        added: self.added,
                        removed: self.removed,
                    },
                    cx,
                ))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Read only"),
            );
        let body = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(header);
        if self.binary {
            return body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child("Binary file")
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Text changes are unavailable for this file."),
                        ),
                )
                .into_any_element();
        }
        body.child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(crate::gpui::readonly_editor(&self.editor, "Git diff")),
        )
        .into_any_element()
    }
}
