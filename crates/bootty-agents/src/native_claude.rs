//! Claude's owned NDJSON transport. Session intent and provider acknowledgement are separate facts.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::{
    NativeAgentRequest, NativeSessionSnapshot, NativeSessionStatus,
    native_protocol::{NativeToolStatus, NativeTurnOutcome, bounded_text, field, string},
};

struct Dispatch {
    id: String,
    acknowledgement: Acknowledgement,
    image_count: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Acknowledgement {
    Pending,
    Accepted,
}

/// One child, one exact provider session, and at most one submitted main-agent turn.
/// The host canonicalizes the captured directory before constructing this pure reducer.
pub struct ClaudeProtocol {
    session_id: String,
    cwd: PathBuf,
    dispatch: Option<Dispatch>,
    message_id: Option<String>,
    block_index: Option<u64>,
}

impl ClaudeProtocol {
    pub fn new(session_id: String, cwd: PathBuf) -> Result<Self, String> {
        if !valid_session_id(&session_id) || !cwd.is_absolute() {
            return Err("Claude requires an exact session UUID and absolute directory".to_owned());
        }
        Ok(Self {
            session_id,
            cwd,
            dispatch: None,
            message_id: None,
            block_index: None,
        })
    }

    /// Admission is provisional. Only the matching replayed user message accepts the turn.
    pub fn prompt(
        &mut self,
        snapshot: &mut NativeSessionSnapshot,
        id: &str,
        prompt: &crate::NativePrompt,
    ) -> Result<Value, String> {
        if self.dispatch.is_some() || snapshot.status != NativeSessionStatus::Idle {
            return Err(
                "Wait for the current Claude turn or interrupt it before prompting".to_owned(),
            );
        }
        if !valid_session_id(id) {
            return Err("Claude requires a user UUID and a prompt of 1–65536 bytes".to_owned());
        }
        snapshot.pending_user_item = Some(id.to_owned());
        snapshot.prompt_message(id, prompt)?;
        snapshot.completed_turn = false;
        snapshot.error = None;
        snapshot.status = NativeSessionStatus::Working;
        snapshot.changed();
        self.dispatch = Some(Dispatch {
            id: id.to_owned(),
            acknowledgement: Acknowledgement::Pending,
            image_count: prompt.image_count(),
        });
        Ok(json!({"type":"user","uuid":id,"session_id":self.session_id,
            "parent_tool_use_id":null,"message":{"role":"user","content":prompt.claude_content()?}}))
    }

    /// Returns an immediate control error for unsupported or unrelated provider requests.
    pub fn ingest(
        &mut self,
        snapshot: &mut NativeSessionSnapshot,
        value: &Value,
    ) -> Result<Option<Value>, String> {
        match field(value, "type").as_str() {
            Some("system") if field(value, "subtype") == "init" => {
                self.initialize(snapshot, value)?;
            }
            Some("system")
                if matches!(
                    field(value, "subtype").as_str(),
                    Some("task_started" | "task_progress" | "task_notification")
                ) && snapshot.session_id.as_deref() == Some(self.session_id.as_str())
                    && field(value, "session_id").as_str() == Some(self.session_id.as_str()) =>
            {
                snapshot.claude_subagents(value);
            }
            Some("control_cancel_request") => cancel_request(snapshot, value),
            Some("control_request") => return self.capture_request(snapshot, value),
            Some("user" | "stream_event" | "assistant" | "result")
                if self.accepts(snapshot, value) =>
            {
                if let Some(timestamp) = field(value, "timestamp")
                    .as_str()
                    .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
                {
                    let timestamp = Some(timestamp.timestamp_millis());
                    snapshot.with_message_times((timestamp, None), |snapshot| {
                        self.turn_event(snapshot, value);
                    });
                } else {
                    self.turn_event(snapshot, value);
                }
            }
            _ => return Ok(None),
        }
        snapshot.changed();
        Ok(None)
    }

    fn initialize(
        &self,
        snapshot: &mut NativeSessionSnapshot,
        value: &Value,
    ) -> Result<(), String> {
        if field(value, "session_id").as_str() != Some(self.session_id.as_str())
            || field(value, "cwd")
                .as_str()
                .is_none_or(|cwd| std::path::Path::new(cwd) != self.cwd)
        {
            return Err(
                "Claude reported a different session or directory; captured identity was preserved"
                    .to_owned(),
            );
        }
        snapshot.session_id = Some(self.session_id.clone());
        Ok(())
    }

    fn accepts(&self, snapshot: &NativeSessionSnapshot, value: &Value) -> bool {
        snapshot.session_id.as_deref() == Some(self.session_id.as_str())
            && field(value, "session_id").as_str() == Some(self.session_id.as_str())
            && field(value, "parent_tool_use_id").is_null()
            && self.dispatch.as_ref().is_some_and(|dispatch| {
                field(value, "user_message_uuid")
                    .as_str()
                    .is_none_or(|id| id == dispatch.id)
            })
    }

    pub fn allows_image_echo(&self, snapshot: &NativeSessionSnapshot, value: &Value) -> bool {
        self.accepts(snapshot, value)
            && field(value, "type") == "user"
            && field(value, "isReplay") == true
            && self.dispatch.as_ref().is_some_and(|dispatch| {
                dispatch.image_count > 0
                    && field(value, "uuid").as_str() == Some(dispatch.id.as_str())
                    && field(field(value, "message"), "content")
                        .as_array()
                        .is_some_and(|blocks| valid_claude_content(blocks, dispatch.image_count))
            })
    }

    pub fn allows_history_echo(&self, snapshot: &NativeSessionSnapshot, value: &Value) -> bool {
        self.accepts(snapshot, value)
            && field(value, "type") == "user"
            && field(value, "isReplay") == true
            && self
                .dispatch
                .as_ref()
                .is_some_and(|dispatch| field(value, "uuid").as_str() == Some(dispatch.id.as_str()))
    }

    fn turn_event(&mut self, snapshot: &mut NativeSessionSnapshot, value: &Value) {
        match field(value, "type").as_str() {
            Some("user") => self.user_message(snapshot, value),
            Some("stream_event") if self.accepted() => self.stream(snapshot, field(value, "event")),
            Some("assistant") if self.accepted() => self.assistant(snapshot, value),
            Some("result")
                if self.accepted()
                    && matches!(
                        field(value, "subtype").as_str(),
                        Some(
                            "success"
                                | "error_during_execution"
                                | "error_max_turns"
                                | "error_max_budget_usd"
                                | "error_max_structured_output_retries"
                        )
                    )
                    && field(value, "is_error").is_boolean() =>
            {
                self.finish(snapshot, value);
            }
            _ => {}
        }
    }

    fn accepted(&self) -> bool {
        self.dispatch
            .as_ref()
            .is_some_and(|dispatch| dispatch.acknowledgement == Acknowledgement::Accepted)
    }

    fn user_message(&mut self, snapshot: &mut NativeSessionSnapshot, value: &Value) {
        let Some(dispatch) = &mut self.dispatch else {
            return;
        };
        if field(value, "isReplay") == true
            && field(value, "uuid").as_str() == Some(dispatch.id.as_str())
            && field(field(value, "message"), "role") == "user"
            && (field(field(value, "message"), "content").is_string()
                || field(field(value, "message"), "content")
                    .as_array()
                    .is_some_and(|blocks| {
                        !blocks.is_empty() && valid_claude_content(blocks, dispatch.image_count)
                    }))
        {
            dispatch.acknowledgement = Acknowledgement::Accepted;
            snapshot.accept_first_turn(&dispatch.id);
            snapshot.turn_id = Some(dispatch.id.clone());
            snapshot.message(
                dispatch.id.clone(),
                "user",
                crate::native_protocol::content_text(field(field(value, "message"), "content")),
                true,
            );
            snapshot.pending_user_item = None;
        } else if self.accepted() {
            tool_results(snapshot, field(field(value, "message"), "content"));
        }
    }

    fn stream(&mut self, snapshot: &mut NativeSessionSnapshot, event: &Value) {
        match field(event, "type").as_str() {
            Some("message_start") => {
                self.message_id = string(field(field(event, "message"), "id"));
                self.block_index = None;
            }
            Some("content_block_start") => {
                self.block_index = field(event, "index").as_u64();
                if let Some(id) = self.block_id() {
                    content_block(snapshot, id, field(event, "content_block"), false);
                }
            }
            Some("content_block_delta") if field(event, "index").as_u64() == self.block_index => {
                let delta = field(event, "delta");
                if let Some(id) = self.block_id() {
                    match field(delta, "type").as_str() {
                        Some("text_delta") => {
                            if let Some(text) = field(delta, "text").as_str() {
                                snapshot.delta(id, "assistant", text);
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(text) = field(delta, "thinking").as_str() {
                                snapshot.delta(id, "reasoning", text);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("content_block_stop") if field(event, "index").as_u64() == self.block_index => {
                self.block_index = None;
            }
            _ => {}
        }
    }

    fn block_id(&self) -> Option<String> {
        self.message_id
            .as_ref()
            .filter(|id| valid_wire_id(id))
            .zip(self.block_index)
            .map(|(id, index)| format!("claude-{id}-{index}"))
    }

    fn assistant(&self, snapshot: &mut NativeSessionSnapshot, value: &Value) {
        let message = field(value, "message");
        let Some(content) = field(message, "content").as_array() else {
            return;
        };
        // Claude emits each completed content block separately, before its block-stop event.
        if content.len() == 1
            && field(message, "id").as_str() == self.message_id.as_deref()
            && let Some(id) = self.block_id()
            && let Some(block) = content.first()
        {
            content_block(snapshot, id, block, true);
        } else if let Some(id) = field(value, "uuid").as_str().filter(|id| valid_wire_id(id)) {
            for (index, block) in content.iter().enumerate() {
                content_block(snapshot, format!("claude-{id}-{index}"), block, true);
            }
        }
    }

    fn finish(&mut self, snapshot: &mut NativeSessionSnapshot, value: &Value) {
        let outcome = match field(value, "terminal_reason").as_str() {
            Some("aborted_streaming" | "aborted_tools") => NativeTurnOutcome::Interrupted,
            _ if field(value, "subtype") == "success" && field(value, "is_error") == false => {
                NativeTurnOutcome::Succeeded
            }
            _ => NativeTurnOutcome::Failed,
        };
        if let Some(dispatch) = self.dispatch.take() {
            snapshot.finish_first_turn(&dispatch.id, outcome);
        }
        snapshot.completed_turn = outcome == NativeTurnOutcome::Succeeded;
        snapshot.status = if outcome == NativeTurnOutcome::Failed {
            snapshot.error = Some("Claude reported a failed turn".to_owned());
            NativeSessionStatus::Error
        } else {
            NativeSessionStatus::Idle
        };
        snapshot.turn_id = None;
        snapshot.pending_user_item = None;
        snapshot.working_since = None;
        snapshot.requests.clear();
        self.message_id = None;
        self.block_index = None;
    }

    fn capture_request(
        &self,
        snapshot: &mut NativeSessionSnapshot,
        value: &Value,
    ) -> Result<Option<Value>, String> {
        let Some(id) = field(value, "request_id")
            .as_str()
            .filter(|id| valid_wire_id(id))
        else {
            return Err("Claude emitted an invalid control request identity".to_owned());
        };
        let request = field(value, "request");
        if !self.accepted()
            || snapshot.session_id.as_deref() != Some(self.session_id.as_str())
            || value
                .get("session_id")
                .is_some_and(|id| id.as_str() != Some(self.session_id.as_str()))
            || field(request, "subtype") != "can_use_tool"
            || field(request, "tool_name")
                .as_str()
                .is_none_or(|name| !valid_wire_id(name))
            || !field(request, "input").is_object()
            || field(request, "tool_use_id")
                .as_str()
                .is_none_or(|id| !valid_wire_id(id))
        {
            return Ok(Some(control_error(
                id,
                "Unsupported or unrelated native control request",
            )));
        }
        if snapshot.requests.len() >= 32 || request.to_string().len() > 16 * 1024 {
            return Err("Claude exceeded the bounded pending permission budget".to_owned());
        }
        if !snapshot
            .requests
            .iter()
            .any(|pending| pending.wire_id == id)
        {
            snapshot.requests.push(NativeAgentRequest {
                id: format!("claude-{id}"),
                method: "claude.permission".to_owned(),
                parameters: request.clone(),
                from_attached_tools: false,
                wire_id: id.into(),
            });
            snapshot.status = NativeSessionStatus::Waiting;
            snapshot.changed();
        }
        Ok(None)
    }
}

fn valid_claude_content(blocks: &[Value], expected_images: usize) -> bool {
    let images = blocks
        .iter()
        .filter(|block| field(block, "type") == "image")
        .count();
    images == expected_images
        && blocks.iter().all(|block| {
            field(block, "type") == "text" && field(block, "text").is_string()
                || field(block, "type") == "image"
                    && field(field(block, "source"), "type") == "base64"
                    && field(field(block, "source"), "media_type") == "image/png"
                    && field(field(block, "source"), "data").is_string()
        })
}

pub fn validate_configuration(config: &crate::NativeSessionConfig) -> Result<(), String> {
    if config.session_file.is_some()
        || config
            .session_id
            .as_ref()
            .is_some_and(|id| !valid_session_id(id))
        || config
            .fresh_session_id
            .as_ref()
            .is_some_and(|id| !valid_session_id(id))
        || config.session_id.is_none() && config.fresh_session_id.is_none()
        || config
            .session_id
            .as_ref()
            .zip(config.fresh_session_id.as_ref())
            .is_some_and(|(observed, intent)| observed != intent)
    {
        return Err("Claude requires its captured exact UUID intent or resume identity".to_owned());
    }
    let launch = crate::AgentLaunch {
        program: config.program.clone(),
        cwd: None,
        arguments: config.arguments.clone(),
        ephemeral: false,
        account_directory: None,
    };
    let retained = launch.retained(crate::AgentKind::Claude);
    if retained.arguments != config.arguments || retained.ephemeral {
        return Err("Native Claude requires reusable options; transport and session identity are host-owned".to_owned());
    }
    Ok(())
}

pub fn command(id: &str, method: &str, params: &Value) -> Result<Value, String> {
    if !valid_wire_id(id) {
        return Err("Invalid Claude control identity".to_owned());
    }
    let fields = params
        .as_object()
        .ok_or("Invalid Claude control parameters")?;
    let request = match method {
        "initialize" | "interrupt" | "get_settings" if fields.is_empty() => {
            json!({"subtype":method})
        }
        "set_model" if fields.len() == 1 => {
            let model = field(params, "model")
                .as_str()
                .filter(|model| {
                    !model.is_empty() && model.len() <= 8192 && !model.chars().any(char::is_control)
                })
                .ok_or("Invalid Claude model selector")?;
            json!({"subtype":method,"model":model})
        }
        "apply_flag_settings" if fields.len() == 1 => {
            let settings = field(params, "settings")
                .as_object()
                .filter(|settings| settings.len() == 1)
                .ok_or("Invalid Claude model settings")?;
            let effort = settings
                .get("effortLevel")
                .ok_or("Missing Claude effort setting")?;
            if !effort.is_null()
                && !matches!(effort.as_str(), Some("low" | "medium" | "high" | "xhigh"))
            {
                return Err("Claude cannot change to that effort in a running session".to_owned());
            }
            json!({"subtype":method,"settings":settings})
        }
        _ => return Err("Unsupported owned Claude control command".to_owned()),
    };
    Ok(json!({"type":"control_request","request_id":id,"request":request}))
}

pub fn reply(value: &Value) -> Result<Value, String> {
    if field(value, "type") != "control_response" {
        return Err("Claude returned an invalid control response".to_owned());
    }
    let response = field(value, "response");
    match field(response, "subtype").as_str() {
        Some("success") => Ok(response
            .get("response")
            .cloned()
            .unwrap_or_else(|| json!({}))),
        Some("error") => Err(bounded_text(
            field(response, "error")
                .as_str()
                .unwrap_or("Claude rejected the control command")
                .to_owned(),
        )),
        _ => Err("Claude returned no control outcome".to_owned()),
    }
}

pub fn launch_arguments(session_id: &str, resume: bool) -> Result<Vec<String>, String> {
    if !valid_session_id(session_id) {
        return Err("Claude requires an exact session UUID".to_owned());
    }
    Ok([
        "--print",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
        if resume { "--resume" } else { "--session-id" },
        session_id,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect())
}

pub fn permission_response(
    request: &NativeAgentRequest,
    response: &Value,
) -> Result<Value, String> {
    if field(&request.parameters, "tool_name") == "AskUserQuestion"
        && response.get("answers").is_some()
    {
        return question_response(request, response);
    }
    let decision = response
        .as_object()
        .filter(|fields| fields.len() == 1)
        .and_then(|fields| fields.get("decision"))
        .and_then(Value::as_str)
        .filter(|decision| matches!(*decision, "accept" | "decline"))
        .ok_or("Claude permission response must contain only an accept or decline decision")?;
    let id = request
        .wire_id
        .as_str()
        .ok_or("Missing Claude permission identity")?;
    let tool = field(&request.parameters, "tool_use_id")
        .as_str()
        .ok_or("Missing Claude tool-use identity")?;
    if decision == "accept" && field(&request.parameters, "tool_name") == "AskUserQuestion" {
        return Err("Claude questions require explicit answers".to_owned());
    }
    let permission = if decision == "accept" {
        json!({"behavior":"allow","updatedInput":field(&request.parameters,"input"),"toolUseID":tool})
    } else {
        json!({"behavior":"deny","message":"Denied by the user","toolUseID":tool})
    };
    Ok(
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":permission}}),
    )
}

fn question_response(request: &NativeAgentRequest, response: &Value) -> Result<Value, String> {
    let answers = response
        .as_object()
        .filter(|fields| fields.len() == 1)
        .and_then(|fields| fields.get("answers"))
        .and_then(Value::as_object)
        .ok_or("Claude questions require only an answers object")?;
    let input = field(&request.parameters, "input");
    let questions = field(input, "questions")
        .as_array()
        .filter(|questions| !questions.is_empty() && questions.len() <= 16)
        .ok_or("Claude emitted no supported questions")?;
    if answers.len() != questions.len() {
        return Err("Answer every captured Claude question exactly once".to_owned());
    }
    let mut keys = std::collections::BTreeSet::new();
    for question in questions {
        let key = field(question, "question")
            .as_str()
            .filter(|key| valid_wire_id(key))
            .ok_or("Claude emitted an invalid question identity")?;
        if !keys.insert(key) {
            return Err("Claude question identities must be unique".to_owned());
        }
        let _answer = answers
            .get(key)
            .and_then(Value::as_str)
            .filter(|answer| !answer.is_empty() && answer.len() <= 8192 && !answer.contains('\0'))
            .ok_or("Claude question answers must be bounded nonempty strings")?;
        // AskUserQuestion accepts custom text as well as comma-separated selected labels.
        // Validate the captured option schema; never reinterpret an explicit user's answer.
        if let Some(options) = field(question, "options").as_array()
            && (options.len() > 64
                || options.iter().any(|option| {
                    field(option, "label")
                        .as_str()
                        .is_none_or(|label| !valid_wire_id(label))
                }))
        {
            return Err("Claude emitted invalid question options".to_owned());
        }
    }
    if response.to_string().len() > 16 * 1024 {
        return Err("Claude answers exceed 16 KiB".to_owned());
    }
    let mut updated = input
        .as_object()
        .cloned()
        .ok_or("Missing Claude question input")?;
    updated.insert("answers".to_owned(), Value::Object(answers.clone()));
    let id = request
        .wire_id
        .as_str()
        .ok_or("Missing Claude permission identity")?;
    let tool = field(&request.parameters, "tool_use_id")
        .as_str()
        .ok_or("Missing Claude tool-use identity")?;
    Ok(
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{
            "behavior":"allow","updatedInput":updated,"toolUseID":tool
        }}}),
    )
}

fn control_error(id: &str, message: &str) -> Value {
    json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":message}})
}

fn cancel_request(snapshot: &mut NativeSessionSnapshot, value: &Value) {
    let before = snapshot.requests.len();
    snapshot
        .requests
        .retain(|request| field(value, "request_id") != &request.wire_id);
    if snapshot.requests.len() != before && snapshot.requests.is_empty() {
        snapshot.status = NativeSessionStatus::Working;
    }
}

fn content_block(snapshot: &mut NativeSessionSnapshot, id: String, block: &Value, complete: bool) {
    match field(block, "type").as_str() {
        Some("text") => snapshot.message(
            id,
            "assistant",
            field(block, "text").as_str().unwrap_or_default().to_owned(),
            complete,
        ),
        Some("thinking") => snapshot.message(
            id,
            "reasoning",
            field(block, "thinking")
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            complete,
        ),
        Some("tool_use") => {
            if let Some(tool) = field(block, "id").as_str().filter(|id| valid_wire_id(id)) {
                snapshot.tool_message(
                    format!("claude-tool-{tool}"),
                    field(block, "name").as_str(),
                    block.get("input").map(Value::to_string),
                    String::new(),
                    NativeToolStatus::Running,
                );
            }
        }
        _ => {}
    }
}

fn tool_results(snapshot: &mut NativeSessionSnapshot, content: &Value) {
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            if field(block, "type") == "tool_result"
                && let Some(id) = field(block, "tool_use_id")
                    .as_str()
                    .filter(|id| valid_wire_id(id))
            {
                snapshot.tool_message(
                    format!("claude-tool-{id}"),
                    None,
                    None,
                    crate::native_protocol::content_text(field(block, "content")),
                    if field(block, "is_error") == true {
                        NativeToolStatus::Failed
                    } else {
                        NativeToolStatus::Completed
                    },
                );
            }
        }
    }
}

pub fn valid_session_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn valid_wire_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 8192 && !id.chars().any(char::is_control)
}
