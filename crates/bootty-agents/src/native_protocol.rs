use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::AgentKind;

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
}

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
    /// Plain bounded presentation, including transcripts saved before ANSI sanitation.
    #[must_use]
    pub fn display_text(&self) -> String {
        bounded_text(
            if self.role == "user" && (!self.citations.is_empty() || !self.attachments.is_empty()) {
                crate::native_citations::display_prompt_citations(
                    &self.text,
                    &self.citations,
                    &self.attachments,
                )
            } else {
                self.text.clone()
            },
        )
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
    /// An observed successful turn completion; imported history cannot establish this fact.
    #[serde(default)]
    pub completed_turn: bool,
    pub transcript: Vec<NativeTranscriptItem>,
    pub requests: Vec<NativeAgentRequest>,
    pub usage: Option<Value>,
    pub error: Option<String>,
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
    message_times: (Option<i64>, Option<i64>),
}

impl NativeSessionSnapshot {
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
            completed_turn: false,
            transcript: Vec::new(),
            requests: Vec::new(),
            usage: None,
            error: None,
            revision: 0,
            pending_user_item: None,
            image_echo_count: 0,
            history_echo_text: None,
            tool_server: None,
            computer_tool_server: None,
            computer_tools: BTreeMap::new(),
            working_since: None,
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
        let text = bounded_text(text);
        if text.trim().is_empty() && matches!(role, "assistant" | "thinking" | "notice" | "custom")
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
                self.transcript.remove(0);
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
            self.transcript.remove(0);
        }
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
        if method == "thread/tokenUsage/updated" {
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
            return turn.is_some()
                && (self.turn_id.as_deref() == turn
                    || (self.turn_id.is_none() && self.status == NativeSessionStatus::Working));
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
        }
        self.working_since = None;
        self.completed_turn = field(field(params, "turn"), "status") == "completed";
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
    }

    fn codex_item(&mut self, item: &Value, complete: bool) {
        let id = field(item, "id").as_str().unwrap_or("item").to_owned();
        match field(item, "type").as_str().unwrap_or_default() {
            "userMessage" => {
                self.message(id, "user", content_text(field(item, "content")), complete);
            }
            "agentMessage" => self.message(
                id,
                "assistant",
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
            }
            "turn/completed" => self.complete_turn(params),
            "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/commandExecution/outputDelta" => {
                let role = if field(value, "method") == "item/agentMessage/delta" {
                    "assistant"
                } else if field(value, "method") == "item/commandExecution/outputDelta" {
                    "tool"
                } else {
                    "thinking"
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

fn bounded_tool_input(name: &str, input: String) -> String {
    // File patches are structured JSON; truncation destroys their source-line mapping.
    // The existing transcript budget still bounds the whole retained projection.
    if name == "fileChange" && input.len() <= RECENT_TRANSCRIPT_BYTES {
        input
    } else {
        bounded_text(input)
    }
}

pub fn bounded_text(mut text: String) -> String {
    const LIMIT: usize = 16 * 1024;
    let mut parser = anstyle_parse::Parser::<anstyle_parse::Utf8Parser>::default();
    let mut presentation = TranscriptText(String::with_capacity(text.len()));
    for byte in text.bytes() {
        parser.advance(&mut presentation, byte);
    }
    text = presentation.0;
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
