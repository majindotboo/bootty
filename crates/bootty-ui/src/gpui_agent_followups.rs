//! Queued prompts and the current plan sit in an inset drawer attached to the shared composer
//! surface.
use super::NativeAgentSessionView;
use crate::gpui::ElementTooltip as _;
use bootty_agents::{NativeQueuedMessage, NativeQueuedMessageStatus, NativeSessionStatus};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenuItem},
    tooltip::Tooltip,
};
use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, prelude::*};

pub(super) struct QueuedMessages {
    expanded: bool,
    /// A queued message on its way to a new side chat: its id and text.
    side_chat: Option<(String, String)>,
}

impl Default for QueuedMessages {
    fn default() -> Self {
        Self {
            expanded: true,
            side_chat: None,
        }
    }
}

impl QueuedMessages {
    pub(super) const fn take_side_chat(&mut self) -> Option<(String, String)> {
        self.side_chat.take()
    }
}

/// Deliberate limit: the hover card shows a message's start, not a whole document.
const PREVIEW_CHARS: usize = 1200;

fn lucide(slug: &str) -> Icon {
    Icon::default().path(format!("icons/{slug}.svg"))
}

#[derive(Clone, Copy)]
enum QueueAction {
    Retry,
    Steer,
    Resume,
    Remove,
    Earlier,
    Later,
}

impl NativeAgentSessionView {
    /// Fills the composer with a message moved here, such as a queued message sent to a new side
    /// chat.
    pub(crate) fn set_draft(
        &mut self,
        text: &str,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        self.restore_draft(text, &[], window, cx);
        cx.notify();
    }

    pub(super) fn render_queued_messages(&self, cx: &Context<Self>) -> impl IntoElement {
        let messages = &self.record.queued_messages;
        let goal = self.render_goal(cx);
        let plan = self.render_plan(cx);
        div()
            .id("native-queue")
            .debug_selector(|| "native-queue".into())
            .min_w_0()
            .mx_3()
            .when(
                !messages.is_empty() || plan.is_some() || goal.is_some(),
                |drawer| {
                    // The composer covers the drawer's lower edge.
                    drawer
                        .mb_neg_3()
                        .pb_3()
                        .px_1()
                        .pt_1()
                        .rounded_t_lg()
                        .border_1()
                        .border_color(cx.theme().border.opacity(0.5))
                        .bg(cx.theme().muted.opacity(0.2))
                },
            )
            .children(goal)
            .children(plan)
            .when(!messages.is_empty(), |drawer| {
                drawer
                    .child(
                        Button::new("native-queued-header")
                            .debug_selector(|| "native-queued-header".into())
                            .ghost()
                            .xsmall()
                            .px_1()
                            .w_full()
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
                                            .child(
                                                if messages.iter().any(|message| {
                                                    message.status
                                                        == NativeQueuedMessageStatus::Paused
                                                }) {
                                                    "Paused"
                                                } else {
                                                    "Queued"
                                                },
                                            ),
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

    fn render_queued_message(
        &self,
        message: &NativeQueuedMessage,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let sending = message.status == NativeQueuedMessageStatus::Sending;
        let failed = message.status == NativeQueuedMessageStatus::Failed;
        let paused = message.status == NativeQueuedMessageStatus::Paused;
        let queued = self.record.queued_messages.len();
        let position = self
            .record
            .queued_messages
            .iter()
            .position(|queued| queued.id == message.id)
            .unwrap_or_default();
        // Resume lets every paused message go, so only the first one offers it.
        let first_paused = paused
            && self
                .record
                .queued_messages
                .iter()
                .find(|queued| queued.status == NativeQueuedMessageStatus::Paused)
                .is_some_and(|queued| queued.id == message.id);
        let id = message.id.clone();
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
                row.child(self.queued_previews(message))
            })
            .child(Self::queued_text(message, cx))
            .when(failed, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Couldn’t send"),
                )
                .child(Self::queued_action(message, QueueAction::Retry, false, cx))
            })
            // A message marked to steer goes in as soon as the turn can take it.
            .when(first_paused, |row| {
                row.child(Self::queued_action(message, QueueAction::Resume, false, cx))
            })
            .when(!failed && message.steer, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Steers next"),
                )
            })
            .when(
                !failed
                    && !message.steer
                    && matches!(
                        self.record.snapshot.status,
                        NativeSessionStatus::Working | NativeSessionStatus::Waiting
                    ),
                |row| row.child(Self::queued_action(message, QueueAction::Steer, false, cx)),
            )
            // Waiting messages trade places to go out in another order.
            .when(!sending && position > 0, |row| {
                row.child(Self::queued_action(
                    message,
                    QueueAction::Earlier,
                    false,
                    cx,
                ))
            })
            .when(!sending && position.saturating_add(1) < queued, |row| {
                row.child(Self::queued_action(message, QueueAction::Later, false, cx))
            })
            .child(Self::queued_action(
                message,
                QueueAction::Remove,
                sending,
                cx,
            ))
            .child(self.queued_menu(message, cx))
    }

    /// Thumbnails of a queued message's attached images; each opens the viewer on the message's
    /// images.
    fn queued_previews(&self, message: &NativeQueuedMessage) -> gpui_kit::Div {
        let images: std::rc::Rc<[crate::gpui_image_viewer::ViewedImage]> = message
            .attachments
            .iter()
            .filter_map(|attachment| {
                Some(crate::gpui_image_viewer::ViewedImage {
                    name: attachment.name.clone(),
                    preview: self.attachment_previews.get(&attachment.id)?.clone(),
                })
            })
            .collect();
        div()
            .flex()
            .flex_shrink_0()
            .gap_1()
            .children(images.iter().enumerate().map(|(index, image)| {
                let images = images.clone();
                let name = image.name.clone();
                Button::new(SharedString::from(format!(
                    "queued-preview:{}:{index}",
                    message.id
                )))
                .ghost()
                .p_0()
                .w_4()
                .h_4()
                .rounded_sm()
                .overflow_hidden()
                .accessibility_label(format!("View {name}"))
                .child(
                    gpui_kit::img(image.preview.thumbnail.clone())
                        .size_full()
                        .object_fit(gpui_kit::ObjectFit::Cover),
                )
                .on_click(move |_, window, cx| {
                    crate::gpui_image_viewer::open(images.to_vec(), index, window, cx);
                })
            }))
    }

    /// A queued message's one-line preview. Hovering shows the message wrapped in a card.
    fn queued_text(message: &NativeQueuedMessage, cx: &Context<Self>) -> impl IntoElement {
        let preview = if message.preview_text.trim().is_empty() {
            "Attachments".to_owned()
        } else {
            message
                .preview_text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let full_text = {
            let text = message.text.trim();
            match text.char_indices().nth(PREVIEW_CHARS) {
                Some((end, _)) => format!("{}…", text.get(..end).unwrap_or(text)),
                None if text.is_empty() => preview.clone(),
                None => text.to_owned(),
            }
        };
        div()
            .debug_selector({
                let id = message.id.clone();
                move || format!("native-queued-text:{id}")
            })
            .flex_1()
            .min_w_0()
            .id(SharedString::from(format!("queued-text:{}", message.id)))
            // Long messages wrap in a card instead of running across the window.
            .tooltip(move |window, cx| {
                let text = full_text.clone();
                Tooltip::element(move |_, _| {
                    div()
                        .debug_selector(|| "native-queued-preview".into())
                        .max_w(gpui_kit::rems(32.))
                        .py_2()
                        .px_1()
                        .whitespace_normal()
                        .child(text.clone())
                })
                .build(window, cx)
            })
            .text_xs()
            .text_ellipsis()
            .text_color(cx.theme().foreground.opacity(0.8))
            .child(preview)
    }

    /// Less common actions for one queued message.
    fn queued_menu(&self, message: &NativeQueuedMessage, cx: &Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let id = message.id.clone();
        let text = message.text.clone();
        let attachments = message.attachments.clone();
        let sending = message.status == NativeQueuedMessageStatus::Sending;
        // Editing never replaces a draft already in the composer.
        let editable = !sending && self.composer.read(cx).value().is_empty();
        let queueing = if self.follow_up_behavior == bootty_config::config::FollowUpBehavior::Queue
        {
            "Turn off queueing"
        } else {
            "Turn on queueing"
        };
        Button::new(SharedString::from(format!("queued-menu:{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("native-queued-menu:{id}")
            })
            .ghost()
            .xsmall()
            .icon(lucide("ellipsis"))
            .text_color(cx.theme().muted_foreground)
            .accessibility_label("More actions for this queued message")
            .dropdown_menu(move |menu, _, _| {
                let edit = {
                    let (owner, id, text, attachments) =
                        (owner.clone(), id.clone(), text.clone(), attachments.clone());
                    PopupMenuItem::new("Edit message")
                        .icon(lucide("pencil"))
                        .disabled(!editable)
                        .on_click(move |_, window, cx| {
                            _ = owner.update(cx, |this, cx| {
                                this.restore_draft(&text, &attachments, window, cx);
                                this.command("queue-cancel", vec![id.clone()], window, cx);
                            });
                        })
                };
                // A side chat starts its own attachment store, so only text can move there.
                let side_chat = attachments.is_empty().then(|| {
                    let (owner, id, text) = (owner.clone(), id.clone(), text.clone());
                    PopupMenuItem::new("Open in side chat")
                        .icon(lucide("message-circle-plus"))
                        .disabled(sending)
                        .on_click(move |_, window, cx| {
                            _ = owner.update(cx, |this, cx| {
                                this.queued_messages.side_chat = Some((id.clone(), text.clone()));
                                this.command("fork", Vec::new(), window, cx);
                            });
                        })
                });
                let toggle = PopupMenuItem::new(queueing)
                    .icon(lucide("list-ordered"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(
                            Box::new(crate::gpui_actions::InvokeCommand::new(
                                bootty_control::CommandInvocation::from_action(
                                    "agents.toggle-queueing",
                                    bootty_control::Caller::Internal,
                                ),
                            )),
                            cx,
                        );
                    });
                let menu = menu.item(edit);
                let menu = if let Some(item) = side_chat {
                    menu.item(item)
                } else {
                    menu
                };
                menu.item(toggle)
            })
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
                Some(lucide("corner-down-right")),
            ),
            QueueAction::Resume => (
                "queue-resume",
                "Send the paused messages",
                Some(lucide("play")),
            ),
            QueueAction::Remove => (
                "queue-cancel",
                "Remove queued message",
                Some(lucide("trash")),
            ),
            QueueAction::Earlier => (
                "queue-move",
                "Send this message sooner",
                Some(lucide("arrow-up")),
            ),
            QueueAction::Later => (
                "queue-move",
                "Send this message later",
                Some(lucide("arrow-down")),
            ),
        };
        let direction = match action {
            QueueAction::Earlier => Some("earlier"),
            QueueAction::Later => Some("later"),
            _ => None,
        };
        let id = message.id.clone();
        let key = direction.map_or_else(
            || operation.to_owned(),
            |direction| format!("{operation}-{direction}"),
        );
        Button::new(SharedString::from(format!("{key}:{id}")))
            .ghost()
            .xsmall()
            .debug_selector({
                let id = message.id.clone();
                move || format!("native-{key}:{id}")
            })
            .text_color(cx.theme().muted_foreground)
            .when_some(icon, Button::icon)
            .when(matches!(action, QueueAction::Retry), |button| {
                button.label("Retry")
            })
            .when(matches!(action, QueueAction::Steer), |button| {
                button.label("Steer")
            })
            .when(matches!(action, QueueAction::Resume), |button| {
                button.label("Resume")
            })
            .disabled(message.status == NativeQueuedMessageStatus::Sending)
            .loading(loading)
            .accessibility_label(label)
            .element_tooltip(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                // Resume names the conversation, not one message.
                let arguments = if operation == "queue-resume" {
                    Vec::new()
                } else {
                    std::iter::once(id.clone())
                        .chain(direction.map(str::to_owned))
                        .collect()
                };
                this.command(operation, arguments, window, cx);
            }))
    }
}
