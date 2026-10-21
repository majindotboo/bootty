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
    /// Held after the user stopped the agent, until they resume the queue or send a message.
    Paused,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeQueuedMessage {
    pub id: String,
    pub text: String,
    pub preview_text: String,
    pub attachments: Vec<crate::NativeAttachmentReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_comments: Vec<crate::NativeReviewComment>,
    pub status: NativeQueuedMessageStatus,
    /// It steers the active turn as soon as the turn can take it.
    #[serde(default)]
    pub steer: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct QueuedInput {
    pub(super) session: String,
    id: String,
    status: NativeQueuedMessageStatus,
    prompt: crate::native_prompt::SavedNativePrompt,
    /// Steer the active turn as soon as it can take the message. Until then, or once the turn
    /// ends, it waits like any queued message.
    #[serde(default)]
    steer: bool,
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
            review_comments: input.prompt.review_comments.clone(),
            status: input.status,
            steer: input.steer,
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
        self.enqueue(target, prompt, false, false)
    }

    /// Stop the running turn and send `prompt` next, ahead of anything already waiting. Unlike a
    /// plain stop, this leaves the queue running.
    /// # Errors
    /// Rejects stale targets or the bounded queue, or returns the provider's interrupt failure.
    pub fn interrupt_and_send(
        &self,
        target: &CommandTarget,
        prompt: &crate::NativePrompt,
    ) -> Result<NativeQueuedMessage, String> {
        let queued = self.enqueue(target, prompt, false, true)?;
        let session = self.resolve(target)?;
        if !session.snapshot().stopping() {
            session.interrupt()?;
        }
        Ok(queued)
    }

    /// Queue a message for the active turn, which takes it as soon as it can.
    pub(super) fn queue_steer(
        &self,
        target: &CommandTarget,
        prompt: &crate::NativePrompt,
    ) -> Result<NativeQueuedMessage, String> {
        self.enqueue(target, prompt, true, false)
    }

    fn enqueue(
        &self,
        target: &CommandTarget,
        prompt: &crate::NativePrompt,
        steer: bool,
        front: bool,
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
            review_comments: saved_prompt.review_comments.clone(),
            status: NativeQueuedMessageStatus::Queued,
            steer,
        };
        let input = QueuedInput {
            session: target.handle.clone(),
            id,
            status: info.status,
            prompt: saved_prompt,
            steer,
        };
        let position = if front {
            candidate
                .followups
                .iter()
                .position(|queued| queued.session == target.handle)
        } else {
            None
        };
        match position {
            Some(position) => candidate.followups.insert(position, input),
            None => candidate.followups.push(input),
        }
        self.commit(candidate)?;
        Ok(info)
    }

    /// Hold a conversation's waiting messages after the user stops the agent, so stopping
    /// doesn't send the next one at once. Returns the messages it held.
    /// # Errors
    /// Returns catalog persistence failures.
    pub(super) fn pause_followups(&self, target: &CommandTarget) -> Result<Vec<String>, String> {
        self.set_waiting_followups(
            target,
            |_| true,
            NativeQueuedMessageStatus::Queued,
            NativeQueuedMessageStatus::Paused,
        )
    }

    /// Queues `ids` again, of the messages a pause held.
    /// # Errors
    /// Returns catalog persistence failures.
    pub(super) fn requeue_followups(
        &self,
        target: &CommandTarget,
        ids: &[String],
    ) -> Result<(), String> {
        self.set_waiting_followups(
            target,
            |input| ids.contains(&input.id),
            NativeQueuedMessageStatus::Paused,
            NativeQueuedMessageStatus::Queued,
        )
        .map(drop)
    }

    /// Let a conversation's paused messages send again, in order, including after a failed turn.
    /// # Errors
    /// Returns catalog persistence failures.
    pub fn resume_followups(&self, target: &CommandTarget) -> Result<(), String> {
        // Resuming after a failed turn moves on from it, without restarting the provider.
        if let Ok(session) = self.resolve(target) {
            session.acknowledge_failure();
        }
        self.unpause_followups(target)
    }

    /// Queues the conversation's paused messages again.
    pub(crate) fn unpause_followups(&self, target: &CommandTarget) -> Result<(), String> {
        self.set_waiting_followups(
            target,
            |_| true,
            NativeQueuedMessageStatus::Paused,
            NativeQueuedMessageStatus::Queued,
        )
        .map(drop)
    }

    /// Moves a waiting message one place earlier or later among its conversation's messages.
    /// # Errors
    /// Rejects stale targets, a message already being sent, or one at the end it moves toward.
    pub fn move_followup(
        &self,
        target: &CommandTarget,
        id: &str,
        earlier: bool,
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
        let positions: Vec<usize> = candidate
            .followups
            .iter()
            .enumerate()
            .filter(|(_, input)| input.session == target.handle)
            .map(|(position, _)| position)
            .collect();
        let at = positions
            .iter()
            .position(|position| {
                candidate
                    .followups
                    .get(*position)
                    .is_some_and(|input| input.id == id)
            })
            .ok_or("Queued message is no longer available")?;
        let neighbor = if earlier {
            at.checked_sub(1)
        } else {
            at.checked_add(1)
        };
        let (Some(&from), Some(&to)) =
            (positions.get(at), neighbor.and_then(|at| positions.get(at)))
        else {
            return Err("This message can't move further".into());
        };
        if [from, to].iter().any(|position| {
            candidate
                .followups
                .get(*position)
                .is_some_and(|input| input.status == NativeQueuedMessageStatus::Sending)
        }) {
            return Err("A message being sent can't move".into());
        }
        candidate.followups.swap(from, to);
        self.commit(candidate)
    }

    /// Moves the conversation's messages `chosen` among those in `from` to `to`, and returns
    /// the ones it moved.
    fn set_waiting_followups(
        &self,
        target: &CommandTarget,
        chosen: impl Fn(&QueuedInput) -> bool,
        from: NativeQueuedMessageStatus,
        to: NativeQueuedMessageStatus,
    ) -> Result<Vec<String>, String> {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let mut changed = Vec::new();
        for input in candidate
            .followups
            .iter_mut()
            .filter(|input| input.session == target.handle && input.status == from && chosen(input))
        {
            input.status = to;
            changed.push(input.id.clone());
        }
        if !changed.is_empty() {
            self.commit(candidate)?;
        }
        Ok(changed)
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

    /// Promote an unsent queued message into the exact active turn. A turn that can't take it
    /// yet gets it as soon as it can, and otherwise it sends as the next turn's message.
    /// # Errors
    /// Rejects stale targets or provider rejections.
    pub fn steer_followup(&self, target: &CommandTarget, id: &str) -> Result<(), String> {
        if self.resolve(target)?.snapshot().steer_refusal().is_some() {
            return self.mark_steer(target, id);
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

    fn mark_steer(&self, target: &CommandTarget, id: &str) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let input = candidate
            .followups
            .iter_mut()
            .find(|input| input.session == target.handle && input.id == id)
            .ok_or("Queued message is no longer available")?;
        input.steer = true;
        self.commit(candidate)
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
        let Some((target, input, delivery)) = self.claim_followup() else {
            return false;
        };
        let outcome = self
            .restore_followup(&target, &input)
            .and_then(|prompt| self.prompt_input_mode(&target, &prompt, delivery));
        self.finish_followup(&input, outcome.is_ok());
        true
    }

    /// The head of one conversation's queue that can go now: any message once the conversation is
    /// idle, or a steer once the active turn can take it.
    fn claim_followup(
        &self,
    ) -> Option<(
        CommandTarget,
        QueuedInput,
        crate::native_session::NativePromptDelivery,
    )> {
        use crate::native_session::NativePromptDelivery;
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let live = lock(&self.live);
        // One message per conversation goes at a time: one being sent or held holds the rest,
        // while a failed one waits for Retry without holding anything.
        let mut held = std::collections::BTreeSet::new();
        let mut failed_turn = None;
        let claimed = candidate
            .followups
            .iter()
            .enumerate()
            .find_map(|(index, input)| {
                match input.status {
                    NativeQueuedMessageStatus::Failed => return None,
                    NativeQueuedMessageStatus::Sending | NativeQueuedMessageStatus::Paused => {
                        held.insert(&input.session);
                        return None;
                    }
                    NativeQueuedMessageStatus::Queued => {}
                }
                if !held.insert(&input.session) {
                    return None;
                }
                let record = candidate
                    .records
                    .iter()
                    .find(|record| record.id == input.session)?;
                let session = live.get(&record.id)?;
                let snapshot = session.snapshot();
                if snapshot.status == NativeSessionStatus::Idle
                    && session.config.permissions == record.config.permissions
                {
                    Some((index, NativePromptDelivery::Idle))
                } else if input.steer
                    && snapshot.status == NativeSessionStatus::Working
                    && snapshot.steer_refusal().is_none()
                {
                    Some((index, NativePromptDelivery::Steer))
                } else {
                    if snapshot.status == NativeSessionStatus::Error {
                        failed_turn.get_or_insert_with(|| record.id.clone());
                    }
                    None
                }
            });
        drop(live);
        // A failed turn holds its conversation's queue until the user resumes it.
        let Some((index, delivery)) = claimed else {
            if let Some(session) = failed_turn {
                for input in candidate.followups.iter_mut().filter(|input| {
                    input.session == session && input.status == NativeQueuedMessageStatus::Queued
                }) {
                    input.status = NativeQueuedMessageStatus::Paused;
                }
                _ = self.commit(candidate);
            }
            return None;
        };
        let input = candidate.followups.get_mut(index)?;
        input.status = NativeQueuedMessageStatus::Sending;
        let input = input.clone();
        let target = candidate
            .records
            .iter()
            .find(|record| record.id == input.session)?
            .target();
        self.commit(candidate).ok()?;
        Some((target, input, delivery))
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
