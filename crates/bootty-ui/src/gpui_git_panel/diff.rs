//! Git and conversation patches use Kit's readonly diff surface and source-line selections.
use bootty_control::CommandTarget;
use bootty_git::{
    diff::{DiffAnchor, DiffSide, FileDiff},
    github::CodeComment,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    diff::{
        Diff, DiffAnnotation, DiffChangeIndicator, DiffEvent, DiffFile, DiffHoverHighlight,
        DiffHunkSeparator, DiffLinePosition, DiffLineRange, DiffSide as ViewSide, DiffState,
    },
    dock::{BasePanel, Panel, PanelEvent},
    input::{Textarea, TextareaState},
    popover::Popover,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, prelude::*,
};

#[path = "files.rs"]
mod files;

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
        if file.new_file {
            "/dev/null".to_owned()
        } else {
            quoted_name("a", file.previous_path.as_deref().unwrap_or(&file.path))?
        },
        if file.deleted_file {
            "/dev/null".to_owned()
        } else {
            quoted_name("b", &file.path)?
        }
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
            if line.no_newline {
                patch.push_str("\\ No newline at end of file\n");
            }
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

pub struct GitDiffPanel {
    pub(super) active: bool,
    pub(crate) group: Option<gpui_kit::WeakEntity<gpui_kit::component::dock::TabGroup>>,
    diff: Entity<DiffState>,
    files: Entity<files::DiffFiles>,
    _files_subscription: Subscription,
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
    /// Optional navigation for exactly the files in this patch, including recorded turns.
    files_open: bool,
    /// The view controls show in a row of their own; a host showing them in its own header
    /// turns this off.
    toolbar: bool,
    /// Bounds the inline comment to its visible column, independently of long code lines.
    width: gpui_kit::Pixels,
    selection: Option<(DiffAnchor, String)>,
    hovered_line: Option<DiffLinePosition>,
    comment_drag: Option<CommentDrag>,
    editing: bool,
    submitting: bool,
    review_error: Option<String>,
    comment: Entity<TextareaState>,
    _selection_subscription: Subscription,
    _comment_subscription: Subscription,
}

struct CommentDrag {
    start: Option<DiffLinePosition>,
    end: Option<DiffLinePosition>,
    origin: gpui_kit::Point<gpui_kit::Pixels>,
    selected_before: Option<DiffLineRange>,
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
        let files = cx.new(files::DiffFiles::new);
        let files_subscription = cx.subscribe(&files, |this, _, event: &files::SelectFile, cx| {
            if this.editing {
                return;
            }
            this.diff.update(cx, |diff, cx| {
                diff.set_file_collapsed(&event.0, false, cx);
                diff.scroll_to_file(&event.0, cx);
            });
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
            files,
            _files_subscription: files_subscription,
            title: None,
            source: None,
            notice: None,
            review: None,
            local: Vec::new(),
            stage: None,
            hide_whitespace: None,
            wrap: true,
            files_open: false,
            toolbar: true,
            width: gpui_kit::px(0.),
            selection: None,
            hovered_line: None,
            comment_drag: None,
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
        let files = files.unwrap_or_default();
        self.files.update(cx, |tree, cx| tree.set_files(&files, cx));
        self.diff.update(cx, |diff, cx| diff.set_files(files, cx));
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

    /// Leaves the view controls to a host that shows [`Self::controls`] in its own header.
    pub(super) const fn hide_toolbar(&mut self) {
        self.toolbar = false;
    }

    /// Whether a code comment is being written, which keeps the keyboard.
    pub(super) const fn is_commenting(&self) -> bool {
        self.editing
    }

    pub(super) fn line_counts(&self, cx: &App) -> Option<bootty_git::changes::LineCounts> {
        let files = self.diff.read(cx).files();
        if files.is_empty() {
            return None;
        }
        Some(bootty_git::changes::LineCounts {
            added: u32::try_from(files.iter().map(DiffFile::additions).sum::<usize>()).ok()?,
            removed: u32::try_from(files.iter().map(DiffFile::deletions).sum::<usize>()).ok()?,
        })
    }

    /// The diff's view controls: unified or side by side, wrapped or not, and, for the
    /// session's changes, whether whitespace changes show.
    pub(super) fn controls(&self, owner: gpui_kit::WeakEntity<Self>, cx: &App) -> Vec<Button> {
        self.control_buttons(owner, false, cx)
    }

    pub(super) fn controls_menu(
        &self,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        Popover::new("diff-display-menu")
            .trigger(
                Button::new("diff-display")
                    .debug_selector(|| "git-diff-display".into())
                    .ghost()
                    .icon(gpui_kit::component::IconName::Settings)
                    .accessibility_label("Diff display")
                    .tooltip("Diff display"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_1()
                    .w_56()
                    .children(self.control_buttons(owner, true, cx)),
            )
            .into_any_element()
    }

    fn set_files_collapsed(&self, collapsed: bool, cx: &mut Context<Self>) {
        self.diff.update(cx, |diff, cx| {
            let paths = diff
                .files()
                .iter()
                .map(|file| file.path().clone())
                .collect::<Vec<_>>();
            for path in paths {
                diff.set_file_collapsed(&path, collapsed, cx);
            }
        });
        cx.notify();
    }

    fn control_buttons(
        &self,
        owner: gpui_kit::WeakEntity<Self>,
        labeled: bool,
        cx: &App,
    ) -> Vec<Button> {
        let split = self.diff.read(cx).mode() == gpui_kit::component::diff::DiffMode::Split;
        let wrap = self.wrap;
        let toggle = |id: &'static str, icon: &'static str, label: &'static str, on: bool| {
            Button::new(id)
                .icon(gpui_kit::component::Icon::default().path(format!("icons/{icon}.svg")))
                .ghost()
                .selected(on)
                .when(labeled, |button| {
                    button.label(label).w_full().justify_start()
                })
                .accessibility_label(label)
                .tooltip(label)
        };
        let collapsed = !self.diff.read(cx).files().is_empty()
            && self
                .diff
                .read(cx)
                .files()
                .iter()
                .all(|file| self.diff.read(cx).is_file_collapsed(file.path()));
        let files_owner = owner.clone();
        let collapse_owner = owner.clone();
        let mut controls = vec![
            toggle(
                "diff-collapse",
                "fold-vertical",
                if collapsed {
                    "Expand all files"
                } else {
                    "Collapse all files"
                },
                collapsed,
            )
            .disabled(self.editing)
            .on_click(move |_, _, cx| {
                _ = collapse_owner.update(cx, |this, cx| {
                    this.set_files_collapsed(!collapsed, cx);
                });
            }),
        ];
        if let Some(hidden) = self.hide_whitespace {
            let owner = owner.clone();
            controls.push(
                toggle(
                    "diff-whitespace",
                    "space",
                    "Hide whitespace changes",
                    hidden,
                )
                .disabled(self.editing)
                .debug_selector(move || format!("diff-whitespace:{hidden}"))
                .on_click(move |_, _, cx| {
                    _ = owner.update(cx, |this, cx| {
                        this.hide_whitespace = Some(!hidden);
                        cx.emit(WhitespaceChanged(!hidden));
                        cx.notify();
                    });
                }),
            );
        }
        let wrap_owner = owner.clone();
        controls.push(
            toggle("diff-wrap", "text-wrap", "Wrap long lines", wrap)
                .debug_selector(move || format!("diff-wrap:{wrap}"))
                .on_click(move |_, _, cx| {
                    _ = wrap_owner.update(cx, |this, cx| {
                        this.wrap = !this.wrap;
                        cx.notify();
                    });
                }),
        );
        controls.push(
            toggle("diff-mode", "columns-2", "Side by side", split)
                .debug_selector(move || {
                    format!("diff-mode:{}", if split { "split" } else { "unified" })
                })
                .on_click(move |_, _, cx| {
                    _ = owner.update(cx, |this, cx| {
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
                    });
                }),
        );
        if !self.diff.read(cx).files().is_empty() {
            controls.push(self.files_control(files_owner, labeled));
        }
        controls
    }

    fn files_control(&self, owner: gpui_kit::WeakEntity<Self>, labeled: bool) -> Button {
        let label = if self.files_open {
            "Hide file list"
        } else {
            "Show file list"
        };
        Button::new("diff-files")
            .icon(gpui_kit::component::Icon::default().path("icons/folder-tree.svg"))
            .ghost()
            .selected(self.files_open)
            .when(labeled, |button| {
                button.label(label).w_full().justify_start()
            })
            .debug_selector(|| "git-diff-files-toggle".into())
            .accessibility_label(label)
            .tooltip(label)
            .on_click(move |_, _, cx| {
                _ = owner.update(cx, |this, cx| {
                    this.files_open = !this.files_open;
                    cx.notify();
                });
            })
    }

    /// Lets the shown change's hunks stage, from the `unstaged` group, or unstage, from the
    /// `staged` one; any other group, or none, stages nothing by hunk.
    pub(super) fn allow_staging(&mut self, group: Option<&str>, cx: &mut Context<Self>) {
        self.stage = match group {
            Some("unstaged") => Some(false),
            Some("staged") => Some(true),
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

    /// Unified context has one painted side; keep a drag on its original source side.
    fn comment_endpoint(
        &self,
        start: &DiffLinePosition,
        end: &DiffLinePosition,
    ) -> Option<DiffLinePosition> {
        if start.path() != end.path() {
            return None;
        }
        if start.side() == end.side() {
            return Some(end.clone());
        }
        let file = self
            .review
            .as_ref()
            .map(|review| &review.file)
            .or_else(|| {
                self.local
                    .iter()
                    .find(|file| file.path == end.path().as_ref())
            })?;
        let side = |side| {
            if side == ViewSide::Original {
                DiffSide::Left
            } else {
                DiffSide::Right
            }
        };
        let line = file
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .find(|line| {
                line.number(side(end.side()))
                    .and_then(|line| usize::try_from(line).ok())
                    == Some(end.line())
            })?
            .number(side(start.side()))?;
        Some(DiffLinePosition::new(
            end.path().clone(),
            start.side(),
            usize::try_from(line).ok()?,
        ))
    }

    /// Kit reports source positions independently of wrapping. A drag from its add button
    /// has no text selection; complete that gesture as a line-range comment. Text drags keep
    /// their ordinary copy behavior, and number-gutter selections stay owned by Kit.
    fn finish_comment_drag(
        &mut self,
        event: &gpui_kit::MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.comment_drag.take() else {
            return;
        };
        let (Some(start), Some(end)) = (drag.start, drag.end) else {
            return;
        };
        let selected_before = drag.selected_before;
        let moved = event.position - drag.origin;
        if self.editing
            || event.button != gpui_kit::MouseButton::Left
            || (moved.x.abs() <= gpui_kit::px(3.) && moved.y.abs() <= gpui_kit::px(3.))
            || start.path() != end.path()
            || start.side() != end.side()
            || self.diff.read(cx).selected_lines() != selected_before
            || (selected_before.is_none() && !self.diff.read(cx).selected_text(cx).is_empty())
        {
            return;
        }
        let range =
            DiffLineRange::new(start.path().clone(), start.side(), start.line(), end.line());
        self.selection = self.selection_for(&range);
        if self.selection.is_some() {
            self.diff
                .update(cx, |diff, cx| diff.set_selected_lines(Some(range), cx));
            self.start_comment(window, cx);
        }
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

    fn cancel_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        self.editing = false;
        self.diff.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn comment_annotation(&self) -> Option<DiffAnnotation> {
        let (anchor, _) = self.selection.as_ref().filter(|_| self.editing)?;
        let side = match anchor.side {
            DiffSide::Left => ViewSide::Original,
            DiffSide::Right => ViewSide::Modified,
        };
        Some(DiffAnnotation::line(
            "code-comment-editor",
            DiffLinePosition::new(
                anchor.path.clone(),
                side,
                usize::try_from(anchor.line).ok()?,
            ),
        ))
    }

    fn render_comment(&self, window: &Window, cx: &Context<Self>) -> gpui_kit::Div {
        let column = if self.diff.read(cx).mode() == gpui_kit::component::diff::DiffMode::Split {
            self.width / 2.
        } else {
            self.width
        };
        div()
            .min_w_0()
            .w_full()
            .max_w((column - window.rem_size() * 1.5).max(gpui_kit::px(0.)))
            .debug_selector(|| "git-inline-comment".into())
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .border_l_2()
            .border_color(cx.theme().primary)
            .bg(cx.theme().background)
            .capture_action(cx.listener(
                |this, enter: &gpui_kit::component::input::Enter, _, cx| {
                    if enter.secondary && !enter.shift {
                        this.save_comment(cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(
                cx.listener(|this, _: &gpui_kit::component::input::Escape, window, cx| {
                    this.cancel_comment(window, cx);
                    cx.stop_propagation();
                }),
            )
            .when_some(self.selection.as_ref(), |editor, (anchor, _)| {
                editor.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{}:{}–{}",
                            anchor.path, anchor.start_line, anchor.line
                        )),
                )
            })
            .child(
                Textarea::new(&self.comment)
                    .context_menu(crate::gpui::text_menu(&self.comment))
                    .disabled(self.submitting)
                    .aria_label("Code comment")
                    .w_full(),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if cfg!(target_os = "macos") {
                                "⌘ Enter to send"
                            } else {
                                "Ctrl Enter to send"
                            }),
                    )
                    .child(
                        Button::new("cancel-code-comment")
                            .disabled(self.submitting)
                            .label("Cancel")
                            .ghost()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel_comment(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("save-code-comment")
                            .debug_selector(|| "git-comment-submit".into())
                            .label(if self.review.is_some() {
                                "Add to review"
                            } else {
                                "Comment"
                            })
                            .primary()
                            .disabled(
                                self.submitting || self.comment.read(cx).value().trim().is_empty(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.save_comment(cx))),
                    ),
            )
    }

    fn review_controls(&self, cx: &Context<Self>) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when_some(self.review_error.as_ref(), |view, error| {
                view.child(
                    div()
                        .p_2()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
            .when_some(
                self.selection.as_ref().filter(|_| !self.editing),
                |view, (anchor, _)| {
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
                                .children(self.selection_actions(cx)),
                        )
                },
            )
    }
}

impl GitDiffPanel {
    fn render_patch(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
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
            let owner = cx.weak_entity();
            let add_owner = owner.clone();
            let hover_owner = owner.clone();
            Diff::new(&self.diff)
                .on_line_hover(move |position, _, cx| {
                    _ = hover_owner.update(cx, |this, _| {
                        if let Some(position) = position {
                            let start = this
                                .comment_drag
                                .as_ref()
                                .and_then(|drag| drag.start.clone())
                                .unwrap_or_else(|| position.clone());
                            let end = this.comment_endpoint(&start, position);
                            if let Some(drag) = &mut this.comment_drag {
                                // A leave callback for an earlier row must not erase this endpoint.
                                drag.start = Some(start);
                                drag.end = end;
                            }
                        }
                        this.hovered_line = position.cloned();
                    });
                })
                .change_indicator(DiffChangeIndicator::Bars)
                .hunk_separator(DiffHunkSeparator::LineInfo)
                .hover_highlight(DiffHoverHighlight::Both)
                .annotations(self.comment_annotation())
                .render_annotation(move |_, window, cx| {
                    owner
                        .update(cx, |this, cx| this.render_comment(window, cx))
                        .unwrap_or_else(|_| div())
                })
                .when(!self.editing, |diff| {
                    diff.on_add_annotation(move |range, window, cx| {
                        _ = add_owner.update(cx, |this, cx| {
                            this.selection = this.selection_for(range);
                            this.diff.update(cx, |diff, cx| {
                                diff.set_selected_lines(Some(range.clone()), cx);
                            });
                            this.start_comment(window, cx);
                        });
                    })
                })
                .header_visible(self.diff.read(cx).files().len() > 1)
                .line_number(true)
                .soft_wrap(self.wrap)
                .size_full()
                .into_any_element()
        }
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
        let owner = cx.weak_entity();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .when(
                self.toolbar && self.title.is_some() && self.notice.is_none(),
                |view| {
                    view.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .children(self.controls(cx.weak_entity(), cx)),
                    )
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .capture_any_mouse_down(cx.listener(
                                |this, event: &gpui_kit::MouseDownEvent, _, cx| {
                                    this.comment_drag = (event.button
                                        == gpui_kit::MouseButton::Left
                                        && !this.editing)
                                        .then(|| CommentDrag {
                                            start: this.hovered_line.clone(),
                                            end: this.hovered_line.clone(),
                                            origin: event.position,
                                            selected_before: this.diff.read(cx).selected_lines(),
                                        });
                                },
                            ))
                            .capture_any_mouse_up(cx.listener(
                                |this, event: &gpui_kit::MouseUpEvent, window, cx| {
                                    this.finish_comment_drag(event, window, cx);
                                },
                            ))
                            .on_prepaint(move |bounds, _, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    if this.width != bounds.size.width {
                                        this.width = bounds.size.width;
                                        cx.notify();
                                    }
                                });
                            })
                            .child(self.render_patch(cx)),
                    )
                    .when(
                        self.files_open && !self.diff.read(cx).files().is_empty(),
                        |body| body.child(self.files.clone()),
                    ),
            )
            .child(self.review_controls(cx))
    }
}
