//! Review comments are inline context chips, with source and comment available on demand.
use super::NativeAgentSessionView;
use bootty_agents::{NativeReviewComment, NativeTranscriptItem};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, ThemeStyled as _,
    button::{Button, ButtonVariants as _},
    input::{InlineToken, InlineTokenContext},
    popover::Popover,
    text::TextView,
};
use gpui_kit::{
    App, Context, EntityInputHandler as _, Focusable as _, IntoElement as _, ParentElement as _,
    Pixels, Point, Styled as _, Window, div, prelude::*,
};

fn token(comment: &NativeReviewComment) -> InlineToken {
    InlineToken::new(format!("review:{}", comment.id), "[review]").with_label(comment.label())
}

pub(super) fn without_tokens(text: &str, comments: &[NativeReviewComment]) -> String {
    let mut text = text.to_owned();
    let mut comments = comments.iter().collect::<Vec<_>>();
    comments.sort_by_key(|comment| std::cmp::Reverse(comment.prompt_range.start));
    for comment in comments {
        if text.get(comment.prompt_range.clone()) == Some("[review]") {
            text.replace_range(comment.prompt_range.clone(), "");
        }
    }
    text
}

impl NativeAgentSessionView {
    pub fn add_diff_comment(
        &mut self,
        comment: &crate::gpui_git_panel::DiffComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = match bootty_agents::terminal_session_id() {
            Ok(id) => id,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.insert_review_comment(
            NativeReviewComment {
                id,
                anchor: comment.anchor.clone(),
                quote: comment.quote.clone(),
                body: comment.body.trim().to_owned(),
                prompt_range: 0..0,
            },
            window,
            cx,
        );
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
    }

    pub(super) fn insert_review_comment(
        &mut self,
        mut comment: NativeReviewComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Moving a comment into another draft gives its reference a new position.
        comment.prompt_range = 0..0;
        self.composer.update(cx, |input, cx| {
            _ = input.replace_with_token(token(&comment), window, cx);
        });
        self.review_comments.retain(|old| old.id != comment.id);
        self.review_comments.push(comment);
        self.draft_replaced(cx);
        cx.notify();
    }

    pub(super) fn restore_review_comments(
        &mut self,
        comments: &[NativeReviewComment],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.review_comments = comments.to_vec();
        self.review_preview = None;
        for comment in comments {
            self.composer.update(cx, |input, cx| {
                _ = input.replace_range_with_token(
                    comment.prompt_range.clone(),
                    token(comment),
                    window,
                    cx,
                );
            });
        }
    }

    pub(super) fn active_review_comments(&self, cx: &App) -> Vec<NativeReviewComment> {
        self.review_comments
            .iter()
            .filter_map(|comment| {
                let id = format!("review:{}", comment.id);
                self.composer
                    .read(cx)
                    .tokens()
                    .iter()
                    .find(|span| span.token().id().as_ref() == id)
                    .map(|span| {
                        let mut comment = comment.clone();
                        comment.prompt_range = span.range();
                        comment
                    })
            })
            .collect()
    }

    pub(super) fn render_review_token(
        comment: &NativeReviewComment,
        context: &InlineTokenContext,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        div()
            .id(format!("review-token:{}", comment.id))
            .role(gpui_kit::Role::Button)
            .aria_label(format!("Review comment, {}", comment.label()))
            .debug_selector(|| "review-comment-token".into())
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .h(context.line_height())
            .max_w(context.available_width())
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(if context.is_selected() {
                cx.theme().selection
            } else {
                cx.theme().muted.opacity(0.4)
            })
            .child(Icon::default().path("icons/message-circle.svg").xsmall())
            .child(
                div()
                    .min_w_0()
                    .max_w(gpui_kit::rems(16.))
                    .text_ellipsis()
                    .child(comment.label()),
            )
            .into_any_element()
    }

    pub(super) fn open_review_token(
        &mut self,
        id: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = id.strip_prefix("review:") {
            self.review_preview = Some((id.to_owned(), position));
            cx.notify();
        }
    }

    fn review_details(comment: &NativeReviewComment, cx: &App) -> gpui_kit::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .min_w_0()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .child(comment.anchor.path.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {}",
                        if comment.anchor.side == bootty_git::diff::DiffSide::Left {
                            "Original"
                        } else {
                            "Modified"
                        },
                        comment.label()
                    )),
            )
            .child(
                TextView::markdown(format!("review-body:{}", comment.id), comment.body.clone())
                    .selectable(true),
            )
            .child(
                div()
                    .id(format!("review-source:{}", comment.id))
                    .max_h_64()
                    .overflow_y_scroll()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted.opacity(0.4))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .whitespace_normal()
                    .child(comment.quote.clone()),
            )
    }

    pub(super) fn render_review_preview(&self, cx: &Context<Self>) -> Option<gpui_kit::AnyElement> {
        let (id, position) = self.review_preview.as_ref()?;
        let comment = self
            .active_review_comments(cx)
            .into_iter()
            .find(|comment| &comment.id == id)?;
        let remove = comment.id.clone();
        let surface = div()
            .id("review-comment-preview")
            .debug_selector(|| "review-comment-preview".into())
            .popover_style(cx)
            .w_96()
            .max_w_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.review_preview = None;
                this.composer.read(cx).focus_handle(cx).focus(window, cx);
                cx.notify();
            }))
            .on_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.review_preview = None;
                        this.composer.read(cx).focus_handle(cx).focus(window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }
                }),
            )
            .child(Self::review_details(&comment, cx))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("review-remove")
                            .ghost()
                            .small()
                            .label("Remove")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(comment) = this
                                    .active_review_comments(cx)
                                    .into_iter()
                                    .find(|comment| comment.id == remove)
                                {
                                    this.composer.update(cx, |input, cx| {
                                        input.set_selected_range(comment.prompt_range, cx);
                                        input.replace_text_in_range(None, "", window, cx);
                                        input.focus(window, cx);
                                    });
                                }
                                this.review_comments.retain(|comment| comment.id != remove);
                                this.review_preview = None;
                                this.draft_replaced(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("review-close")
                            .ghost()
                            .small()
                            .label("Close")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.review_preview = None;
                                this.composer.read(cx).focus_handle(cx).focus(window, cx);
                                cx.notify();
                            })),
                    ),
            );
        Some(
            gpui_kit::deferred(
                gpui_kit::base::Positioner::corner(gpui_kit::Anchor::BottomLeft, *position)
                    .occlude()
                    .child(surface),
            )
            .into_any_element(),
        )
    }

    pub(super) fn render_sent_review_comments(
        item: &NativeTranscriptItem,
        _cx: &App,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(item.review_comments.iter().map(|comment| {
                let comment = comment.clone();
                Popover::new(format!("sent-review:{}", comment.id))
                    .trigger(
                        Button::new(format!("sent-review-trigger:{}", comment.id))
                            .small()
                            .ghost()
                            .icon(Icon::default().path("icons/message-circle.svg"))
                            .label(comment.label()),
                    )
                    .content(move |_, _, cx| {
                        div()
                            .w_96()
                            .max_w_full()
                            .p_3()
                            .child(Self::review_details(&comment, cx))
                    })
            }))
    }
}
