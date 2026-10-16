//! Conversation context reuses the workspace's committed project and cached Git projection.
use super::*;
use crate::gpui::chrome::{SessionTarget, SidebarDiffSummary, SidebarPullRequest};

#[derive(Clone, PartialEq, Eq)]
pub struct ConversationContext {
    pub project: Option<String>,
    pub branch: Option<String>,
    pub pull_request: Option<SidebarPullRequest>,
    /// The binding whose host runs Git for the conversation's checkout.
    pub binding: Option<CommandTarget>,
    pub diff: Option<SidebarDiffSummary>,
    pub target: Option<SessionTarget>,
}

/// Whether the last measured conversation was wide. A new conversation starts from it, since it
/// usually opens where the last one was, rather than painting narrow and then jumping.
pub(super) struct LastWide(pub(super) bool);
impl gpui_kit::Global for LastWide {}

impl NativeAgentSessionView {
    /// The project, branch and Git binding of the conversation's session, as the workspace sees
    /// them.
    pub fn set_conversation_context(
        &mut self,
        context: Option<ConversationContext>,
        cx: &mut Context<Self>,
    ) {
        if self.context_card != context {
            self.context_card = context;
            self.refresh_branches(cx);
            cx.notify();
        }
    }

    pub(super) fn render_context_card(&self, cx: &Context<Self>) -> impl IntoElement {
        let context = self.context_card.as_ref();
        let project = context
            .and_then(|context| context.project.clone())
            .unwrap_or_else(|| "No project".into());
        let sources = self.render_context_sources(cx);
        div()
            .id("native-context-card")
            .absolute()
            .top_4()
            .right_4()
            .w(gpui_kit::rems(18.))
            .max_h_full()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.5))
            .shadow_sm()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(project),
            )
            .when_some(
                context.and_then(|context| context.branch.clone()),
                |card, branch| {
                    card.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(crate::gpui::sized_icon(
                                "git-branch",
                                crate::gpui::IconSize::Small,
                                cx.theme().muted_foreground,
                            ))
                            .child(branch),
                    )
                },
            )
            .children(
                context
                    .and_then(|context| context.pull_request.clone())
                    .map(|pull_request| Self::render_context_pull_request(pull_request, cx)),
            )
            .when_some(
                context.and_then(|context| context.target.clone()),
                |card, target| card.child(self.render_context_changes(target, cx)),
            )
            .when(!self.subagents().is_empty(), |card| {
                card.child(self.render_context_subagents(cx))
            })
            .when(
                self.related
                    .iter()
                    .any(|related| related.subagent.is_none()),
                |card| {
                    card.child(
                        div()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .pt_3()
                            .child(self.render_related_conversations(cx)),
                    )
                },
            )
            .when(!sources.is_empty(), |card| {
                card.child(
                    div()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .pt_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .debug_selector(|| "native-context-sources".into())
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Sources"),
                        )
                        .children(sources),
                )
            })
    }
    fn render_context_subagents(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Subagents"),
            )
            .children(
                self.subagents()
                    .into_iter()
                    .map(|agent| self.render_subagent_entry(agent, cx)),
            )
    }

    /// The branch's pull request: its number in the color of where it stands, then in words.
    fn render_context_pull_request(
        pull_request: SidebarPullRequest,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let tint = gpui_kit::Hsla::from(pull_request.color);
        div()
            .debug_selector(|| "native-context-pull-request".into())
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(crate::gpui::sized_icon(
                "git-pull-request",
                crate::gpui::IconSize::Small,
                tint,
            ))
            .child(
                div()
                    .text_color(tint)
                    .child(format!("#{}", pull_request.number)),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(pull_request.status),
            )
    }

    fn render_context_changes(
        &self,
        target: SessionTarget,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let diff = self.context_card.as_ref().and_then(|context| context.diff);
        Button::new("context-changes")
            .ghost()
            .small()
            .w_full()
            .icon(IconName::File)
            .accessibility_label("Open Git changes")
            .child(div().flex_1().child("Changes"))
            .when_some(diff, |button, diff| {
                button.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .text_color(cx.theme().success)
                                .child(format!("+{}", diff.added)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("−{}", diff.removed)),
                        ),
                )
            })
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(OpenNativeSession::Changes {
                    target: target.clone(),
                });
            }))
    }

    /// Wide mode for a host that knows the conversation's width before it paints, so the first
    /// frame already has the right layout.
    pub(crate) fn fit_width(
        &mut self,
        width: gpui_kit::Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let wide = width >= gpui_kit::rems(76.).to_pixels(window.rem_size());
        let beside = self.card_fits_beside(width, window.rem_size());
        if self.wide != wide || self.card_beside != beside {
            self.wide = wide;
            self.card_beside = beside;
            cx.notify();
        }
    }

    fn card_fits_beside(&self, width: gpui_kit::Pixels, rem: gpui_kit::Pixels) -> bool {
        card_fits_beside(self.presentation.width.rems(), width, rem)
    }

    /// Wide mode follows the measured width, which arrives after the first paint, for hosts that
    /// don't pass it in.
    pub(super) fn render_context_geometry(&self, cx: &Context<Self>) -> impl IntoElement {
        let geometry_owner = cx.weak_entity();
        let (wide, beside) = (self.wide, self.card_beside);
        let column = self.presentation.width.rems();
        gpui_kit::canvas(
            move |bounds, window, cx| {
                let rem = window.rem_size();
                let next = bounds.size.width >= gpui_kit::rems(76.).to_pixels(rem);
                let next_beside = card_fits_beside(column, bounds.size.width, rem);
                if next != wide || next_beside != beside {
                    let owner = geometry_owner;
                    cx.defer(move |cx| {
                        cx.set_global(LastWide(next));
                        _ = owner.update(cx, |this, cx| {
                            if this.wide != next || this.card_beside != next_beside {
                                this.wide = next;
                                this.card_beside = next_beside;
                                cx.notify();
                            }
                        });
                    });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    }

    /// Files the conversation's messages carry, each opening its message. Images already show
    /// in their messages.
    fn render_context_sources(&self, cx: &Context<Self>) -> Vec<gpui_kit::AnyElement> {
        self.record
            .attachments
            .iter()
            .filter(|attachment| attachment.kind != bootty_agents::NativeAttachmentKind::Image)
            .filter_map(|attachment| {
                let message = self
                    .record
                    .snapshot
                    .transcript
                    .iter()
                    .find(|item| {
                        item.attachments
                            .iter()
                            .any(|source| source.id == attachment.id)
                    })?
                    .id
                    .clone();
                let id = format!("context-source:{}", attachment.id);
                Some(
                    Button::new(gpui_kit::SharedString::from(id.clone()))
                        .debug_selector(move || id)
                        .ghost()
                        .small()
                        .w_full()
                        .accessibility_label(attachment.name.clone())
                        .tooltip(attachment.name.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(attachment.name.clone()),
                        )
                        .child(crate::gpui::sized_icon(
                            "paperclip",
                            crate::gpui::IconSize::Small,
                            cx.theme().muted_foreground,
                        ))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.reveal_response(&message, cx)),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// Whether the context card fits in the margin beside a centered message column `column` rems
/// wide, so the column needn't move aside for it. A full-width column leaves no margin.
fn card_fits_beside(column: Option<f32>, width: gpui_kit::Pixels, rem: gpui_kit::Pixels) -> bool {
    // The card is 18rem wide, 1rem from the edge and 1rem from the column: 20rem each side.
    column.is_some_and(|column| width >= gpui_kit::rems(column + 40.).to_pixels(rem))
}
