use std::time::{Duration, Instant};

use bootty_agents::NativeBrowserAttachment;
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget,
};
use gpui_kit::component::{WindowExt as _, notification::Notification};

use super::*;

impl BrowserPanel {
    pub(super) fn render_attachment_action(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let attached = self.attachment.current().is_some_and(|attachment| {
            self.window_target.as_ref() == Some(&attachment.window)
                && self.selected == attachment.page
        });
        let label = if attached {
            "Detach page"
        } else {
            "Attach page"
        };
        Button::new("browser-attach-document")
            .label(label)
            .ghost()
            .small()
            .loading(self.attaching)
            .disabled(self.attaching || !self.attachment.supported() || self.conversation_target.is_none()
                || !attached && self.selected_tab().is_none_or(|tab| tab.view.is_none() || tab.loading))
            .accessibility_label(label)
            .tooltip(if attached { "Remove this conversation's browser read access" }
                else { "Allow this conversation to read this document; navigation and input remain unavailable" })
            .on_click(cx.listener(move |this, _, window, cx| {
                if attached {
                    if let Some(target) = this.conversation_target.clone() {
                        this.submit_attachment(target, None, window, cx);
                    }
                } else { this.attach_document(window, cx); }
            }))
            .into_any_element()
    }

    fn attach_document(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some((target, host, tab)) = self
            .conversation_target
            .clone()
            .zip(self.window_target.clone())
            .zip(self.selected_tab())
            .map(|((target, host), tab)| (target, host, tab))
        else {
            return;
        };
        let Some(view) = tab
            .view
            .as_ref()
            .filter(|_| !tab.loading && self.attachment.supported())
        else {
            return;
        };
        let (Ok(address), Ok(receiver)) =
            (view.current_address(), view.capture_credential_document())
        else {
            return;
        };
        let page = tab.id;
        let revisions = (tab.view_revision, tab.load_revision);
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        self.attaching = true;
        cx.notify();
        cx.spawn_in(window, async move |owner, cx| {
            let document =
                super::snapshot::receive(receiver, deadline, cx.background_executor()).await;
            _ = owner.update_in(cx, |this, window, cx| {
                let current = this.attachment.supported()
                    && !this.resetting
                    && this.conversation_target.as_ref() == Some(&target)
                    && this.window_target.as_ref() == Some(&host)
                    && this.tabs.iter().any(|tab| {
                        tab.id == page
                            && !tab.loading
                            && (tab.view_revision, tab.load_revision) == revisions
                            && tab
                                .view
                                .as_ref()
                                .and_then(|view| view.current_address().ok())
                                .as_ref()
                                == Some(&address)
                    });
                match document {
                    Ok(document) if current => this.submit_attachment(
                        target,
                        Some(&NativeBrowserAttachment {
                            window: host,
                            page,
                            document,
                        }),
                        window,
                        cx,
                    ),
                    _ => {
                        this.attaching = false;
                        window.push_notification(
                            Notification::error("The browser page changed. Attach it again."),
                            cx,
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn submit_attachment(
        &mut self,
        target: CommandTarget,
        attachment: Option<&NativeBrowserAttachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(encoded) = serde_json::to_string(&attachment) else {
            return;
        };
        let invocation = CommandInvocation {
            target: Some(target.clone()),
            ..CommandInvocation::new(
                "agents.native.browser-attach",
                vec![target.handle, target.generation.to_string(), encoded],
                Caller::Internal,
            )
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let Ok(receiver) = self
            .sender
            .submit(invocation, deadline, CommandCancellation::new())
        else {
            self.attaching = false;
            window.push_notification(
                Notification::error("This conversation is no longer available."),
                cx,
            );
            cx.notify();
            return;
        };
        self.attaching = true;
        cx.notify();
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            let error = match result {
                Ok(CommandOutcome::Success { .. }) => None,
                Ok(outcome) => crate::commands::command_outcome_message(&outcome),
                Err(_) => Some("This conversation is no longer available.".into()),
            };
            _ = owner.update_in(cx, |this, window, cx| {
                this.attaching = false;
                if let Some(error) = error {
                    window.push_notification(Notification::error(error), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
