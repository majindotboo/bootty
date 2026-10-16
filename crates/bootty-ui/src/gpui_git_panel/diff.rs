//! Git and conversation patches use Kit's readonly diff surface and source-line selections.
use bootty_control::CommandTarget;
use bootty_git::{
    diff::{DiffAnchor, DiffSide, FileDiff},
    github::CodeComment,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffEvent, DiffFile, DiffLineRange, DiffSide as ViewSide, DiffState},
    dock::{BasePanel, Panel, PanelEvent},
    input::{Textarea, TextareaState},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, prelude::*,
};

struct ReviewDiff {
    target: CommandTarget,
    number: u32,
    head: String,
    file: FileDiff,
}

pub(super) struct PreparedReviewDiff {
    file: FileDiff,
    files: Result<Vec<DiffFile>, String>,
}

impl PreparedReviewDiff {
    pub(super) fn prepare(file: bootty_git::github::PullRequestFile) -> Result<Self, String> {
        let file = FileDiff::parse(file.filename, file.previous_filename, file.patch.as_deref())?;
        Ok(Self::from_diff(file))
    }

    pub(super) fn from_diff(file: FileDiff) -> Self {
        let files = component_files(&file);
        Self { file, files }
    }
}

// Git owns validation and review anchors; Kit owns patch presentation and selection geometry.
fn component_files(file: &FileDiff) -> Result<Vec<DiffFile>, String> {
    if file.patch_unavailable {
        return Err("The host did not provide a text diff for this file.".into());
    }
    let quoted_name = |prefix: &str, name: &str| {
        serde_json::to_string(&format!("{prefix}/{name}")).map_err(|error| error.to_string())
    };
    let mut patch = format!(
        "--- {}\n+++ {}\n",
        quoted_name("a", file.previous_path.as_deref().unwrap_or(&file.path))?,
        quoted_name("b", &file.path)?
    );
    for hunk in &file.hunks {
        patch.push_str(&hunk.header);
        patch.push('\n');
        for line in &hunk.lines {
            patch.push(if line.old_line.is_none() {
                '+'
            } else if line.new_line.is_none() {
                '-'
            } else {
                ' '
            });
            patch.push_str(&line.text);
            patch.push('\n');
        }
    }
    DiffFile::parse(&patch).map_err(|error| error.to_string())
}

/// Whitespace changes were hidden or shown again; the Changes panel asks Git again.
pub(super) struct WhitespaceChanged(pub bool);

/// Hunks of a local diff to stage, or to unstage when `reverse`.
pub(super) struct StageHunk {
    pub patch: String,
    pub reverse: bool,
}

/// A comment on lines of a local diff, for the session's conversation.
#[derive(Clone, Debug)]
pub struct DiffComment {
    pub anchor: DiffAnchor,
    pub quote: String,
    pub body: String,
}

pub(super) struct DraftCodeComment {
    pub target: CommandTarget,
    pub number: u32,
    pub head: String,
    pub comment: CodeComment,
}

#[allow(clippy::struct_excessive_bools)] // Each flag is an independent view state.
pub struct GitDiffPanel {
    pub(super) active: bool,
    pub(crate) group: Option<gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>>,
    diff: Entity<DiffState>,
    title: Option<String>,
    source: Option<String>,
    notice: Option<String>,
    review: Option<ReviewDiff>,
    /// The shown files' patches outside a review, to quote comments for the conversation.
    local: Vec<FileDiff>,
    /// Whether the shown change can be staged by hunk: unstaged changes stage, staged ones
    /// unstage (`true`).
    stage: Option<bool>,
    /// Whether the shown diff hides whitespace changes, when it came from the Changes panel,
    /// which can ask Git again.
    hide_whitespace: Option<bool>,
    /// Long lines wrap rather than scroll sideways.
    wrap: bool,
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
        let diff = cx.new(|cx| DiffState::new([], cx));
        let subscription = cx.subscribe(&diff, |this, _, event: &DiffEvent, cx| {
            if this.editing {
                return;
            }
            if let DiffEvent::SelectionChanged(range) = event {
                let selection = range.as_ref().and_then(|range| this.selection_for(range));
                if this.selection != selection {
                    this.selection = selection;
                    cx.notify();
                }
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
            diff,
            title: None,
            source: None,
            notice: None,
            review: None,
            local: Vec::new(),
            stage: None,
            hide_whitespace: None,
            wrap: false,
            selection: None,
            editing: false,
            submitting: false,
            review_error: None,
            comment,
            _selection_subscription: subscription,
            _comment_subscription: comment_subscription,
        }
    }

    fn replace_files(
        &mut self,
        title: String,
        files: Result<Vec<DiffFile>, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.editing {
            self.review_error =
                Some("Add or cancel the code comment before changing files.".into());
            cx.notify();
            return false;
        }
        self.title = Some(title);
        self.source = None;
        self.review = None;
        self.local.clear();
        self.stage = None;
        self.hide_whitespace = None;
        self.selection = None;
        self.notice = files.as_ref().err().cloned();
        self.diff
            .update(cx, |diff, cx| diff.set_files(files.unwrap_or_default(), cx));
        cx.emit(PanelEvent::LayoutChanged);
        cx.notify();
        true
    }

    pub(crate) fn show(
        &mut self,
        title: String,
        contents: String,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title.as_ref() == Some(&title) && self.source.as_ref() == Some(&contents) {
            return;
        }
        let source = contents.clone();
        let files = DiffFile::parse(&contents)
            .map_err(|error| error.to_string())
            .and_then(|files| {
                if files.is_empty() {
                    Err(if contents.trim().is_empty() {
                        "No text changes to show.".into()
                    } else {
                        contents
                    })
                } else {
                    Ok(files)
                }
            });
        if self.replace_files(title, files, cx) {
            self.local = FileDiff::parse_all(&source);
            self.source = Some(source);
        }
    }

    /// Marks the shown diff as the Changes panel's, which offers hiding whitespace changes, and
    /// whether it hides them.
    pub(super) fn mark_from_changes(&mut self, hide_whitespace: bool, cx: &mut Context<Self>) {
        self.hide_whitespace = Some(hide_whitespace);
        cx.notify();
    }

    /// The diff's view controls: unified or side by side, wrapped or not, and, for the
    /// session's changes, whether whitespace changes show.
    fn toolbar(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let split = self.diff.read(cx).mode() == gpui_kit::component::diff::DiffMode::Split;
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .when_some(self.hide_whitespace, |row, hidden| {
                row.child(
                    Button::new("diff-whitespace")
                        .debug_selector(move || format!("diff-whitespace:{hidden}"))
                        .label("Hide whitespace")
                        .xsmall()
                        .ghost()
                        .selected(hidden)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hide_whitespace = Some(!hidden);
                            cx.emit(WhitespaceChanged(!hidden));
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("diff-wrap")
                    .debug_selector({
                        let wrap = self.wrap;
                        move || format!("diff-wrap:{wrap}")
                    })
                    .label("Wrap lines")
                    .xsmall()
                    .ghost()
                    .selected(self.wrap)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.wrap = !this.wrap;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("diff-mode")
                    .debug_selector(move || {
                        format!("diff-mode:{}", if split { "split" } else { "unified" })
                    })
                    .label(if split { "Unified" } else { "Side by side" })
                    .xsmall()
                    .ghost()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.diff.update(cx, |diff, cx| {
                            diff.set_mode(
                                if split {
                                    gpui_kit::component::diff::DiffMode::Unified
                                } else {
                                    gpui_kit::component::diff::DiffMode::Split
                                },
                                cx,
                            );
                        });
                        cx.notify();
                    })),
            )
    }

    /// Lets the shown change's hunks stage, from the `unstaged` group, or unstage, from the
    /// `staged` one.
    pub(super) fn allow_staging(&mut self, group: &str, cx: &mut Context<Self>) {
        self.stage = match group {
            "unstaged" => Some(false),
            "staged" => Some(true),
            _ => None,
        };
        cx.notify();
    }

    /// Stages the hunks the selection touches, or unstages them.
    fn stage_selection(&mut self, cx: &mut Context<Self>) {
        let (Some(reverse), Some((anchor, _))) = (self.stage, &self.selection) else {
            return;
        };
        let Some(patch) = self
            .local
            .iter()
            .find(|file| file.path == anchor.path)
            .and_then(|file| file.hunk_patch(anchor))
        else {
            return;
        };
        cx.emit(StageHunk { patch, reverse });
        self.selection = None;
        self.diff
            .update(cx, |diff, cx| diff.set_selected_lines(None, cx));
        cx.notify();
    }

    pub(super) fn show_review(
        &mut self,
        target: CommandTarget,
        number: u32,
        head: String,
        prepared: PreparedReviewDiff,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.replace_files(prepared.file.path.clone(), prepared.files, cx) {
            self.review = Some(ReviewDiff {
                target,
                number,
                head,
                file: prepared.file,
            });
        }
    }

    pub(crate) fn show_file_diff(
        &mut self,
        file: FileDiff,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prepared = PreparedReviewDiff::from_diff(file);
        if self.replace_files(prepared.file.path.clone(), prepared.files, cx) {
            self.local = vec![prepared.file];
        }
    }

    fn selection_for(&self, range: &DiffLineRange) -> Option<(DiffAnchor, String)> {
        if range.side() != range.end_side() {
            return None;
        }
        let file = match &self.review {
            Some(review) => &review.file,
            None => self
                .local
                .iter()
                .find(|file| file.path == range.path().as_ref())?,
        };
        let side = match range.side() {
            ViewSide::Original => DiffSide::Left,
            ViewSide::Modified => DiffSide::Right,
        };
        let anchor = DiffAnchor {
            path: range.path().to_string(),
            side,
            start_line: u32::try_from(range.start().min(range.end())).ok()?,
            line: u32::try_from(range.start().max(range.end())).ok()?,
        };
        let quote = file.quote(&anchor).ok()?;
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
        // Outside a review, the comment goes to the session's conversation.
        let Some(review) = &self.review else {
            cx.emit(DiffComment {
                anchor,
                quote,
                body,
            });
            self.editing = false;
            self.selection = None;
            self.diff
                .update(cx, |diff, cx| diff.set_selected_lines(None, cx));
            cx.notify();
            return;
        };
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
                self.diff
                    .update(cx, |diff, cx| diff.set_selected_lines(None, cx));
                self.diff.focus_handle(cx).focus(window, cx);
            }
            Err(error) => self.review_error = Some(error),
        }
        cx.notify();
    }

    /// What selected lines offer: staging their hunk outside a review, and commenting on them.
    fn selection_actions(&self, cx: &Context<Self>) -> Vec<Button> {
        let stage = self.stage.filter(|_| self.review.is_none()).map(|reverse| {
            Button::new("stage-hunk")
                .debug_selector(|| "git-stage-hunk".into())
                .label(if reverse {
                    "Unstage hunk"
                } else {
                    "Stage hunk"
                })
                .ghost()
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.stage_selection(cx)))
        });
        let comment = Button::new("code-comment")
            .label("Comment")
            .ghost()
            .small()
            .on_click(cx.listener(|this, _, window, cx| this.start_comment(window, cx)));
        stage.into_iter().chain([comment]).collect()
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
                                row.children(self.selection_actions(cx))
                            }),
                    )
                    .when(self.editing, |view| {
                        view.child(
                            Textarea::new(&self.comment)
                                .context_menu(crate::gpui::text_menu(&self.comment))
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
                                            this.diff.focus_handle(cx).focus(window, cx);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("save-code-comment")
                                        .label(if self.review.is_some() {
                                            "Add to review"
                                        } else {
                                            "Add to conversation"
                                        })
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
impl EventEmitter<DiffComment> for GitDiffPanel {}
impl EventEmitter<StageHunk> for GitDiffPanel {}
impl EventEmitter<WhitespaceChanged> for GitDiffPanel {}
impl Focusable for GitDiffPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.diff.focus_handle(cx)
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
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .when(self.title.is_some() && self.notice.is_none(), |view| {
                view.child(self.toolbar(cx))
            })
            .child(div().flex_1().min_h_0().child(
                if self.title.is_none() || self.notice.is_some() {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_6()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            self.notice
                                .clone()
                                .unwrap_or_else(|| "Choose a change to view its diff.".into()),
                        )
                        .into_any_element()
                } else {
                    // Several files need their names; one file's is the panel's title.
                    Diff::new(&self.diff)
                        .header_visible(self.diff.read(cx).files().len() > 1)
                        .soft_wrap(self.wrap)
                        .soft_wrap(true)
                        .size_full()
                        .into_any_element()
                },
            ))
            .child(self.review_controls(cx))
    }
}
