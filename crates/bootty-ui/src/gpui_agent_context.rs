//! Conversation context reuses the workspace's committed project and cached Git projection.
use super::*;
use crate::gpui::chrome::{SessionTarget, SidebarDiffSummary};

#[derive(Clone, PartialEq, Eq)]
pub struct ConversationContext {
    pub project: Option<String>,
    pub branch: Option<String>,
    pub diff: Option<SidebarDiffSummary>,
    pub target: Option<SessionTarget>,
}

impl NativeAgentSessionView {
    pub(crate) fn set_conversation_context(
        &mut self,
        context: Option<ConversationContext>,
        cx: &mut Context<Self>,
    ) {
        if self.context_card != context {
            self.context_card = context;
            cx.notify();
        }
    }

    pub(super) fn render_context_card(&self, cx: &Context<Self>) -> impl IntoElement {
        let context = self.context_card.as_ref();
        let project = context
            .and_then(|context| context.project.clone())
            .unwrap_or_else(|| "No project".into());
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
            .when(!self.record.attachments.is_empty(), |card| {
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
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Sources"),
                        )
                        .children(self.render_context_sources(cx)),
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
                    .map(|agent| Self::render_subagent_entry(agent, cx)),
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

    pub(super) fn render_context_geometry(&self, cx: &Context<Self>) -> impl IntoElement {
        let geometry_owner = cx.weak_entity();
        let wide = self.wide;
        gpui_kit::canvas(
            move |bounds, window, cx| {
                let next = bounds.size.width >= gpui_kit::rems(76.).to_pixels(window.rem_size());
                if next != wide {
                    let owner = geometry_owner;
                    cx.defer(move |cx| {
                        _ = owner.update(cx, |this, cx| {
                            if this.wide != next {
                                this.wide = next;
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

    fn render_context_sources(&self, cx: &Context<Self>) -> Vec<gpui_kit::AnyElement> {
        self.record
            .attachments
            .iter()
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
                Some(
                    Button::new(gpui_kit::SharedString::from(format!(
                        "context-source:{}",
                        attachment.id
                    )))
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
                    .on_click(cx.listener(move |this, _, _, cx| this.reveal_response(&message, cx)))
                    .into_any_element(),
                )
            })
            .collect()
    }
}
