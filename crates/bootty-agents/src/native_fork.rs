//! Completed-response forks and once-per-provider history bootstrap.
/* Adapted from Zeron sessions.rs and rpc.rs.
MIT License

Copyright (c) 2026 Wing

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/
use super::{
    Arc, CommandTarget, Deserialize, NativeAgentService, NativeAttachmentReference,
    NativeSessionRecord, Serialize, lock,
};
use crate::{NativeAgentSession, NativePrompt, NativeTranscriptItem};
use serde_json::json;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeSideChat {
    pub source_id: String,
    pub boundary: String,
    #[serde(default)]
    pub seeded_identity: Option<String>,
    /// Immutable copied context, independent of the bounded live transcript.
    #[serde(default)]
    pub transcript: Vec<NativeTranscriptItem>,
}

impl NativeSideChat {
    pub(crate) fn retain_copied_transcript(&mut self, recent: &[NativeTranscriptItem]) {
        if self.transcript.is_empty() {
            self.transcript = self.copied_transcript(recent).unwrap_or_default().to_vec();
        }
    }

    /// Older catalogs retained the copied prefix only in their recent transcript.
    #[must_use]
    pub fn copied_transcript<'a>(
        &'a self,
        recent: &'a [NativeTranscriptItem],
    ) -> Option<&'a [NativeTranscriptItem]> {
        if !self.transcript.is_empty() {
            return Some(&self.transcript);
        }
        recent
            .iter()
            .position(|item| item.id == self.boundary)
            .and_then(|end| recent.get(..=end))
    }
}

pub(super) struct ForkContext {
    pub origin: NativeSideChat,
    pub source: CommandTarget,
    pub attachments: Vec<NativeAttachmentReference>,
}

impl NativeAgentService {
    /// Create an independent provider session from a completed parent response.
    /// # Errors
    /// Rejects stale parents, missing completed responses, absent task membership or persistence failures.
    pub fn fork_side_chat(
        &self,
        source: &CommandTarget,
        response: Option<&str>,
        tools: Option<Arc<crate::ToolBridge>>,
    ) -> Result<NativeSessionRecord, String> {
        self.fork_side_chat_placed(source, response, tools, |_| Ok(()))
    }

    /// Publish the copied native pane after reservation, before its provider initializes.
    /// # Errors
    /// Rejects stale sources, invalid history, failed placement or provider initialization.
    pub fn fork_side_chat_placed(
        &self,
        source: &CommandTarget,
        response: Option<&str>,
        tools: Option<Arc<crate::ToolBridge>>,
        place: impl Fn(&NativeSessionRecord) -> Result<(), String>,
    ) -> Result<NativeSessionRecord, String> {
        let parent = self
            .sessions()
            .into_iter()
            .find(|record| record.target() == *source)
            .ok_or("The source conversation is stale")?;
        let prefix = parent
            .side_chat
            .as_ref()
            .and_then(|fork| fork.copied_transcript(&parent.snapshot.transcript))
            .unwrap_or_default();
        let mut transcript = prefix.to_vec();
        transcript.extend(
            parent
                .snapshot
                .transcript
                .iter()
                .filter(|item| !prefix.iter().any(|copied| copied.id == item.id))
                .cloned(),
        );
        let end = transcript.iter().rposition(|item| {
            item.role == "assistant"
                && item.complete
                && !item.text.trim().is_empty()
                && response.is_none_or(|response| response == item.id)
        });
        if let Some(end) = end {
            transcript.truncate(end.saturating_add(1));
        } else if let Some(response) = response {
            transcript = self.resolve(source)?.history_through(response)?;
        } else {
            return Err("Wait for a completed response before starting a side chat".into());
        }
        for item in &mut transcript {
            item.id = format!("fork:{}:{}", parent.id, item.id);
            for citation in &mut item.citations {
                citation.message_id = format!("fork:{}:{}", parent.id, citation.message_id);
            }
        }
        let boundary = transcript.last().ok_or("No completed response")?.id.clone();
        let history = serde_json::to_string(&transcript).map_err(|e| e.to_string())?;
        if history.len() > 2 * 1024 * 1024 {
            return Err("The conversation exceeds the side-chat context limit".into());
        }
        // Provider identity, pending approvals and application grants never cross the fork.
        let mut config = parent.config.clone();
        config.session_id = None;
        config.session_file = None;
        config.fresh_session_id = None;
        let attachment_ids = transcript
            .iter()
            .flat_map(|item| &item.attachments)
            .map(|reference| reference.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let attachments = parent
            .attachments
            .iter()
            .filter(|reference| attachment_ids.contains(reference.id.as_str()))
            .cloned()
            .collect();
        let task = parent
            .task_identity
            .as_deref()
            .ok_or("The source has no saved task")?;
        self.create_captured(
            &parent.binding_id,
            Some(task),
            "Side chat",
            config,
            tools,
            Some(super::CreationOrigin::SideChat(ForkContext {
                origin: NativeSideChat {
                    source_id: parent.id,
                    boundary,
                    seeded_identity: None,
                    transcript,
                },
                source: source.clone(),
                attachments,
            })),
            Some(&place),
        )
        .map_err(|error| error.message)
    }

    pub(super) fn side_chat_prompt(
        &self,
        target: &CommandTarget,
        prompt: &NativePrompt,
        session: &NativeAgentSession,
    ) -> Result<(NativePrompt, Option<String>), String> {
        // Leading native commands must retain their provider routing; bootstrap on a normal prompt.
        if prompt.message().trim_start().starts_with('/') {
            return Ok((prompt.clone(), None));
        }
        let store = lock(&self.store);
        let record = store
            .records
            .iter()
            .find(|record| record.target() == *target)
            .ok_or("The side chat is stale")?
            .clone();
        drop(store);
        let Some(fork) = &record.side_chat else {
            return Ok((prompt.clone(), None));
        };
        let identity = session
            .snapshot()
            .session_id
            .or_else(|| record.config.fresh_session_id.clone())
            .ok_or("The side-chat provider identity is not ready")?;
        if fork.seeded_identity.as_deref() == Some(&identity) {
            return Ok((prompt.clone(), None));
        }
        let history = fork
            .copied_transcript(&record.snapshot.transcript)
            .ok_or("Missing side-chat history")?
            .iter()
            .filter(|item| !item.text.is_empty() || item.tool.is_some() || !item.attachments.is_empty())
            .map(|item| {
                let attachments = item.attachments.iter().map(|reference| {
                    let path = self.attachment_store.reference_path(&record.id,reference)?;
                    Ok(json!({"name":reference.name,"path":path,"kind":reference.kind}))
                }).collect::<Result<Vec<_>,String>>()?;
                Ok(json!({"role":item.role,"text":item.text,"tool":item.tool,"attachments":attachments}))
            })
            .collect::<Result<Vec<_>,String>>()?;
        let history = serde_json::to_string(&history).map_err(|e| e.to_string())?;
        Ok((prompt.with_history(history)?, Some(identity)))
    }
}
