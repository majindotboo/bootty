//! Read-only Git diff editor and line decorations.

use gpui_kit::component::{
    ActiveTheme as _,
    dock::{BasePanel, Panel, PanelEvent},
    input::{EditorState, TextDecoration, TextDecorationCollection},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    Hsla, IntoElement, Render, SharedString, Window,
};

pub struct GitDiffPanel {
    pub(super) active: bool,
    pub(crate) group: Option<gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>>,
    editor: Entity<EditorState>,
    title: Option<String>,
    decorations: TextDecorationCollection,
    decoration_colors: Option<[Hsla; 3]>,
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
        self.decoration_colors = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value(contents, window, cx));
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
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
        let colors = [
            cx.theme().success,
            cx.theme().danger,
            cx.theme().muted_foreground,
        ];
        if self.decoration_colors != Some(colors) {
            self.decoration_colors = Some(colors);
            let contents = self.editor.read(cx).value();
            let mut offset = 0_usize;
            let decorations = contents
                .split_inclusive('\n')
                .filter_map(|line| {
                    let range = offset..offset.saturating_add(line.len());
                    offset = range.end;
                    let color = if line.starts_with("+++")
                        || line.starts_with("---")
                        || line.starts_with("@@")
                        || line.starts_with("diff ")
                        || line.starts_with("index ")
                    {
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
                            ..Default::default()
                        },
                    ))
                })
                .collect();
            self.decorations.set(decorations, cx);
        }
        crate::gpui::readonly_editor(&self.editor, "Git diff")
    }
}
