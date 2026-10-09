//! Shared Git and conversation diff editor; review selections retain exact source lines.

use std::{
    ops::{Add as _, Div as _, Mul as _, Sub as _},
    sync::Arc,
};

use bootty_control::CommandTarget;
use bootty_git::{
    diff::{DiffAnchor, DiffLine, DiffSide, FileDiff},
    github::CodeComment,
};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent},
    input::{EditorState, TextDecoration, TextDecorationCollection, Textarea, TextareaState},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    Hsla, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
    div, prelude::*,
};

struct ReviewDiff {
    target: CommandTarget,
    number: u32,
    head: String,
    file: FileDiff,
    rows: Arc<Vec<(std::ops::Range<usize>, DiffLine)>>,
}

pub(super) struct PreparedReviewDiff {
    file: FileDiff,
    contents: String,
    rows: Arc<Vec<(std::ops::Range<usize>, DiffLine)>>,
    gutter: Arc<DiffGutter>,
}

struct DiffGutter {
    rows: Arc<Vec<(std::ops::Range<usize>, DiffLine)>>,
    display_rows: Vec<Option<usize>>,
    digits: usize,
}

impl PreparedReviewDiff {
    pub(super) fn prepare(file: bootty_git::github::PullRequestFile) -> Result<Self, String> {
        let file = FileDiff::parse(file.filename, file.previous_filename, file.patch.as_deref())?;
        Ok(Self::from_diff(file))
    }

    pub(super) fn from_diff(file: FileDiff) -> Self {
        let mut contents = String::new();
        let mut rows = Vec::new();
        let mut display_rows = Vec::new();
        for hunk in &file.hunks {
            contents.push_str(&hunk.header);
            contents.push('\n');
            display_rows.push(None);
            for line in &hunk.lines {
                let start = contents.len();
                contents.push(if line.old_line.is_none() {
                    '+'
                } else if line.new_line.is_none() {
                    '-'
                } else {
                    ' '
                });
                contents.push_str(&line.text);
                contents.push('\n');
                display_rows.push(Some(rows.len()));
                rows.push((start..contents.len(), line.clone()));
            }
        }
        if file.patch_unavailable {
            contents = "The host did not provide a text diff for this file.".into();
        }
        let rows = Arc::new(rows);
        let digits = rows
            .iter()
            .flat_map(|(_, line)| [line.old_line, line.new_line])
            .flatten()
            .max()
            .unwrap_or(1)
            .to_string()
            .len();
        let gutter = Arc::new(DiffGutter {
            rows: Arc::clone(&rows),
            display_rows,
            digits,
        });
        Self {
            file,
            contents,
            rows,
            gutter,
        }
    }
}

fn render_gutter(
    editor: Entity<EditorState>,
    gutter: Arc<DiffGutter>,
    gutter_width: gpui_kit::Rems,
) -> impl IntoElement {
    gpui_kit::canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, cx| {
            let (height, visible) = {
                let editor = editor.read(cx);
                let Some(visible) = editor.visible_row_range() else {
                    return;
                };
                let Some(height) = editor.line_height() else {
                    return;
                };
                let rows = visible
                    .filter_map(|row| {
                        let index = gutter.display_rows.get(row).copied().flatten()?;
                        let (range, line) = gutter.rows.get(index)?;
                        let bounds = editor.range_to_bounds(&(range.start..range.start))?;
                        Some((bounds, line.old_line, line.new_line))
                    })
                    .collect::<Vec<_>>();
                (height, rows)
            };
            let font_size = cx.theme().mono_font_size;
            for (line_bounds, old_line, new_line) in visible {
                for (column, number) in [(0_f32, old_line), (1_f32, new_line)] {
                    let Some(number) = number else {
                        continue;
                    };
                    let text: gpui_kit::SharedString = number.to_string().into();
                    let shaped = window.text_system().shape_line(
                        text.clone(),
                        font_size,
                        &[gpui_kit::TextRun {
                            len: text.len(),
                            font: gpui_kit::font(cx.theme().mono_font_family.clone()),
                            color: cx.theme().muted_foreground,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    );
                    let width = bounds.size.width.div(2.0);
                    let x = bounds
                        .origin
                        .x
                        .add(width.mul(column + 1.0))
                        .sub(shaped.width)
                        .sub(gpui_kit::rems(0.5).to_pixels(window.rem_size()));
                    _ = shaped.paint(
                        gpui_kit::point(x, line_bounds.origin.y),
                        height,
                        gpui_kit::TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .left_0()
    .w(gutter_width)
}

pub(super) struct DraftCodeComment {
    pub target: CommandTarget,
    pub number: u32,
    pub head: String,
    pub comment: CodeComment,
}

pub struct GitDiffPanel {
    pub(super) active: bool,
    pub(crate) group: Option<gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>>,
    editor: Entity<EditorState>,
    gutter: Option<Arc<DiffGutter>>,
    title: Option<String>,
    decorations: TextDecorationCollection,
    decoration_colors: Option<[Hsla; 3]>,
    review: Option<ReviewDiff>,
    selection: Option<(DiffAnchor, String)>,
    editing: bool,
    submitting: bool,
    review_error: Option<String>,
    comment: Entity<TextareaState>,
    _selection_subscription: Subscription,
    _comment_subscription: Subscription,
}

impl GitDiffPanel {
    pub(super) fn has_review_draft(&self, target: &CommandTarget) -> bool {
        self.editing
            && self
                .review
                .as_ref()
                .is_some_and(|review| &review.target == target)
    }
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("diff")
                .soft_wrap(false)
        });
        let decorations = editor.update(cx, |editor, cx| {
            editor.create_decorations_collection(Vec::new(), cx)
        });
        let subscription = cx.observe_in(&editor, window, |this, editor, _, cx| {
            if this.editing {
                return;
            }
            let range = editor.read(cx).selected_range();
            let selection = this.selection_for(range);
            if this.selection != selection {
                this.selection = selection;
                cx.notify();
            }
        });
        let comment = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 4)
                .placeholder("Add a comment…")
        });
        let comment_subscription = cx.observe(&comment, |_, _, cx| cx.notify());
        Self {
            active: false,
            group: None,
            editor,
            gutter: None,
            decorations,
            decoration_colors: None,
            title: None,
            review: None,
            selection: None,
            editing: false,
            submitting: false,
            review_error: None,
            comment,
            _selection_subscription: subscription,
            _comment_subscription: comment_subscription,
        }
    }

    pub(super) fn show(
        &mut self,
        title: String,
        contents: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing {
            self.review_error =
                Some("Add or cancel the code comment before changing files.".into());
            cx.notify();
            return;
        }
        self.review = None;
        self.selection = None;
        self.editing = false;
        if self.title.as_ref() == Some(&title) && self.editor.read(cx).value().as_str() == contents
        {
            return;
        }
        self.title = Some(title);
        self.gutter = None;
        self.editor
            .update(cx, |editor, cx| editor.set_line_number(true, window, cx));
        self.decoration_colors = None;
        self.editor
            .update(cx, |editor, cx| editor.set_value(contents, window, cx));
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
    }

    pub(super) fn show_review(
        &mut self,
        target: CommandTarget,
        number: u32,
        head: String,
        prepared: PreparedReviewDiff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing {
            self.review_error =
                Some("Add or cancel the code comment before changing files.".into());
            cx.notify();
            return;
        }
        let PreparedReviewDiff {
            file,
            contents,
            rows,
            gutter,
        } = prepared;
        self.show(file.path.clone(), contents, window, cx);
        self.set_gutter(gutter, window, cx);
        self.review = Some(ReviewDiff {
            target,
            number,
            head,
            file,
            rows,
        });
        cx.notify();
    }

    pub(crate) fn show_file_diff(
        &mut self,
        file: FileDiff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prepared = PreparedReviewDiff::from_diff(file);
        self.show(prepared.file.path, prepared.contents, window, cx);
        self.set_gutter(prepared.gutter, window, cx);
        cx.notify();
    }

    fn set_gutter(&mut self, gutter: Arc<DiffGutter>, window: &mut Window, cx: &mut Context<Self>) {
        self.gutter = Some(gutter);
        self.editor
            .update(cx, |editor, cx| editor.set_line_number(false, window, cx));
    }

    fn render_editor(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let Some(gutter) = &self.gutter else {
            return crate::gpui::readonly_editor(&self.editor, "Git diff").into_any_element();
        };
        let gutter = Arc::clone(gutter);
        let editor = self.editor.clone();
        let digits = num_traits::ToPrimitive::to_f32(&gutter.digits).unwrap_or(10.0);
        let gutter_width = gpui_kit::rems(digits.mul_add(1.25, 1.5));
        let canvas = render_gutter(editor, gutter, gutter_width);
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                div()
                    .size_full()
                    .pl(gutter_width)
                    .child(crate::gpui::readonly_editor(&self.editor, "File diff")),
            )
            .child(canvas)
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(Self::select_diff_line),
            )
            .on_scroll_wheel(cx.listener(
                move |this, event: &gpui_kit::ScrollWheelEvent, window, cx| {
                    let bounds = this.editor.read(cx).input_bounds();
                    let width = gutter_width.to_pixels(window.rem_size());
                    if event.position.x >= bounds.left()
                        || event.position.x < bounds.left().sub(width)
                    {
                        return;
                    }
                    this.editor.update(cx, |editor, cx| {
                        let Some(height) = editor.line_height() else {
                            return;
                        };
                        let offset = editor.scroll_offset().add(event.delta.pixel_delta(height));
                        editor.set_scroll_offset(offset, cx);
                    });
                    cx.stop_propagation();
                },
            ))
            .into_any_element()
    }

    fn select_diff_line(
        &mut self,
        event: &gpui_kit::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(gutter) = &self.gutter else {
            return;
        };
        let editor = self.editor.read(cx);
        if event.position.x >= editor.input_bounds().left() {
            return;
        }
        let range = editor.visible_row_range().and_then(|mut visible| {
            visible.find_map(|row| {
                let index = gutter.display_rows.get(row).copied().flatten()?;
                let (range, _) = gutter.rows.get(index)?;
                let bounds = editor.range_to_bounds(&(range.start..range.start))?;
                (event.position.y >= bounds.top() && event.position.y < bounds.bottom())
                    .then(|| range.clone())
            })
        });
        let Some(mut range) = range else {
            return;
        };
        if event.modifiers.shift {
            let selected = editor.selected_range();
            range = selected.start.min(range.start)..selected.end.max(range.end);
        }
        self.editor
            .update(cx, |editor, cx| editor.set_selected_range(range, cx));
        self.editor.focus_handle(cx).focus(window, cx);
        cx.stop_propagation();
    }

    fn selection_for(&self, range: std::ops::Range<usize>) -> Option<(DiffAnchor, String)> {
        if range.is_empty() {
            return None;
        }
        let review = self.review.as_ref()?;
        let start = review
            .rows
            .partition_point(|(row, _)| row.end <= range.start);
        let end = review
            .rows
            .partition_point(|(row, _)| row.start < range.end);
        let lines = review
            .rows
            .get(start..end)?
            .iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return None;
        }
        let side = if lines.iter().all(|line| line.new_line.is_some()) {
            DiffSide::Right
        } else if lines.iter().all(|line| line.old_line.is_some()) {
            DiffSide::Left
        } else {
            return None;
        };
        let anchor = DiffAnchor {
            path: review.file.path.clone(),
            side,
            start_line: lines.first()?.number(side)?,
            line: lines.last()?.number(side)?,
        };
        let quote = review.file.quote(&anchor).ok()?;
        Some((anchor, quote))
    }

    fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection.is_none() {
            return;
        }
        self.editing = true;
        self.comment.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn save_comment(&mut self, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        let Some((anchor, quote)) = self.selection.clone() else {
            return;
        };
        let body = self.comment.read(cx).value().to_string();
        if body.trim().is_empty() || body.len() > 65536 {
            return;
        }
        let Some(review) = &self.review else { return };
        cx.emit(DraftCodeComment {
            target: review.target.clone(),
            number: review.number,
            head: review.head.clone(),
            comment: CodeComment {
                anchor,
                quote,
                body,
            },
        });
        self.submitting = true;
        self.review_error = None;
        cx.notify();
    }

    pub(super) fn admit_comment(
        &mut self,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.submitting = false;
        match result {
            Ok(()) => {
                self.editing = false;
                self.selection = None;
                self.review_error = None;
                self.editor
                    .update(cx, |editor, cx| editor.set_selected_range(0..0, cx));
                self.editor.focus_handle(cx).focus(window, cx);
            }
            Err(error) => self.review_error = Some(error),
        }
        cx.notify();
    }

    fn review_controls(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(self.review_error.as_ref(), |view, error| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
            .when_some(self.selection.as_ref(), |view, (anchor, _)| {
                view.p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}:{}–{}",
                                        anchor.path, anchor.start_line, anchor.line
                                    )),
                            )
                            .when(!self.editing, |row| {
                                row.child(
                                    Button::new("code-comment")
                                        .label("Comment")
                                        .ghost()
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.start_comment(window, cx);
                                        })),
                                )
                            }),
                    )
                    .when(self.editing, |view| {
                        view.child(
                            Textarea::new(&self.comment)
                                .disabled(self.submitting)
                                .aria_label("Code comment")
                                .w_full(),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_1()
                                .child(
                                    Button::new("cancel-code-comment")
                                        .disabled(self.submitting)
                                        .label("Cancel")
                                        .ghost()
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.editing = false;
                                            this.editor.focus_handle(cx).focus(window, cx);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("save-code-comment")
                                        .label("Add to review")
                                        .small()
                                        .disabled(
                                            self.submitting
                                                || self.comment.read(cx).value().trim().is_empty(),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_comment(cx);
                                        })),
                                ),
                        )
                    })
            })
    }
}

impl EventEmitter<PanelEvent> for GitDiffPanel {}
impl EventEmitter<DraftCodeComment> for GitDiffPanel {}
impl Focusable for GitDiffPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}
impl BasePanel for GitDiffPanel {
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
        if self.title.is_none() {
            return gpui_kit::div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .text_color(cx.theme().muted_foreground)
                .child("Choose a change to view its diff.")
                .into_any_element();
        }
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
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(div().flex_1().min_h_0().child(self.render_editor(cx)))
            .child(self.review_controls(cx))
            .into_any_element()
    }
}
