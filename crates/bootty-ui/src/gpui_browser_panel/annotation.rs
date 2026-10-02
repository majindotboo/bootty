use super::{BrowserFeedbackReady, BrowserPanel};
use bootty_browser::BrowserElement;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    input::{Textarea, TextareaState},
    notification::Notification,
};
use gpui_kit::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, Styled, WeakEntity, Window, div, prelude::*,
};

pub(super) struct AnnotationEditor {
    element: BrowserElement,
    comment: Entity<TextareaState>,
    pub(super) owner: WeakEntity<BrowserPanel>,
}

impl AnnotationEditor {
    pub(super) fn new(
        element: BrowserElement,
        owner: WeakEntity<BrowserPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let comment = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Describe the change you want")
                .auto_grow(3, 6)
        });
        cx.observe(&comment, |_, _, cx| cx.notify()).detach();
        Self {
            element,
            comment,
            owner,
        }
    }

    fn share(&self, paste: bool, window: &mut Window, cx: &mut Context<Self>) {
        let comment = self.comment.read(cx).value().to_string();
        if comment.trim().is_empty() || comment.len() > 4096 {
            return;
        }
        let feedback = self.element.feedback(&comment);
        if paste {
            _ = self
                .owner
                .update(cx, |_, cx| cx.emit(BrowserFeedbackReady(feedback)));
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(feedback));
            window.push_notification(Notification::success("Copied browser feedback"), cx);
        }
        window.close_dialog(cx);
        _ = self
            .owner
            .update(cx, |panel, cx| panel.finish_annotation(window, cx));
    }
}

impl Focusable for AnnotationEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.comment.focus_handle(cx)
    }
}

impl Render for AnnotationEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let comment = self.comment.read(cx).value();
        let can_share = !comment.trim().is_empty() && comment.len() <= 4096;
        div().flex().flex_col().gap_3()
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.element.url.clone()))
            .child(div().flex().flex_col().gap_1()
                .child(div().font_semibold().child(self.element.tag.clone()))
                .child(div().text_sm().child(self.element.selector.clone()))
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.element.text.clone())))
            .child(Textarea::new(&self.comment).aria_label("Requested change"))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Includes the page address, selected element and your comment. Pasting leaves the feedback in the selected terminal for review."))
            .child(div().flex().justify_end().gap_2()
                .child(Button::new("annotation-copy").outline().label("Copy feedback").disabled(!can_share).on_click(cx.listener(|this, _, window, cx| this.share(false, window, cx))))
                .child(Button::new("annotation-paste").primary().label("Paste into terminal").disabled(!can_share).on_click(cx.listener(|this, _, window, cx| this.share(true, window, cx)))))
    }
}
