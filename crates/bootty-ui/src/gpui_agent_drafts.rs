//! Unsent composer text survives a restart: the native service keeps it with the conversation.
use super::NativeAgentSessionView;
use bootty_control::{Caller, CommandCancellation, CommandInvocation};
use gpui_kit::{App, Context, Window};
use std::time::{Duration, Instant};

/// Deliberate limit: a draft saves this long after typing stops, so text typed in the moment
/// before quitting is lost. Save on window close too if that matters.
const DRAFT_SAVE_DELAY: Duration = Duration::from_millis(750);

impl NativeAgentSessionView {
    /// Put the saved draft back in an empty composer.
    pub(super) fn restore_saved_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(draft) = self.record.draft.clone()
            && self.composer.read(cx).value().is_empty()
        {
            self.restore_draft(&draft.text, &draft.attachments, window, cx);
        }
    }

    pub(super) fn schedule_draft_save(&mut self, cx: &Context<Self>) {
        self.draft_save = Some(cx.spawn(async move |owner, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
            _ = owner.update(cx, |this, cx| this.save_draft(cx));
        }));
    }

    /// Saves are sent in order and the newest wins, so none waits for the one before.
    fn save_draft(&self, cx: &App) {
        let composer = self.composer.read(cx);
        let mut attachments = Vec::<bootty_agents::NativeAttachmentReference>::new();
        for span in composer.tokens() {
            let id = span.token().id();
            if let Some(placed) = attachments
                .iter_mut()
                .find(|attachment| attachment.id == id.as_ref())
            {
                placed.prompt_ranges.push(span.range());
            } else if let Some(attachment) = self
                .attachments
                .iter()
                .find(|attachment| attachment.reference.id == id.as_ref())
            {
                let mut reference = attachment.reference.clone();
                reference.prompt_ranges = vec![span.range()];
                attachments.push(reference);
            }
        }
        let draft = bootty_agents::NativeDraft {
            text: composer.value().to_string(),
            attachments,
        };
        let Ok(draft) = serde_json::to_string(&draft) else {
            return;
        };
        let mut invocation = CommandInvocation::new(
            "agents.native.draft",
            vec![
                self.record.id.clone(),
                self.record.generation.to_string(),
                draft,
            ],
            Caller::Internal,
        );
        invocation.target = Some(self.record.target());
        _ = self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        );
    }
}
