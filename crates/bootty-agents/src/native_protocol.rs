use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::AgentKind;

/// Extract a readable provider failure without exposing its protocol envelope.
#[must_use]
pub fn native_error_message(error: &str) -> String {
    bounded_text(serde_json::from_str::<Value>(error).map_or_else(
        |_| error.to_owned(),
        |value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The provider could not complete this request.")
                .to_owned()
        },
    ))
}

/// Recognize the archive failure for exactly the captured provider identity.
#[must_use]
pub fn codex_session_archived(error: &str, session_id: Option<&str>) -> bool {
    session_id.is_some_and(|id| {
        serde_json::from_str::<Value>(error).is_ok_and(|error| {
            error.get("code").and_then(Value::as_i64) == Some(-32600)
                && error
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| {
                        message.starts_with(&format!("session {id} is archived."))
                    })
        })
    })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeProviderArchiveState {
    #[default]
    Unknown,
    Available,
    Archived,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeProviderWriterState {
    #[default]
    Available,
    Blocked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSessionStatus {
    Starting,
    Idle,
    Working,
    Waiting,
    Stopped,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeTranscriptItem {
    pub id: String,
    pub role: String,
    pub text: String,
    pub complete: bool,
    /// UTC epoch milliseconds from the provider, or first live observation when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub tool: Option<NativeToolCall>,
    #[serde(default)]
    pub subagent: Option<crate::NativeSubagent>,
    #[serde(default)]
    pub images: Vec<crate::NativeImageReference>,
    #[serde(default)]
    pub attachments: Vec<crate::NativeAttachmentReference>,
    #[serde(default)]
    pub citations: Vec<crate::NativeResponseCitation>,
    /// Apps the user mentioned in this prompt, once each, for presentation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applications: Vec<crate::NativeApplication>,
}

/// One step of the plan the agent is following.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeTodo {
    pub text: String,
    pub status: NativeTodoStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Deliberate limit: a plan keeps this many steps of at most `MAX_TODO_BYTES` each, so a runaway
/// plan can't grow the catalog. Raise it if real plans hit it.
const MAX_TODOS: usize = 64;
const MAX_TODO_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeToolStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
    Declined,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeToolCall {
    /// Provider tool name, or the logical name of an exactly identified Bootty MCP tool.
    pub name: String,
    pub input: String,
    /// Completed records an observed result; it does not infer success from output text.
    pub status: NativeToolStatus,
}

impl NativeTranscriptItem {
    /// Provider-reported context maintenance uses operation status, not assistant prose.
    #[must_use]
    pub fn compaction_status(&self) -> Option<NativeToolStatus> {
        if let Some(tool) = self
            .tool
            .as_ref()
            .filter(|tool| tool.name == "contextCompaction")
        {
            return Some(tool.status);
        }
        // Read snapshots saved before compaction became a typed operation.
        match (self.role.as_str(), self.text.as_str()) {
            ("notice", "Codex compacted the conversation's context.") => {
                Some(NativeToolStatus::Completed)
            }
            ("notice", "Compacting the conversation's context…") => {
                Some(NativeToolStatus::Running)
            }
            _ => None,
        }
    }

    /// Plain presentation, including transcripts saved before ANSI sanitation.
    #[must_use]
    pub fn display_text(&self) -> String {
        let text = if self.role == "user"
            && (!self.citations.is_empty() || !self.attachments.is_empty())
        {
            crate::native_citations::display_prompt_citations(
                &self.text,
                &self.citations,
                &self.attachments,
            )
        } else {
            self.text.clone()
        };
        let text = transcript_text(&self.role, text);
        if self.role == "user" {
            crate::native_annotations::display_user_annotations(text)
        } else if self.role == "assistant" && self.tool.is_none() && self.subagent.is_none() {
            crate::native_annotations::display_annotation_references(text)
        } else {
            text
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeAgentRequest {
    pub id: String,
    pub method: String,
    pub parameters: Value,
    #[serde(default)]
    pub(crate) from_attached_tools: bool,
    #[serde(skip)]
    pub(crate) wire_id: Value,
}

impl NativeAgentRequest {
    /// Only offer reusable choices supported by this exact Codex approval.
    #[must_use]
    pub fn approval_response(&self, decision: crate::NativeApprovalDecision) -> Option<Value> {
        use crate::NativeApprovalDecision as D;
        if self.method == "claude.permission" {
            return crate::native_claude::approval_response(&self.parameters, decision);
        }
        if self.method == crate::APPLICATION_ACCESS_METHOD {
            // An app grant lasts the conversation, so a one-call allowance is not offered.
            return (decision != D::AllowOnce).then(|| Value::String(decision.id().into()));
        }
        let value = match decision {
            D::Deny => Value::String("decline".into()),
            D::AllowOnce => Value::String("accept".into()),
            D::AllowSession => Value::String("acceptForSession".into()),
            D::AlwaysAllow => {
                if self.method != "item/commandExecution/requestApproval" {
                    return None;
                }
                let amendment = self.parameters.get("proposedExecpolicyAmendment")?;
                if amendment.as_array().is_none_or(|parts| {
                    parts.is_empty() || parts.iter().any(|part| !part.is_string())
                }) {
                    return None;
                }
                serde_json::json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":amendment}})
            }
        };
        if matches!(decision, D::AllowSession | D::AlwaysAllow)
            && !matches!(
                self.method.as_str(),
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
            )
        {
            return None;
        }
        if let Some(available) = self
            .parameters
            .get("availableDecisions")
            .and_then(Value::as_array)
            && !available.contains(&value)
        {
            return None;
        }
        Some(serde_json::json!({"decision":value}))
    }

    /// The app an agent asked to use, when this is Bootty's app access request.
    #[must_use]
    pub fn application_access(&self) -> Option<crate::NativeApplication> {
        (self.method == crate::APPLICATION_ACCESS_METHOD)
            .then(|| serde_json::from_value(self.parameters.clone()).ok())
            .flatten()
    }

    /// Presentation provenance for the captured attachment, including remote requests.
    /// This is never an authorization grant.
    #[must_use]
    pub const fn is_from_attached_tools(&self) -> bool {
        self.from_attached_tools
    }

    /// An MCP confirmation with no form fields or authentication proof to invent.
    #[must_use]
    pub fn is_mcp_approval(&self) -> bool {
        self.method == "mcpServer/elicitation/request"
            && crate::native_elicitation::is_approval(&self.parameters)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTurnOutcome {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeTurnReceipt {
    pub id: String,
    pub outcome: NativeTurnOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComputerToolStage {
    Started {
        parent: Option<String>,
    },
    Result {
        parent: Option<String>,
        images: Vec<[u8; 32]>,
    },
}

impl ComputerToolStage {
    fn parent(&self) -> Option<&str> {
        match self {
            Self::Started { parent } | Self::Result { parent, .. } => parent.as_deref(),
        }
    }
}

fn computer_image_hashes(content: &Value) -> Option<Vec<[u8; 32]>> {
    crate::native_prompt::tool_image_bytes(&[content])?;
    content
        .as_array()?
        .iter()
        .filter(|block| field(block, "type") == "image")
        .map(|block| {
            let data = if field(block, "mimeType") == "image/png" {
                field(block, "data").as_str()?
            } else {
                field(block, "url")
                    .as_str()?
                    .strip_prefix("data:image/png;base64,")?
            };
            Some(Sha256::digest(data.as_bytes()).into())
        })
        .collect()
}

/// A voice call Codex runs on a conversation's thread.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RealtimeCall {
    #[default]
    None,
    /// Started; Codex hasn't answered the offer yet.
    Awaiting,
    /// Codex answered or refused; whoever started the call hasn't taken it yet.
    Answered(Result<String, String>),
    Live,
    /// Ignore late answers until Codex confirms the prior call closed.
    Stopping,
}

impl RealtimeCall {
    /// Take Codex's answer, leaving the call live if Codex accepted it.
    pub fn take_answer(&mut self) -> Option<Result<String, String>> {
        if !matches!(self, Self::Answered(_)) {
            return None;
        }
        let Self::Answered(answer) = std::mem::take(self) else {
            return None;
        };
        if answer.is_ok() {
            *self = Self::Live;
        }
        Some(answer)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeSessionSnapshot {
    pub provider: AgentKind,
    #[serde(default)]
    pub application_mentions_supported: bool,
    #[serde(skip)]
    pub browser_access: crate::NativeBrowserAccess,
    pub session_id: Option<String>,
    /// Exact provider-owned resume path; Pi RPC reports it separately from its session UUID.
    #[serde(default)]
    pub session_file: Option<String>,
    pub turn_id: Option<String>,
    /// First accepted prompt in this process generation; later prompts cannot replace its outcome.
    #[serde(default)]
    pub first_turn: Option<NativeTurnReceipt>,
    #[serde(skip)]
    pub(crate) pi_dispatch_id: Option<String>,
    pub status: NativeSessionStatus,
    /// Unexpected remote EOF; never persisted or inferred from an error string.
    #[serde(skip)]
    pub transport_lost: bool,
    /// When an interrupt of this turn was requested. Read it through `stopping()`; the provider
    /// reader clears it when a turn ends.
    #[serde(skip)]
    pub(crate) stop_requested_at: Option<Instant>,
    /// An observed successful turn completion; imported history cannot establish this fact.
    #[serde(default)]
    pub completed_turn: bool,
    /// How many turns this conversation has completed while Bootty watched.
    #[serde(default)]
    pub completed_turns: u64,
    pub transcript: Vec<NativeTranscriptItem>,
    /// The plan the current turn reported, step by step. A new turn starts without one.
    #[serde(default)]
    pub todos: Vec<NativeTodo>,
    pub requests: Vec<NativeAgentRequest>,
    pub usage: Option<Value>,
    pub error: Option<String>,
    /// The provider refused its exclusive writer lease; history remains readable.
    #[serde(default)]
    pub provider_writer: NativeProviderWriterState,
    /// Observed provider archive state; independent of Bootty's task archive.
    #[serde(default)]
    pub provider_archive: NativeProviderArchiveState,
    pub revision: u64,
    #[serde(skip)]
    pub(crate) pending_user_item: Option<String>,
    #[serde(skip)]
    pub(crate) image_echo_count: usize,
    #[serde(skip)]
    pub(crate) history_echo_text: Option<String>,
    #[serde(skip)]
    pub(crate) tool_server: Option<String>,
    #[serde(skip)]
    pub(crate) computer_tool_server: Option<String>,
    #[serde(skip)]
    computer_tools: BTreeMap<String, ComputerToolStage>,
    /// Monotonic origin of this accepted live turn; inactive generations never restore it.
    #[serde(skip)]
    pub(crate) working_since: Option<Instant>,
    #[serde(skip)]
    pub(crate) realtime: RealtimeCall,
    #[serde(skip)]
    pub(crate) realtime_epoch: u64,
    #[serde(skip)]
    message_times: (Option<i64>, Option<i64>),
}

/// A pending request stays whole, because an approval answers with its exact input. The budget
/// keeps a burst from exhausting memory and the catalog; raise it only with a streamed store.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_PENDING_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_PENDING_REQUESTS: usize = 32;

impl NativeSessionSnapshot {
    /// Record how a turn ended; a successful one counts toward the conversation's completed turns.
    pub(crate) const fn finish_turn(&mut self, succeeded: bool) {
        self.completed_turn = succeeded;
        if succeeded {
            self.completed_turns = self.completed_turns.saturating_add(1);
        }
    }

    /// Why a new provider request can't be held for an answer, if it can't. The caller declines
    /// it through the provider's protocol instead of ending the conversation.
    pub(crate) fn request_refusal(&self, request: &Value) -> Option<&'static str> {
        let size = request.to_string().len();
        let pending = self
            .requests
            .iter()
            .map(|pending| pending.parameters.to_string().len())
            .fold(0_usize, usize::saturating_add);
        if self.requests.len() >= MAX_PENDING_REQUESTS {
            Some("Bootty declined a provider request because 32 are already waiting.")
        } else if size > MAX_REQUEST_BYTES
            || pending.saturating_add(size) > MAX_PENDING_REQUEST_BYTES
        {
            Some("Bootty declined a provider request that is too large to show.")
        } else {
            None
        }
    }

    /// Records a declined provider request where the conversation shows it.
    /// Records the part of a provider message too large to hold, which Bootty left out so the
    /// conversation goes on.
    pub(crate) fn skipped_frame(&mut self, bytes: usize) {
        let id = format!("skipped-frame-{}", self.revision);
        let text = format!(
            "Bootty left out {} MiB from {} that is too large to show. The conversation continues.",
            bytes.div_ceil(1024 * 1024),
            self.provider.display_name()
        );
        self.message(id, "notice", text, true);
        self.changed();
    }

    /// Marks where a turn ended early. The mark stays in the transcript and in its saved history.
    pub(crate) fn interrupted_turn(&mut self, turn: &str) {
        let text = if self.stopping() {
            "You interrupted this turn."
        } else {
            "This turn was interrupted."
        };
        self.message(
            format!("interrupted-{turn}"),
            "notice",
            text.to_owned(),
            true,
        );
    }

    pub(crate) fn declined_request(&mut self, id: &str, reason: String) {
        self.message(format!("declined-request-{id}"), "notice", reason, true);
        self.changed();
    }

    /// A user message the provider never confirmed, and no longer can: it isn't the message in
    /// flight, or the process that would confirm it has ended. Restored history keeps the fact,
    /// because confirmation is what marks a user message complete.
    #[must_use]
    pub fn undelivered(&self, item: &NativeTranscriptItem) -> bool {
        item.role == "user"
            && !item.complete
            && (self.pending_user_item.as_deref() != Some(item.id.as_str())
                || matches!(
                    self.status,
                    NativeSessionStatus::Stopped | NativeSessionStatus::Error
                ))
    }

    /// An interrupt was acknowledged, but the provider has not reported the turn's end yet.
    #[must_use]
    pub const fn stopping(&self) -> bool {
        self.stop_requested_at.is_some()
            && matches!(
                self.status,
                NativeSessionStatus::Working | NativeSessionStatus::Waiting
            )
    }

    /// Why the active turn can't take a steered message now. Callers keep the message queued.
    #[must_use]
    pub const fn steer_refusal(&self) -> Option<&'static str> {
        if self.stopping() {
            Some("The agent is stopping; queued messages send after it stops")
        } else if !matches!(self.status, NativeSessionStatus::Working)
            || self.pending_user_item.is_some()
        {
            Some("Wait for the previous message to reach the active turn")
        } else {
            None
        }
    }

    /// The provider reader calls this after every event: a turn that ended is no longer stopping.
    pub(crate) const fn settle_stop_request(&mut self) {
        if !matches!(
            self.status,
            NativeSessionStatus::Working | NativeSessionStatus::Waiting
        ) {
            self.stop_requested_at = None;
        }
    }

    #[must_use]
    pub const fn provider_archived(&self) -> bool {
        matches!(self.provider_archive, NativeProviderArchiveState::Archived)
    }
    #[must_use]
    pub const fn provider_writer_blocked(&self) -> bool {
        matches!(self.provider_writer, NativeProviderWriterState::Blocked)
    }

    pub(crate) fn set_provider_error(&mut self, error: &str) {
        let message = native_error_message(error);
        let blocked = self.provider == AgentKind::Codex
            && self.session_id.as_ref().is_some_and(|id| {
                message.eq_ignore_ascii_case(&format!("thread {id} already has an active writer"))
                    || message
                        .eq_ignore_ascii_case(&format!("thread {id} already has an active writer."))
            });
        self.provider_writer = if blocked {
            NativeProviderWriterState::Blocked
        } else {
            NativeProviderWriterState::Available
        };
        self.error = (!blocked).then_some(message);
        if blocked {
            self.status = NativeSessionStatus::Stopped;
            self.working_since = None;
        }
    }
    /// Elapsed live turn time; restored or inactive snapshots have no clock origin.
    #[must_use]
    pub fn working_elapsed(&self, now: Instant) -> Option<Duration> {
        (self.status == NativeSessionStatus::Working)
            .then_some(self.working_since)
            .flatten()
            .map(|started| now.saturating_duration_since(started))
    }

    pub(crate) const fn new(provider: AgentKind) -> Self {
        Self {
            provider,
            application_mentions_supported: false,
            browser_access: crate::NativeBrowserAccess::Unavailable,
            session_id: None,
            session_file: None,
            turn_id: None,
            first_turn: None,
            pi_dispatch_id: None,
            status: NativeSessionStatus::Starting,
            transport_lost: false,
            stop_requested_at: None,
            completed_turn: false,
            completed_turns: 0,
            transcript: Vec::new(),
            todos: Vec::new(),
            requests: Vec::new(),
            usage: None,
            error: None,
            provider_writer: NativeProviderWriterState::Available,
            provider_archive: NativeProviderArchiveState::Unknown,
            revision: 0,
            pending_user_item: None,
            image_echo_count: 0,
            history_echo_text: None,
            tool_server: None,
            computer_tool_server: None,
            computer_tools: BTreeMap::new(),
            working_since: None,
            realtime: RealtimeCall::None,
            realtime_epoch: 0,
            message_times: (None, None),
        }
    }

    pub(crate) fn accept_first_turn(&mut self, id: &str) {
        if self.first_turn.is_none() && !id.is_empty() && id.len() <= 8192 {
            self.first_turn = Some(NativeTurnReceipt {
                id: id.to_owned(),
                outcome: NativeTurnOutcome::Running,
            });
        }
    }

    /// A turn that ended, by completion, interrupt or steer, cannot still be running tools.
    pub(crate) fn settle_running_tools(&mut self) {
        for tool in self
            .transcript
            .iter_mut()
            .filter_map(|item| item.tool.as_mut())
        {
            if tool.status == NativeToolStatus::Running {
                tool.status = NativeToolStatus::Interrupted;
            }
        }
    }

    pub(crate) fn finish_first_turn(&mut self, id: &str, outcome: NativeTurnOutcome) {
        if let Some(receipt) = &mut self.first_turn
            && receipt.id == id
            && receipt.outcome == NativeTurnOutcome::Running
        {
            receipt.outcome = outcome;
        }
    }

    pub(crate) const fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// The transport injects live observation time; history reducers inject provider time.
    pub(crate) fn with_message_times<T>(
        &mut self,
        times: (Option<i64>, Option<i64>),
        update: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.message_times, times);
        self.message_times.1 = times.1.or(previous.1).or(times.0);
        let result = update(self);
        self.message_times = previous;
        result
    }

    pub(crate) fn message(&mut self, id: String, role: &str, text: String, complete: bool) {
        if id.is_empty() || id.len() > 8192 {
            return;
        }
        let text = transcript_text(role, text);
        if text.trim().is_empty()
            && matches!(
                role,
                "assistant" | "thinking" | "notice" | "custom" | "plan"
            )
        {
            if let Some(item) = self.transcript.iter_mut().find(|item| item.id == id) {
                item.complete = complete;
                item.created_at = item.created_at.or(self.message_times.0);
                item.updated_at = self.message_times.1.or(item.updated_at);
            }
            return;
        }
        // The host owns the visible prompt. Provider echoes may include path/citation context
        // assembled for transport, so they update identity/completion without replacing its text.
        if role == "user"
            && let Some(pending) = self.pending_user_item.clone()
            && !self
                .transcript
                .iter()
                .any(|item| item.id == id && item.id != pending)
            && let Some(item) = self.transcript.iter_mut().find(|item| item.id == pending)
        {
            if item.id != id {
                item.id.clone_from(&id);
            }
            item.complete = complete;
            item.created_at = self.message_times.0.or(item.created_at);
            item.updated_at = self.message_times.1.or(item.updated_at);
            self.pending_user_item = None;
            self.trim_transcript();
            return;
        }
        if let Some(item) = self.transcript.iter_mut().find(|item| item.id == id) {
            if item.role != "user" || role != "user" {
                item.text = text;
            }
            item.complete = complete;
            item.created_at = item.created_at.or(self.message_times.0);
            item.updated_at = self.message_times.1.or(item.updated_at);
        } else {
            // Keep a bounded recent view; the provider's durable transcript owns full history.
            if self.transcript.len() >= 256 {
                self.evict_oldest();
            }
            self.transcript.push(NativeTranscriptItem {
                id,
                role: role.to_owned(),
                text,
                complete,
                created_at: self.message_times.0,
                updated_at: self.message_times.1,
                tool: None,
                subagent: None,
                images: Vec::new(),
                attachments: Vec::new(),
                citations: Vec::new(),
                applications: Vec::new(),
            });
        }
        self.trim_transcript();
    }

    fn trim_transcript(&mut self) {
        // Full history stays provider-owned; the host retains at most 1 MiB across text and inputs.
        while self
            .transcript
            .iter()
            .flat_map(|item| {
                std::iter::once(item.text.len())
                    .chain(
                        item.tool
                            .iter()
                            .flat_map(|tool| [tool.name.len(), tool.input.len()]),
                    )
                    .chain(item.subagent.iter().flat_map(|agent| {
                        [
                            agent.id.len(),
                            agent.title.len(),
                            agent.prompt.len(),
                            agent.model.as_ref().map_or(0, String::len),
                        ]
                    }))
                    .chain(item.attachments.iter().flat_map(|attachment| {
                        [
                            attachment.id.len(),
                            attachment.name.len(),
                            attachment.mime_type.len(),
                        ]
                    }))
                    .chain(item.citations.iter().flat_map(|citation| {
                        [
                            citation.message_id.len(),
                            citation.quote.len(),
                            citation.comment.len(),
                        ]
                    }))
            })
            .fold(0_usize, usize::saturating_add)
            > RECENT_TRANSCRIPT_BYTES
        {
            self.evict_oldest();
        }
    }

    /// Drop the oldest retained item but keep user prompts: they mark turn boundaries, and a long
    /// turn must not lose the prompt that started it.
    fn evict_oldest(&mut self) {
        let index = self
            .transcript
            .iter()
            .position(|item| item.role != "user")
            .unwrap_or_default();
        self.transcript.remove(index);
    }

    pub(crate) fn tool_message(
        &mut self,
        id: String,
        name: Option<&str>,
        input: Option<String>,
        text: String,
        status: NativeToolStatus,
    ) {
        let name = name.map(|name| {
            if self.provider == AgentKind::Pi {
                self.tool_server
                    .as_deref()
                    .and_then(|server| crate::tool_bridge::logical_pi_tool_name(server, name))
                    .unwrap_or(name)
            } else {
                name
            }
        });
        let message_id = id.clone();
        self.message(id, "tool", text, status != NativeToolStatus::Running);
        if let Some(item) = self
            .transcript
            .iter_mut()
            .find(|item| item.id == message_id)
        {
            if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                let previous_input = item
                    .tool
                    .as_ref()
                    .map_or_else(String::new, |tool| tool.input.clone());
                item.tool = Some(NativeToolCall {
                    name: bounded_text(name.to_owned()),
                    input: input.map_or(previous_input, |input| bounded_tool_input(name, input)),
                    status,
                });
            } else if let Some(tool) = &mut item.tool {
                tool.status = status;
                if let Some(input) = input {
                    tool.input = bounded_tool_input(&tool.name, input);
                }
            }
        }
        self.trim_transcript();
    }

    pub(crate) fn restore_image_references(&mut self, history: &[NativeTranscriptItem]) {
        for item in &mut self.transcript {
            if let Some(saved) = history
                .iter()
                .find(|saved| saved.id == item.id && saved.role == item.role)
            {
                if item.role == "user" {
                    item.text.clone_from(&saved.text);
                }
                item.images.clone_from(&saved.images);
                item.attachments.clone_from(&saved.attachments);
                item.citations.clone_from(&saved.citations);
                item.applications.clone_from(&saved.applications);
                item.created_at = item.created_at.or(saved.created_at);
                item.updated_at = saved.updated_at.or(item.updated_at);
            }
        }
    }

    pub(crate) fn prompt_message(
        &mut self,
        id: &str,
        prompt: &crate::NativePrompt,
    ) -> Result<(), String> {
        self.history_echo_text = prompt.history_echo_text(self.provider)?;
        self.image_echo_count = prompt.image_count();
        self.message(id.to_owned(), "user", prompt.message().to_owned(), false);
        if let Some(item) = self.transcript.iter_mut().find(|item| item.id == id) {
            item.images = prompt.image_references();
            item.attachments = prompt.attachment_references();
            item.citations = prompt.citations().to_vec();
            item.applications.clear();
            for mention in prompt.applications() {
                let application = crate::NativeApplication::from(&mention.target);
                if !item.applications.contains(&application) {
                    item.applications.push(application);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn delta(&mut self, id: String, role: &str, delta: &str) {
        let text = self
            .transcript
            .iter()
            .find(|item| item.id == id)
            .map_or_else(|| delta.to_owned(), |item| format!("{}{delta}", item.text));
        self.message(id, role, text, false);
    }

    pub(crate) fn ingest(&mut self, value: &Value) {
        let accepted = self.accepts(value);
        let params = field(value, "params");
        if self.session_id.as_deref() == field(params, "threadId").as_str()
            && self.session_id.is_some()
            && matches!(
                field(value, "method").as_str(),
                Some("item/started" | "item/completed")
            )
            && self.codex_subagents(field(params, "item"), accepted)
        {
            self.changed();
        }
        if accepted {
            self.observe_computer_tool(value);
            self.codex(value);
            self.changed();
        }
    }

    pub(crate) fn observe_computer_tool(&mut self, value: &Value) {
        if self.turn_id.is_none() || self.computer_tool_server.is_none() {
            return;
        }
        let item = if self.provider == AgentKind::Pi {
            value
        } else {
            field(field(value, "params"), "item")
        };
        let id = field(
            item,
            if self.provider == AgentKind::Pi {
                "toolCallId"
            } else {
                "id"
            },
        )
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 8192);
        let kind = if self.provider == AgentKind::Pi {
            field(value, "type")
        } else {
            field(value, "method")
        };
        if matches!(kind.as_str(), Some("tool_execution_start" | "item/started"))
            && self.computer_item(item)
            && self.computer_tools.len() < 32
            && let Some(id) = id
        {
            let parent = field(item, "parentToolCallId")
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 8192)
                .map(str::to_owned);
            self.computer_tools
                .insert(id.to_owned(), ComputerToolStage::Started { parent });
        } else if matches!(kind.as_str(), Some("tool_execution_end" | "item/completed"))
            && self.computer_item(item)
            && let Some(stage) = id.and_then(|id| self.computer_tools.get_mut(id))
        {
            if stage.parent() != field(item, "parentToolCallId").as_str() {
                return;
            }
            let images = if self.provider == AgentKind::Pi {
                computer_image_hashes(field(field(item, "result"), "content")).unwrap_or_default()
            } else {
                Vec::new()
            };
            *stage = ComputerToolStage::Result {
                parent: stage.parent().map(str::to_owned),
                images,
            };
        } else if matches!(kind.as_str(), Some("turn/completed" | "agent_settled")) {
            self.computer_tools.clear();
        }
    }

    fn computer_item(&self, item: &Value) -> bool {
        if self.provider == AgentKind::Pi {
            self.computer_tool_server.as_deref().is_some_and(|server| {
                field(item, "toolName").as_str().is_some_and(|name| {
                    crate::tool_bridge::logical_pi_tool_name(server, name)
                        == Some("computer_snapshot")
                })
            })
        } else {
            field(item, "type") == "mcpToolCall"
                && field(item, "tool") == "computer_snapshot"
                && self.computer_tool_server.as_deref() == field(item, "server").as_str()
        }
    }

    pub(crate) fn computer_image_contents<'a>(&self, value: &'a Value) -> Vec<&'a Value> {
        if self.turn_id.is_none() {
            return Vec::new();
        }
        if self.provider == AgentKind::Pi {
            if field(value, "type") == "tool_execution_end"
                && self.computer_item(value)
                && field(value, "toolCallId").as_str().is_some_and(|id| {
                    self.computer_tools.get(id).is_some_and(|stage| {
                        stage.parent() == field(value, "parentToolCallId").as_str()
                    })
                })
            {
                return vec![field(field(value, "result"), "content")];
            }
            if matches!(
                field(value, "type").as_str(),
                Some("agent_end" | "turn_end")
            ) && self.pi_dispatch_id.is_some()
            {
                // Keep only bounded hashes until the owned run settles, including nested echoes.
                return field(
                    value,
                    if field(value, "type") == "agent_end" {
                        "messages"
                    } else {
                        "toolResults"
                    },
                )
                .as_array()
                .into_iter()
                .flatten()
                .filter(|message| {
                    field(message, "role") == "toolResult"
                        && self.pi_computer_image_result(message, field(message, "content"))
                })
                .map(|message| field(message, "content"))
                .collect();
            }
            let message = field(value, "message");
            let (item, content) = if field(value, "type") == "tool_execution_end" {
                (value, field(field(value, "result"), "content"))
            } else if matches!(
                field(value, "type").as_str(),
                Some("message_start" | "message_end")
            ) && field(message, "role") == "toolResult"
            {
                (message, field(message, "content"))
            } else {
                return Vec::new();
            };
            if self.pi_computer_image_result(item, content) {
                return vec![content];
            }
            return Vec::new();
        }
        if !self.accepts(value) {
            return Vec::new();
        }
        let params = field(value, "params");
        let items = if field(value, "method") == "item/completed" {
            vec![field(params, "item")]
        } else if field(value, "method") == "turn/completed" {
            field(field(params, "turn"), "items")
                .as_array()
                .into_iter()
                .flatten()
                .collect()
        } else {
            return Vec::new();
        };
        items
            .into_iter()
            .filter(|item| {
                self.computer_item(item)
                    && field(item, "id")
                        .as_str()
                        .is_some_and(|id| self.computer_tools.contains_key(id))
            })
            .map(|item| field(field(item, "result"), "content"))
            .collect()
    }

    fn pi_computer_image_result(&self, item: &Value, content: &Value) -> bool {
        let Some(images) = computer_image_hashes(content) else {
            return false;
        };
        let Some(id) = field(item, "toolCallId").as_str() else {
            return false;
        };
        if self.computer_item(item) {
            return matches!(self.computer_tools.get(id), Some(ComputerToolStage::Result { images: expected, .. }) if *expected == images);
        }
        field(item, "toolName") == "codemode"
            && images.iter().all(|image| self.computer_tools.values().any(|stage| {
                matches!(stage, ComputerToolStage::Result { parent: Some(parent), images } if parent == id && images.contains(image))
            }))
    }

    // Only the exact provider thread/turn owned by this child can change the projection.
    pub(crate) fn accepts(&self, value: &Value) -> bool {
        let params = field(value, "params");
        if self.session_id.is_none()
            || self.session_id.as_deref() != field(params, "threadId").as_str()
        {
            return false;
        }
        let method = field(value, "method").as_str().unwrap_or_default();
        if method == "thread/tokenUsage/updated" || method.starts_with("thread/realtime/") {
            return true;
        }
        // MCP requests have their own identity; the SDK cannot always correlate a turn.
        // The exact owned thread is still required, and any supplied turn must match.
        if method == "mcpServer/elicitation/request" && field(params, "turnId").is_null() {
            return true;
        }
        let turn = field(params, "turnId")
            .as_str()
            .or_else(|| field(field(params, "turn"), "id").as_str());
        if method == "turn/started" {
            // During a voice call, Codex starts the turns it delegates to the agent.
            return turn.is_some()
                && (self.turn_id.as_deref() == turn
                    || (self.turn_id.is_none()
                        && (self.status == NativeSessionStatus::Working
                            || !matches!(
                                self.realtime,
                                RealtimeCall::None | RealtimeCall::Stopping
                            ))));
        }
        turn.is_some() && turn == self.turn_id.as_deref()
    }

    pub(crate) fn history_item(&mut self, item: &Value) {
        _ = self.codex_subagents(item, true);
        self.codex(&serde_json::json!({"method":"item/completed","params":{"item":item}}));
    }

    pub(crate) fn history_turn(&mut self, turn: &Value) {
        let milliseconds = |key| {
            field(turn, key)
                .as_i64()
                .and_then(|time| time.checked_mul(1000))
        };
        self.with_message_times(
            (milliseconds("startedAt"), milliseconds("completedAt")),
            |snapshot| {
                for item in field(turn, "items").as_array().into_iter().flatten() {
                    snapshot.history_item(item);
                }
                if field(turn, "status") == "interrupted"
                    && let Some(id) = field(turn, "id").as_str()
                {
                    snapshot.interrupted_turn(id);
                }
            },
        );
    }

    fn complete_turn(&mut self, params: &Value) {
        if let Some(id) = self.turn_id.clone() {
            let outcome = match field(field(params, "turn"), "status").as_str() {
                Some("completed") => Some(NativeTurnOutcome::Succeeded),
                Some("failed") => Some(NativeTurnOutcome::Failed),
                Some("interrupted") => Some(NativeTurnOutcome::Interrupted),
                _ => None,
            };
            if let Some(outcome) = outcome {
                self.finish_first_turn(&id, outcome);
            }
            if outcome == Some(NativeTurnOutcome::Interrupted) {
                self.interrupted_turn(&id);
            }
        }
        self.working_since = None;
        self.finish_turn(field(field(params, "turn"), "status") == "completed");
        self.status = if field(field(params, "turn"), "status") == "failed" {
            self.error =
                string(field(field(field(params, "turn"), "error"), "message")).map(bounded_text);
            NativeSessionStatus::Error
        } else {
            NativeSessionStatus::Idle
        };
        self.turn_id = None;
        self.pending_user_item = None;
        self.requests.clear();
        for item in &mut self.transcript {
            item.complete = true;
        }
        if self.status == NativeSessionStatus::Error {
            for tool in self
                .transcript
                .iter_mut()
                .filter_map(|item| item.tool.as_mut())
            {
                if tool.name == "contextCompaction" && tool.status == NativeToolStatus::Running {
                    tool.status = NativeToolStatus::Failed;
                }
            }
        }
        self.settle_running_tools();
    }

    fn codex_item(&mut self, item: &Value, complete: bool) {
        let id = field(item, "id").as_str().unwrap_or("item").to_owned();
        match field(item, "type").as_str().unwrap_or_default() {
            "userMessage" => {
                let text = content_text(field(item, "content"));
                let text = delegated_request(&text).map(str::to_owned).unwrap_or(text);
                self.message(id, "user", text, complete);
            }
            "agentMessage" => self.message(
                id,
                "assistant",
                field(item, "text").as_str().unwrap_or_default().to_owned(),
                complete,
            ),
            // The summary is what Codex shows; raw reasoning stands in when there is none. The
            // completed item replaces the streamed text, which mixes both.
            "reasoning" => {
                let summary = joined_strings(field(item, "summary"));
                let text = if summary.trim().is_empty() {
                    joined_strings(field(item, "content"))
                } else {
                    summary
                };
                self.message(id, "thinking", text, complete);
            }
            "webSearch" => self.tool_message(
                id,
                Some("webSearch"),
                Some(web_search_input(item)),
                String::new(),
                codex_tool_status(item, complete),
            ),
            "imageView" => self.tool_message(
                id,
                Some("imageView"),
                Some(serde_json::json!({"path": field(item, "path")}).to_string()),
                String::new(),
                codex_tool_status(item, complete),
            ),
            "contextCompaction" => self.tool_message(
                id,
                Some("contextCompaction"),
                None,
                String::new(),
                if complete {
                    NativeToolStatus::Completed
                } else {
                    NativeToolStatus::Running
                },
            ),
            // The completed plan item is authoritative; its deltas only preview it.
            "plan" => self.message(
                id,
                "plan",
                field(item, "text").as_str().unwrap_or_default().to_owned(),
                complete,
            ),
            "commandExecution" => {
                let output = field(item, "aggregatedOutput").as_str().unwrap_or_default();
                let output = if output.is_empty() {
                    self.transcript
                        .iter()
                        .find(|item| item.id == id)
                        .map_or_else(String::new, |item| item.text.clone())
                } else {
                    output.to_owned()
                };
                self.tool_message(
                    id,
                    Some("commandExecution"),
                    string(field(item, "command")),
                    output,
                    codex_tool_status(item, complete),
                );
            }
            "fileChange" => {
                self.tool_message(
                    id,
                    Some("fileChange"),
                    item.get("changes").map(Value::to_string),
                    String::new(),
                    codex_tool_status(item, complete),
                );
            }
            "mcpToolCall" | "dynamicToolCall" => {
                let output = if field(item, "type") == "mcpToolCall" {
                    let error = field(field(item, "error"), "message").as_str();
                    error.map_or_else(
                        || content_text(field(field(item, "result"), "content")),
                        str::to_owned,
                    )
                } else {
                    content_text(field(item, "contentItems"))
                };
                self.tool_message(
                    id,
                    field(item, "tool").as_str(),
                    item.get("arguments").map(Value::to_string),
                    output,
                    codex_tool_status(item, complete),
                );
            }
            _ => {}
        }
    }

    fn codex(&mut self, value: &Value) {
        let params = field(value, "params");
        match field(value, "method").as_str().unwrap_or_default() {
            "turn/started" => {
                if let Some(id) = field(field(params, "turn"), "id").as_str() {
                    self.accept_first_turn(id);
                }
                self.turn_id = string(field(field(params, "turn"), "id"));
                self.status = NativeSessionStatus::Working;
                self.completed_turn = false;
                self.todos.clear();
            }
            "turn/plan/updated" => self.todos = codex_todos(field(params, "plan")),
            "thread/realtime/sdp" if self.realtime == RealtimeCall::Awaiting => {
                if let Some(sdp) = field(params, "sdp").as_str() {
                    self.realtime = RealtimeCall::Answered(Ok(sdp.to_owned()));
                }
            }
            "thread/realtime/error" => {
                let message = field(params, "message").as_str().map_or_else(
                    || "Codex couldn't start voice mode".to_owned(),
                    str::to_owned,
                );
                self.realtime = if self.realtime == RealtimeCall::Awaiting {
                    RealtimeCall::Answered(Err(bounded_text(message)))
                } else if self.realtime == RealtimeCall::Stopping {
                    RealtimeCall::Stopping
                } else {
                    RealtimeCall::None
                };
            }
            "thread/realtime/closed" => self.realtime = RealtimeCall::None,
            "turn/completed" => self.complete_turn(params),
            "item/agentMessage/delta"
            | "item/plan/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/commandExecution/outputDelta" => {
                let role = match field(value, "method").as_str() {
                    Some("item/agentMessage/delta") => "assistant",
                    Some("item/plan/delta") => "plan",
                    Some("item/commandExecution/outputDelta") => "tool",
                    _ => "thinking",
                };
                self.delta(
                    field(params, "itemId")
                        .as_str()
                        .unwrap_or("stream")
                        .to_owned(),
                    role,
                    field(params, "delta").as_str().unwrap_or_default(),
                );
            }
            "item/started" | "item/completed" => self.codex_item(
                field(params, "item"),
                field(value, "method") == "item/completed",
            ),
            "thread/tokenUsage/updated" => {
                let usage = field(params, "tokenUsage");
                if usage.to_string().len() <= 16 * 1024 {
                    self.usage = Some(usage.clone());
                }
            }
            // Codex reports transient failures it is already retrying; the turn continues.
            "error" if field(params, "willRetry") == true => {}
            "error" => {
                if let Some(id) = self.turn_id.clone() {
                    self.finish_first_turn(&id, NativeTurnOutcome::Failed);
                }
                self.working_since = None;
                self.error = string(field(field(params, "error"), "message")).map(bounded_text);
                self.status = NativeSessionStatus::Error;
            }
            _ => {}
        }
    }
}

fn codex_todos(plan: &Value) -> Vec<NativeTodo> {
    plan.as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| {
            let text = field(step, "step").as_str()?;
            let status = match field(step, "status").as_str() {
                Some("inProgress") => NativeTodoStatus::InProgress,
                Some("completed") => NativeTodoStatus::Completed,
                _ => NativeTodoStatus::Pending,
            };
            Some(NativeTodo {
                text: text
                    .get(..text.floor_char_boundary(MAX_TODO_BYTES))
                    .unwrap_or_default()
                    .to_owned(),
                status,
            })
        })
        .take(MAX_TODOS)
        .collect()
}

fn joined_strings(parts: &Value) -> String {
    parts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A web search as tool input: what it searched for, or the page it opened.
fn web_search_input(item: &Value) -> String {
    let action = field(item, "action");
    let queries = field(action, "queries")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" · ");
    let query = field(action, "query")
        .as_str()
        .or_else(|| field(action, "pattern").as_str())
        .or_else(|| (!queries.is_empty()).then_some(queries.as_str()))
        .or_else(|| field(item, "query").as_str());
    let mut input = serde_json::Map::new();
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        input.insert("query".into(), query.into());
    }
    if let Some(url) = field(action, "url").as_str() {
        input.insert("url".into(), url.into());
    }
    Value::Object(input).to_string()
}

fn codex_tool_status(item: &Value, complete: bool) -> NativeToolStatus {
    match field(item, "status").as_str() {
        Some("failed") => NativeToolStatus::Failed,
        Some("declined") => NativeToolStatus::Declined,
        Some("interrupted") => NativeToolStatus::Interrupted,
        Some("inProgress") => NativeToolStatus::Running,
        Some("completed") => NativeToolStatus::Completed,
        _ if complete => NativeToolStatus::Completed,
        _ => NativeToolStatus::Running,
    }
}

pub fn string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

pub fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

pub fn content_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    value.as_array().map_or_else(String::new, |items| {
        items
            .iter()
            .filter_map(|item| {
                field(item, "text")
                    .as_str()
                    .or_else(|| field(item, "thinking").as_str())
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

const RECENT_TRANSCRIPT_BYTES: usize = 1024 * 1024;

/// The words a voice call delegated to the agent. Codex wraps them with the call's transcript.
fn delegated_request(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix("<realtime_delegation>")?;
    let start = inner.find("<input>")?.saturating_add("<input>".len());
    let end = inner.find("</input>")?;
    inner.get(start..end).map(str::trim)
}

fn bounded_tool_input(name: &str, input: String) -> String {
    if name != "fileChange" {
        return bounded_text(input);
    }
    // File patches are structured JSON; truncation destroys their source-line mapping.
    // The existing transcript budget still bounds the whole retained projection.
    if input.len() <= RECENT_TRANSCRIPT_BYTES {
        return input;
    }
    // Too large to keep whole: each changed path stays, without its patch.
    let paths = serde_json::from_str::<Vec<Value>>(&input).unwrap_or_default();
    Value::from(
        paths
            .iter()
            .map(|change| serde_json::json!({"path": field(change, "path"), "diff": ""}))
            .collect::<Vec<_>>(),
    )
    .to_string()
}

/// Bootty's one shape for changed files, whichever provider made them: a `fileChange` tool whose
/// input lists each path with its unified diff hunks.
pub fn file_change_input(path: &str, diff: &str) -> String {
    serde_json::json!([{"path": path, "diff": diff}]).to_string()
}

fn transcript_text(role: &str, text: String) -> String {
    if matches!(role, "user" | "assistant" | "thinking" | "plan") {
        // Conversation text uses the aggregate transcript budget; tool output has its own cap.
        presentation_text(&text)
    } else {
        bounded_text(text)
    }
}

fn presentation_text(text: &str) -> String {
    let mut parser = anstyle_parse::Parser::<anstyle_parse::Utf8Parser>::default();
    let mut presentation = TranscriptText(String::with_capacity(text.len()));
    for byte in text.bytes() {
        parser.advance(&mut presentation, byte);
    }
    presentation.0
}

pub fn bounded_text(mut text: String) -> String {
    const LIMIT: usize = 16 * 1024;
    text = presentation_text(&text);
    if text.len() > LIMIT {
        let mut end = LIMIT;
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.truncate(end);
        text.push_str("\n[This recent transcript view truncates output after 16 KiB]");
    }
    text
}

struct TranscriptText(String);

impl anstyle_parse::Perform for TranscriptText {
    fn print(&mut self, character: char) {
        if !character.is_control() {
            self.0.push(character);
        }
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\n' | b'\r' | b'\t') {
            self.0.push(char::from(byte));
        }
    }
}
