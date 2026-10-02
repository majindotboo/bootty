use super::{BrowserFeedbackReady, BrowserInteraction, BrowserPanel};
use bootty_browser::{AnnotationAction, AnnotationTheme, BrowserElement};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, notification::Notification};
use gpui_kit::{ClipboardItem, Context, Window};

impl BrowserPanel {
    pub(super) fn edit_annotation(
        &mut self,
        element: BrowserElement,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let theme = cx.theme();
        let colors = AnnotationTheme {
            background: u32::from(theme.popover.to_rgb()),
            foreground: u32::from(theme.popover_foreground.to_rgb()),
            muted: u32::from(theme.muted_foreground.to_rgb()),
            border: u32::from(theme.border.to_rgb()),
            primary: u32::from(theme.primary.to_rgb()),
            primary_foreground: u32::from(theme.primary_foreground.to_rgb()),
            font_size: f32::from(window.rem_size()),
            radius: f32::from(theme.radius),
        };
        let result = self
            .selected_tab()
            .and_then(|tab| tab.view.as_ref())
            .map(|view| view.show_annotation_editor(&colors));
        match result {
            Some(Ok(())) => {
                self.annotation = Some(element);
                self.interaction = BrowserInteraction::Editing;
            }
            Some(Err(error)) => {
                if let Some(tab) = self.selected_mut() {
                    tab.error = Some(error.to_string());
                }
                self.stop_annotation();
            }
            None => self.stop_annotation(),
        }
        cx.notify();
    }

    pub(super) fn receive_annotation(
        &mut self,
        id: u64,
        action: AnnotationAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if id != self.selected || !self.visible || !self.host_visible {
            return;
        }
        if action == AnnotationAction::Cancel {
            if matches!(
                self.interaction,
                BrowserInteraction::Selecting | BrowserInteraction::Editing
            ) {
                self.stop_annotation();
                cx.notify();
            }
            return;
        }
        let Some(element) = self.annotation.as_ref() else {
            return;
        };
        let current = self
            .selected_tab()
            .and_then(|tab| tab.view.as_ref())
            .and_then(|view| view.current_address().ok());
        if self.interaction != BrowserInteraction::Editing
            || current.as_deref() != Some(&element.url)
        {
            self.stop_annotation();
            cx.notify();
            return;
        }
        let (comment, paste) = match action {
            AnnotationAction::Copy(comment) => (comment, false),
            AnnotationAction::Paste(comment) => (comment, true),
            AnnotationAction::Cancel => return,
        };
        let feedback = element.feedback(&comment);
        self.stop_annotation();
        if paste {
            cx.emit(BrowserFeedbackReady(feedback));
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(feedback));
            window.push_notification(Notification::success("Copied browser feedback"), cx);
        }
        cx.notify();
    }
}
