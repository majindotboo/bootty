//! Durable follow-ups belong to the conversation owner, independently of its views.
use super::{NativeAgentService, NativeSessionStatus, Store, lock};
use bootty_control::CommandTarget;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeQueuedMessageStatus {
    Queued,
    Sending,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeQueuedMessage {
    pub id: String,
    pub text: String,
    pub preview_text: String,
    pub attachments: Vec<crate::NativeAttachmentReference>,
    pub status: NativeQueuedMessageStatus,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct QueuedInput {
    pub(super) session: String,
    id: String,
    status: NativeQueuedMessageStatus,
    prompt: crate::native_prompt::SavedNativePrompt,
}

#[derive(Default)]
pub(super) struct Wake {
    started: AtomicBool,
    revision: Mutex<u64>,
    changed: Condvar,
}

impl Wake {
    pub(super) fn notify(&self) {
        let mut revision = lock(&self.revision);
        *revision = revision.wrapping_add(1);
        drop(revision);
        self.changed.notify_all();
    }
}

pub(super) fn restore(store: &mut Store) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for input in &mut store.followups {
        let count = counts.entry(&input.session).or_default();
        *count = count.saturating_add(1);
        if input.id.is_empty()
            || *count > 16
            || !ids.insert(&input.id)
            || !store
                .records
                .iter()
                .any(|record| record.id == input.session)
            || input.prompt.text.len() > crate::MAX_NATIVE_PROMPT_TEXT_BYTES
        {
            return Err("Invalid queued conversation input".into());
        }
        // A crash can leave submission ambiguous. Preserve it for an explicit
        // retry instead of sending the same instruction a second time.
        if input.status == NativeQueuedMessageStatus::Sending {
            input.status = NativeQueuedMessageStatus::Failed;
        }
    }
    Ok(())
}

pub(super) fn projection(store: &Store, session: &str) -> Vec<NativeQueuedMessage> {
    store
        .followups
        .iter()
        .filter(|input| input.session == session)
        .map(|input| NativeQueuedMessage {
            id: input.id.clone(),
            text: input.prompt.text.clone(),
            preview_text: input.prompt.preview_text(),
            attachments: input.prompt.attachments.clone(),
            status: input.status,
        })
        .collect()
}

impl NativeAgentService {
    /// Run queued messages when their exact conversation becomes idle. No UI or
    /// publication thread waits for provider input, and no timer polls for completion.
    /// # Errors
    /// Returns inability to start the conversation input worker.
    pub fn start_followups(self: &Arc<Self>) -> Result<(), String> {
        if self.followup_wake.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let owner = Arc::downgrade(self);
        let wake = Arc::clone(&self.followup_wake);
        std::thread::Builder::new()
            .name("native-agent-followups".into())
            .spawn(move || {
                loop {
                    let observed = *lock(&wake.revision);
                    let Some(service) = owner.upgrade() else {
                        return;
                    };
                    if service.shutdown.load(Ordering::Acquire) {
                        return;
                    }
                    let delivered = service.send_next_followup();
                    drop(service);
                    if delivered {
                        continue;
                    }
                    let revision = lock(&wake.revision);
                    drop(
                        wake.changed
                            .wait_while(revision, |revision| *revision == observed)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                }
            })
            .map_err(|error| {
                self.followup_wake.started.store(false, Ordering::Release);
                error.to_string()
            })?;
        Ok(())
    }

    /// Save accepted input before clearing the caller's draft.
    /// # Errors
    /// Rejects stale targets, invalid citations, or the bounded queue/catalog budget.
    pub fn queue_input(
        &self,
        target: &CommandTarget,
        prompt: &crate::NativePrompt,
    ) -> Result<NativeQueuedMessage, String> {
        let session = self.resolve(target)?;
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let record = candidate
            .records
            .iter()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is stale")?;
        Self::validate_prompt_citations(record, prompt, &session)?;
        if !matches!(
            session.snapshot().status,
            NativeSessionStatus::Idle | NativeSessionStatus::Working | NativeSessionStatus::Waiting
        ) {
            return Err("Resume this conversation before queuing input".into());
        }
        // Keep at most sixteen accepted messages per conversation. The existing
        // private catalog byte limit also bounds retained annotation images.
        if candidate
            .followups
            .iter()
            .filter(|input| input.session == target.handle)
            .count()
            >= 16
        {
            return Err("This conversation already has sixteen queued messages".into());
        }
        let id = crate::terminal_session_id()?;
        let saved_prompt = crate::native_prompt::SavedNativePrompt::capture(prompt);
        let info = NativeQueuedMessage {
            id: id.clone(),
            text: prompt.message().into(),
            preview_text: saved_prompt.preview_text(),
            attachments: saved_prompt.attachments.clone(),
            status: NativeQueuedMessageStatus::Queued,
        };
        candidate.followups.push(QueuedInput {
            session: target.handle.clone(),
            id,
            status: info.status,
            prompt: saved_prompt,
        });
        self.commit(candidate)?;
        Ok(info)
    }

    /// Remove only an unsent queued message.
    /// # Errors
    /// Rejects stale targets or a message already being delivered.
    pub fn cancel_followup(&self, target: &CommandTarget, id: &str) -> Result<(), String> {
        self.update_followup(target, id, None)
    }

    /// Explicitly retry retained input after a failed or interrupted delivery.
    /// # Errors
    /// Rejects stale targets or a message already being delivered.
    pub fn retry_followup(&self, target: &CommandTarget, id: &str) -> Result<(), String> {
        self.update_followup(target, id, Some(NativeQueuedMessageStatus::Queued))
    }

    /// Promote an unsent queued message into the exact active turn.
    /// # Errors
    /// Rejects stale targets, missing active turns or provider rejections.
    pub fn steer_followup(&self, target: &CommandTarget, id: &str) -> Result<(), String> {
        // A refusal known before sending keeps the message queued for its normal turn.
        if let Some(refusal) = self.resolve(target)?.snapshot().steer_refusal() {
            return Err(refusal.to_owned());
        }
        let input = {
            let _mutation = lock(&self.mutation);
            let mut candidate = lock(&self.store).clone();
            if !candidate
                .records
                .iter()
                .any(|record| record.target() == *target)
            {
                return Err("Native session target is stale".into());
            }
            let input = candidate
                .followups
                .iter_mut()
                .find(|input| input.session == target.handle && input.id == id)
                .ok_or("Queued message is no longer available")?;
            if input.status == NativeQueuedMessageStatus::Sending {
                return Err("This message is already being sent".into());
            }
            input.status = NativeQueuedMessageStatus::Sending;
            let input = input.clone();
            self.commit(candidate)?;
            input
        };
        let outcome = self.restore_followup(target, &input).and_then(|prompt| {
            self.prompt_input_mode(
                target,
                &prompt,
                crate::native_session::NativePromptDelivery::Steer,
            )
        });
        self.finish_followup(&input, outcome.is_ok());
        outcome.map(|_| ())
    }

    fn update_followup(
        &self,
        target: &CommandTarget,
        id: &str,
        status: Option<NativeQueuedMessageStatus>,
    ) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        if !candidate
            .records
            .iter()
            .any(|record| record.target() == *target)
        {
            return Err("Native session target is stale".into());
        }
        let input = candidate
            .followups
            .iter_mut()
            .find(|input| input.session == target.handle && input.id == id)
            .ok_or("Queued message is no longer available")?;
        if input.status == NativeQueuedMessageStatus::Sending {
            return Err("This message is already being sent".into());
        }
        if let Some(status) = status {
            input.status = status;
        } else {
            candidate
                .followups
                .retain(|input| input.session != target.handle || input.id != id);
        }
        self.commit(candidate)
    }

    fn send_next_followup(&self) -> bool {
        let Some((target, input)) = self.claim_followup() else {
            return false;
        };
        let outcome = self
            .restore_followup(&target, &input)
            .and_then(|prompt| self.prompt_idle_input(&target, &prompt));
        self.finish_followup(&input, outcome.is_ok());
        true
    }

    fn claim_followup(&self) -> Option<(CommandTarget, QueuedInput)> {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let live = lock(&self.live);
        let mut seen = std::collections::BTreeSet::new();
        let index = candidate.followups.iter().position(|input| {
            seen.insert(&input.session)
                && input.status == NativeQueuedMessageStatus::Queued
                && candidate.records.iter().any(|record| {
                    record.id == input.session
                        && live.get(&record.id).is_some_and(|session| {
                            session.snapshot().status == NativeSessionStatus::Idle
                                && session.config.permissions == record.config.permissions
                        })
                })
        })?;
        drop(live);
        let input = candidate.followups.get_mut(index)?;
        input.status = NativeQueuedMessageStatus::Sending;
        let input = input.clone();
        let target = candidate
            .records
            .iter()
            .find(|record| record.id == input.session)?
            .target();
        self.commit(candidate).ok()?;
        Some((target, input))
    }

    fn restore_followup(
        &self,
        target: &CommandTarget,
        input: &QueuedInput,
    ) -> Result<crate::NativePrompt, String> {
        let ids = input
            .prompt
            .attachments
            .iter()
            .map(|attachment| attachment.id.clone())
            .collect::<Vec<_>>();
        let attachments = self.resolve_prompt_attachments(target, &ids)?;
        input.prompt.restore(attachments)
    }

    fn finish_followup(&self, sent: &QueuedInput, accepted: bool) {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        if accepted {
            candidate.followups.retain(|input| input.id != sent.id);
        } else if let Some(input) = candidate
            .followups
            .iter_mut()
            .find(|input| input.id == sent.id)
        {
            input.status = NativeQueuedMessageStatus::Failed;
        }
        if let Err(error) = self.commit(candidate) {
            eprintln!("Queued conversation input could not be saved: {error}");
        }
    }
}
