use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeAgentRequest {
    pub id: String,
    pub method: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeSessionSnapshot {
    pub provider: AgentKind,
    pub session_id: Option<String>,
    pub session_file: Option<String>,
    pub turn_id: Option<String>,
    pub status: NativeSessionStatus,
    pub transcript: Vec<NativeTranscriptItem>,
    pub requests: Vec<NativeAgentRequest>,
    pub usage: Option<Value>,
    pub account: Option<Value>,
    pub error: Option<String>,
    pub revision: u64,
    #[serde(skip)]
    pub(crate) pending_user_item: Option<String>,
}

impl NativeSessionSnapshot {
    pub(crate) const fn new(provider: AgentKind) -> Self {
        Self {
            provider,
            session_id: None,
            session_file: None,
            turn_id: None,
            status: NativeSessionStatus::Starting,
            transcript: Vec::new(),
            requests: Vec::new(),
            usage: None,
            account: None,
            error: None,
            revision: 0,
            pending_user_item: None,
        }
    }

    pub(crate) const fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn message(&mut self, id: String, role: &str, text: String, complete: bool) {
        let text = bounded_text(text);
        // Replace only this dispatch's provisional message with its provider echo. Identical
        // prompts in independent turns retain independent identities.
        if role == "user"
            && self
                .pending_user_item
                .as_ref()
                .is_some_and(|pending| pending != &id)
            && let Some(item) = self
                .transcript
                .iter_mut()
                .find(|item| self.pending_user_item.as_ref() == Some(&item.id) && item.text == text)
        {
            item.id.clone_from(&id);
            self.pending_user_item = None;
        }
        if let Some(item) = self.transcript.iter_mut().find(|item| item.id == id) {
            item.text = text;
            item.complete = complete;
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
            });
        }
    }

    fn delta(&mut self, id: String, role: &str, delta: &str) {
        let text = self
            .transcript
            .iter()
            .find(|item| item.id == id)
            .map_or_else(|| delta.to_owned(), |item| format!("{}{delta}", item.text));
        self.message(id, role, text, false);
    }

    pub(crate) fn ingest(&mut self, value: &Value) {
        match self.provider {
            AgentKind::Codex => self.codex(value),
            AgentKind::Claude => self.claude(value),
            AgentKind::Pi => self.pi(value),
        }
        self.changed();
    }

    fn codex(&mut self, value: &Value) {
        let params = field(value, "params");
        match field(value, "method").as_str().unwrap_or_default() {
            "thread/started" => self.session_id = string(field(field(params, "thread"), "id")),
            "turn/started" => {
                self.turn_id = string(field(field(params, "turn"), "id"));
                self.status = NativeSessionStatus::Working;
            }
            "turn/completed" => {
                self.status = if field(field(params, "turn"), "status") == "failed" {
                    self.error = string(field(field(field(params, "turn"), "error"), "message"));
                    NativeSessionStatus::Error
                } else {
                    NativeSessionStatus::Idle
                };
                self.turn_id = None;
            }
            "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta" => {
                let role = if field(value, "method") == "item/agentMessage/delta" {
                    "assistant"
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
            "item/started" | "item/completed" => {
                let item = field(params, "item");
                let complete = field(value, "method") == "item/completed";
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
                    "commandExecution" => self.message(
                        id,
                        "tool",
                        format!(
                            "{}\n{}",
                            field(item, "command").as_str().unwrap_or_default(),
                            field(item, "aggregatedOutput").as_str().unwrap_or_default()
                        ),
                        complete,
                    ),
                    "fileChange" => {
                        self.message(id, "change", field(item, "changes").to_string(), complete);
                    }
                    _ => {}
                }
            }
            "thread/tokenUsage/updated" => self.usage = Some(field(params, "tokenUsage").clone()),
            "account/rateLimits/updated" => self.usage = Some(params.clone()),
            "account/updated" => self.account = Some(params.clone()),
            "error" => {
                self.error = string(field(field(params, "error"), "message"));
                self.status = NativeSessionStatus::Error;
            }
            _ => {}
        }
    }

    fn claude(&mut self, value: &Value) {
        if let Some(id) = string(field(value, "session_id")) {
            self.session_id = Some(id);
        }
        match field(value, "type").as_str().unwrap_or_default() {
            "system" if field(value, "subtype") == "init" => {
                if self.status == NativeSessionStatus::Starting {
                    self.status = NativeSessionStatus::Idle;
                }
            }
            "stream_event" => {
                self.status = NativeSessionStatus::Working;
                let event = field(value, "event");
                if field(event, "type") == "content_block_delta" {
                    let role = if field(field(event, "delta"), "type") == "thinking_delta" {
                        "thinking"
                    } else {
                        "assistant"
                    };
                    let text = field(field(event, "delta"), "text")
                        .as_str()
                        .or_else(|| field(field(event, "delta"), "thinking").as_str())
                        .unwrap_or_default();
                    let id = format!(
                        "claude-{}-{}-{}",
                        self.revision,
                        field(value, "parent_tool_use_id"),
                        field(event, "index")
                    );
                    // Partial events precede the final assistant record; use one current block.
                    let id = self
                        .transcript
                        .last()
                        .filter(|item| !item.complete && item.role == role)
                        .map_or(id, |item| item.id.clone());
                    self.delta(id, role, text);
                }
            }
            "assistant" | "user" => {
                let message = field(value, "message");
                let role = field(value, "type").as_str().unwrap_or("assistant");
                let id = field(message, "id")
                    .as_str()
                    .or_else(|| field(value, "uuid").as_str())
                    .map_or_else(|| format!("message-{}", self.revision), str::to_owned);
                let text = content_text(field(message, "content"));
                if role == "assistant"
                    && let Some(item) = self
                        .transcript
                        .last_mut()
                        .filter(|item| !item.complete && item.role == role)
                {
                    item.id.clone_from(&id);
                }
                self.message(id, role, text, true);
            }
            "result" => {
                self.usage = Some(
                    json!({"usage":field(value,"usage"),"cost_usd":field(value,"total_cost_usd")}),
                );
                self.status = if field(value, "is_error") == true {
                    self.error = string(field(value, "result"))
                        .or_else(|| {
                            (!field(value, "errors").is_null())
                                .then(|| field(value, "errors").to_string())
                        })
                        .or_else(|| Some("Provider reported an unsuccessful turn".to_owned()));
                    NativeSessionStatus::Error
                } else {
                    NativeSessionStatus::Idle
                };
            }
            _ => {}
        }
    }

    fn pi(&mut self, value: &Value) {
        match field(value, "type").as_str().unwrap_or_default() {
            "agent_start" | "turn_start" => self.status = NativeSessionStatus::Working,
            "agent_end" => self.status = NativeSessionStatus::Idle,
            "message_update" => {
                let event = field(value, "assistantMessageEvent");
                let role = if field(event, "type") == "thinking_delta" {
                    "thinking"
                } else {
                    "assistant"
                };
                if let Some(delta) = field(event, "delta").as_str() {
                    let id = self
                        .transcript
                        .last()
                        .filter(|item| !item.complete && item.role == role)
                        .map_or_else(|| format!("pi-{}", self.revision), |item| item.id.clone());
                    self.delta(id, role, delta);
                }
            }
            "message_end" => {
                let message = field(value, "message");
                let role = field(message, "role").as_str().unwrap_or("assistant");
                let id = self
                    .transcript
                    .last()
                    .filter(|item| !item.complete && item.role == role)
                    .map_or_else(|| format!("pi-{}", self.revision), |item| item.id.clone());
                self.message(id, role, content_text(field(message, "content")), true);
                if !field(message, "usage").is_null() {
                    self.usage = Some(field(message, "usage").clone());
                }
                if field(message, "stopReason") == "error" {
                    self.error = string(field(message, "errorMessage"));
                    self.status = NativeSessionStatus::Error;
                }
            }
            "extension_ui_request" => self.pi_information(value),
            "tool_execution_start" => {
                self.message(
                    field(value, "toolCallId")
                        .as_str()
                        .unwrap_or("tool")
                        .to_owned(),
                    "tool",
                    format!(
                        "{}\n{}",
                        field(value, "toolName").as_str().unwrap_or_default(),
                        field(value, "args")
                    ),
                    false,
                );
            }
            "tool_execution_end" => {
                self.message(
                    field(value, "toolCallId")
                        .as_str()
                        .unwrap_or("tool")
                        .to_owned(),
                    "tool",
                    content_text(field(field(value, "result"), "content")),
                    true,
                );
            }
            _ => {}
        }
    }
    fn pi_information(&mut self, value: &Value) {
        let (id, text) = match field(value, "method").as_str() {
            Some("notify") => (
                format!("notice:{}", field(value, "id")),
                field(value, "message")
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some("setStatus") => (
                format!("status:{}", field(value, "statusKey")),
                field(value, "statusText")
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some("setWidget") => (
                format!("widget:{}", field(value, "widgetKey")),
                field(value, "widgetLines")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Some("setTitle") => (
                "provider-title".to_owned(),
                field(value, "title")
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some("set_editor_text") => (
                "provider-editor".to_owned(),
                field(value, "text").as_str().unwrap_or_default().to_owned(),
            ),
            _ => return,
        };
        if text.is_empty() {
            self.transcript.retain(|item| item.id != id);
        } else {
            self.message(id, "notice", text, true);
        }
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

fn bounded_text(mut text: String) -> String {
    const LIMIT: usize = 16 * 1024;
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
