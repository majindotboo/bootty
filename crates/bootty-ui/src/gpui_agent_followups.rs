//! Queued prompts sit in an inset drawer attached to the shared composer surface.
use super::NativeAgentSessionView;
use bootty_agents::{NativeQueuedMessage, NativeQueuedMessageStatus, NativeSessionStatus};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    tooltip::Tooltip,
};
use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, prelude::*};

pub(super) struct QueuedMessages {
    expanded: bool,
}

impl Default for QueuedMessages {
    fn default() -> Self {
        Self { expanded: true }
    }
}

#[derive(Clone, Copy)]
enum QueueAction {
    Retry,
    Steer,
    Remove,
}

impl NativeAgentSessionView {
    pub(super) fn render_queued_messages(&self, cx: &Context<Self>) -> impl IntoElement {
        let messages = &self.record.queued_messages;
        div()
            .id("native-queue")
            .debug_selector(|| "native-queue".into())
            .min_w_0()
            .mx_3()
            .when(!messages.is_empty(), |drawer| {
                // Like T3's attachment seam, the composer covers the drawer's lower edge.
                drawer
                    .mb_neg_3()
                    .pb_3()
                    .px_1()
                    .pt_1()
                    .rounded_t_lg()
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.5))
                    .bg(cx.theme().muted.opacity(0.2))
                    .child(
                        Button::new("native-queued-header")
                            .debug_selector(|| "native-queued-header".into())
                            .ghost()
                            .xsmall()
                            .px_1()
                            .w_full()
                            .justify_start()
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(div().w_6().flex().justify_center().child(
                                        crate::gpui::sized_icon(
                                            "list-ordered",
                                            crate::gpui::IconSize::Small,
                                            cx.theme().muted_foreground,
                                        ),
                                    ))
                                    .child(
                                        div()
                                            .id("native-queue-label")
                                            .debug_selector(|| "native-queue-label".into())
                                            .flex_1()
                                            .truncate()
                                            .child(self.queue_label()),
                                    )
                                    .child(messages.len().to_string())
                                    .child(if self.queued_messages.expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    }),
                            )
                            .accessibility_label(if self.queued_messages.expanded {
                                "Collapse queued messages"
                            } else {
                                "Expand queued messages"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.queued_messages.expanded = !this.queued_messages.expanded;
                                cx.notify();
                            })),
                    )
                    .when(self.queued_messages.expanded, |drawer| {
                        drawer.child(
                            div()
                                .id("native-queued-list")
                                .max_h_32()
                                .overflow_y_scroll()
                                .children(
                                    messages
                                        .iter()
                                        .map(|message| self.render_queued_message(message, cx)),
                                ),
                        )
                    })
            })
    }

    /// Says when queued messages will go out, so a waiting queue never looks stuck.
    fn queue_label(&self) -> String {
        let snapshot = &self.record.snapshot;
        let provider = self.record.config.provider.display_name();
        match snapshot.status {
            _ if snapshot.stopping() => format!("Queued · sends after {provider} stops"),
            NativeSessionStatus::Working | NativeSessionStatus::Waiting => {
                "Queued · sends after this turn".to_owned()
            }
            NativeSessionStatus::Stopped | NativeSessionStatus::Error => {
                "Queued · sends after the conversation resumes".to_owned()
            }
            NativeSessionStatus::Starting | NativeSessionStatus::Idle => "Queued".to_owned(),
        }
    }

    fn render_queued_message(
        &self,
        message: &NativeQueuedMessage,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let sending = message.status == NativeQueuedMessageStatus::Sending;
        let failed = message.status == NativeQueuedMessageStatus::Failed;
        let id = message.id.clone();
        let preview = if message.preview_text.trim().is_empty() {
            "Attachments".to_owned()
        } else {
            message
                .preview_text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let full_preview = preview.clone();
        div()
            .id(SharedString::from(format!("queued-row:{id}")))
            .debug_selector(move || format!("native-queued-row:{id}"))
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .px_1()
            .py_1()
            .rounded_sm()
            .hover(|row| row.bg(cx.theme().muted.opacity(0.35)))
            .child(div().w_6().flex_shrink_0())
            .when(!message.attachments.is_empty(), |row| {
                row.child(div().flex().flex_shrink_0().gap_1().children(
                    message.attachments.iter().filter_map(|attachment| {
                        self.attachment_previews.get(&attachment.id).map(|preview| {
                            let preview = preview.clone();
                            let name = attachment.name.clone();
                            Button::new(SharedString::from(format!(
                                "queued-preview:{}:{}",
                                message.id, attachment.id
                            )))
                            .ghost()
                            .p_0()
                            .w_4()
                            .h_4()
                            .rounded_sm()
                            .overflow_hidden()
                            .accessibility_label(format!("View {name}"))
                            .child(
                                gpui_kit::img(preview.clone())
                                    .size_full()
                                    .object_fit(gpui_kit::ObjectFit::Cover),
                            )
                            .on_click(move |_, window, cx| {
                                crate::gpui_prompt_attachments::open_image(
                                    name.clone(),
                                    preview.clone(),
                                    window,
                                    cx,
                                );
                            })
                        })
                    }),
                ))
            })
            .child(
                div()
                    .debug_selector({
                        let id = message.id.clone();
                        move || format!("native-queued-text:{id}")
                    })
                    .flex_1()
                    .min_w_0()
                    .id(SharedString::from(format!("queued-text:{}", message.id)))
                    .tooltip(move |window, cx| Tooltip::new(full_preview.clone()).build(window, cx))
                    .text_xs()
                    .text_ellipsis()
                    .text_color(cx.theme().foreground.opacity(0.8))
                    .child(preview),
            )
            .when(failed, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Couldn’t send"),
                )
                .child(Self::queued_action(message, QueueAction::Retry, false, cx))
            })
            .when(
                !failed && self.record.snapshot.steer_refusal().is_none(),
                |row| row.child(Self::queued_action(message, QueueAction::Steer, false, cx)),
            )
            .child(Self::queued_action(
                message,
                QueueAction::Remove,
                sending,
                cx,
            ))
    }

    /// Queued rows leave when the queue drains, often under a resting pointer.
    fn queued_action(
        message: &NativeQueuedMessage,
        action: QueueAction,
        loading: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (operation, label, icon) = match action {
            QueueAction::Retry => ("queue-retry", "Retry sending this message", None),
            QueueAction::Steer => (
                "queue-steer",
                "Steer with this queued message",
                Some(IconName::ArrowUp),
            ),
            QueueAction::Remove => (
                "queue-cancel",
                "Remove queued message",
                Some(IconName::Close),
            ),
        };
        let id = message.id.clone();
        let button = Button::new(SharedString::from(format!("{operation}:{id}")))
            .ghost()
            .xsmall()
            .debug_selector({
                let id = message.id.clone();
                move || format!("native-{operation}:{id}")
            })
            .text_color(cx.theme().muted_foreground)
            .when_some(icon, Button::icon)
            .when(matches!(action, QueueAction::Retry), |button| {
                button.label("Retry")
            })
            .when(matches!(action, QueueAction::Steer), |button| {
                button.label("Steer")
            })
            .disabled(message.status == NativeQueuedMessageStatus::Sending)
            .loading(loading)
            .accessibility_label(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.command(operation, vec![id.clone()], window, cx);
            }));
        super::element_tooltip(
            SharedString::from(format!("{operation}-tooltip:{}", message.id)),
            label,
            button,
        )
    }
}
