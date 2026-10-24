//! Unsent composer text survives a restart: the native service keeps it with the conversation.
use super::NativeAgentSessionView;
use bootty_control::{Caller, CommandCancellation, CommandInvocation};
use gpui_kit::{App, Context, Window};
use std::time::{Duration, Instant};

/// Typing coalesces writes; closing flushes the current draft before the host exits.
const DRAFT_SAVE_DELAY: Duration = Duration::from_millis(750);

impl NativeAgentSessionView {
    /// Put the saved draft back in an empty composer.
    pub(super) fn restore_saved_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(draft) = self.record.draft.clone()
            && self.composer.read(cx).value().is_empty()
        {
            self.restore_draft(
                &draft.text,
                &draft.attachments,
                &draft.review_comments,
                window,
                cx,
            );
        }
    }

    /// The composer reports only typing as a change; text set in code, such as clearing a sent
    /// message, saves the draft here.
    pub(super) fn draft_replaced(&mut self, cx: &Context<Self>) {
        self.draft_revision = self.draft_revision.wrapping_add(1);
        self.schedule_draft_save(cx);
    }

    pub(super) fn schedule_draft_save(&mut self, cx: &Context<Self>) {
        self.draft_save = Some(cx.spawn(async move |owner, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
            _ = owner.update(cx, |this, cx| this.save_draft(cx));
        }));
    }

    /// Saves are sent in order and the newest wins, so none waits for the one before.
    fn current_draft(&self, cx: &App) -> bootty_agents::NativeDraft {
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
        bootty_agents::NativeDraft {
            text: composer.value().to_string(),
            attachments,
            review_comments: self.active_review_comments(cx),
        }
    }

    pub(crate) fn flush_draft(
        &mut self,
        cx: &Context<Self>,
    ) -> Result<Option<std::sync::mpsc::Receiver<bootty_control::CommandOutcome>>, String> {
        self.draft_save = None;
        let draft = self.current_draft(cx);
        if draft == self.record.draft.clone().unwrap_or_default() {
            return Ok(None);
        }
        self.send_draft(&draft).map(Some)
    }

    fn save_draft(&self, cx: &App) {
        _ = self.send_draft(&self.current_draft(cx));
    }

    fn send_draft(
        &self,
        draft: &bootty_agents::NativeDraft,
    ) -> Result<std::sync::mpsc::Receiver<bootty_control::CommandOutcome>, String> {
        let draft = serde_json::to_string(draft).map_err(|error| error.to_string())?;
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
        self.sender
            .submit(
                invocation,
                Instant::now()
                    .checked_add(Duration::from_secs(60))
                    .unwrap_or_else(Instant::now),
                CommandCancellation::new(),
            )
            .map_err(|error| format!("Could not submit the draft save: {error:?}"))
    }
}
