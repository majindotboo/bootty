//! Claude's owned NDJSON transport. Session intent and provider acknowledgement are separate facts.

use std::{collections::VecDeque, path::PathBuf};

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

/// One child and one exact provider session, including admitted active follow-ups.
/// The host canonicalizes the captured directory before constructing this pure reducer.
pub struct ClaudeProtocol {
    session_id: String,
    cwd: PathBuf,
    dispatch: Option<Dispatch>,
    steering: VecDeque<Dispatch>,
    message_id: Option<String>,
    block_index: Option<u64>,
    /// The tool row whose arguments are streaming, and the JSON received so far.
    tool_input: Option<(String, String)>,
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
            steering: VecDeque::new(),
            message_id: None,
            block_index: None,
            tool_input: None,
        })
    }

    /// Claude takes a `next` message at the turn's next tool boundary without stopping the
    /// running tool, and the turn ends with one result. A turn that ends first gets it as the
    /// following turn instead.
    pub fn steer(
        &mut self,
        snapshot: &mut NativeSessionSnapshot,
        id: &str,
        prompt: &crate::NativePrompt,
    ) -> Result<Value, String> {
        if let Some(refusal) = snapshot.steer_refusal() {
            return Err(refusal.into());
        }
        if !self.accepted() {
            return Err("Wait for the previous message to reach the active turn".into());
        }
        // Bound admitted input until Claude reports its results; increase only
        // when the transport offers an explicit backlog acknowledgement.
        if self.steering.len() >= 16 || !valid_session_id(id) {
            return Err("Claude has too many pending follow-ups".into());
        }
        let value = json!({"type":"user","uuid":id,"session_id":self.session_id,
            "parent_tool_use_id":null,"priority":"next",
            "message":{"role":"user","content":prompt.claude_content()?}});
        snapshot.prompt_message(id, prompt)?;
        snapshot.pending_user_item = Some(id.to_owned());
        self.steering.push_back(Dispatch {
            id: id.to_owned(),
            acknowledgement: Acknowledgement::Pending,
            image_count: prompt.image_count(),
        });
        snapshot.changed();
        Ok(value)
    }

    pub fn retract_steer(&mut self, snapshot: &mut NativeSessionSnapshot, id: &str) {
        self.steering.retain(|dispatch| dispatch.id != id);
        snapshot.transcript.retain(|item| item.id != id);
        if snapshot.pending_user_item.as_deref() == Some(id) {
            snapshot.pending_user_item = None;
        }
        snapshot.changed();
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
        self.adopt_self_started_turn(snapshot, value);
        match field(value, "type").as_str() {
            Some("system") if field(value, "subtype") == "init" => {
                self.initialize(snapshot, value)?;
            }
            Some("system")
                if matches!(
                    field(value, "subtype").as_str(),
                    Some("task_started" | "task_progress" | "task_updated" | "task_notification")
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

    /// Claude starts a turn on its own, for example to report a finished background task. While
    /// idle, adopt that turn so its messages and tools appear instead of being dropped.
    fn adopt_self_started_turn(&mut self, snapshot: &mut NativeSessionSnapshot, value: &Value) {
        if self.dispatch.is_some()
            || snapshot.status != NativeSessionStatus::Idle
            || !matches!(
                field(value, "type").as_str(),
                Some("stream_event" | "assistant")
            )
            || snapshot.session_id.as_deref() != Some(self.session_id.as_str())
            || field(value, "session_id").as_str() != Some(self.session_id.as_str())
            || !field(value, "parent_tool_use_id").is_null()
        {
            return;
        }
        // Later events of this turn name the message that started it; keep that identity.
        let id = field(value, "user_message_uuid")
            .as_str()
            .or_else(|| field(value, "uuid").as_str())
            .filter(|id| valid_wire_id(id))
            .map_or_else(|| "claude-self-started-turn".to_owned(), str::to_owned);
        self.dispatch = Some(Dispatch {
            id,
            acknowledgement: Acknowledgement::Accepted,
            image_count: 0,
        });
        snapshot.status = NativeSessionStatus::Working;
        snapshot.completed_turn = false;
        snapshot.error = None;
    }

    fn accepts(&self, snapshot: &NativeSessionSnapshot, value: &Value) -> bool {
        snapshot.session_id.as_deref() == Some(self.session_id.as_str())
            && field(value, "session_id").as_str() == Some(self.session_id.as_str())
            && field(value, "parent_tool_use_id").is_null()
            && self.dispatch.is_some()
            && field(value, "user_message_uuid").as_str().is_none_or(|id| {
                self.dispatch
                    .iter()
                    .chain(&self.steering)
                    .any(|dispatch| id == dispatch.id)
            })
    }

    pub fn allows_image_echo(&self, snapshot: &NativeSessionSnapshot, value: &Value) -> bool {
        self.accepts(snapshot, value)
            && field(value, "type") == "user"
            && field(value, "isReplay") == true
            && self.dispatch.iter().chain(&self.steering).any(|dispatch| {
                dispatch.image_count > 0
                    && field(value, "uuid").as_str() == Some(dispatch.id.as_str())
                    && field(field(value, "message"), "content")
                        .as_array()
                        .is_some_and(|blocks| valid_claude_content(blocks, dispatch.image_count))
            })
    }

    /// Image bytes in this turn's tool results, which Claude repeats in `tool_use_result`. They
    /// stay outside the control budget; the transcript keeps only a result's text.
    pub fn tool_result_image_bytes(
        &self,
        snapshot: &NativeSessionSnapshot,
        value: &Value,
    ) -> usize {
        if !self.accepts(snapshot, value) || field(value, "type") != "user" {
            return 0;
        }
        let blocks = field(field(value, "message"), "content").as_array();
        let mut bytes = blocks
            .into_iter()
            .flatten()
            .filter(|block| field(block, "type") == "tool_result")
            .flat_map(|block| field(block, "content").as_array().into_iter().flatten())
            .filter(|item| field(item, "type") == "image")
            .filter_map(|item| field(field(item, "source"), "data").as_str())
            .map(str::len)
            .fold(0_usize, usize::saturating_add);
        let repeated = field(value, "tool_use_result");
        if field(repeated, "type") == "image"
            && let Some(data) = field(field(repeated, "file"), "base64").as_str()
        {
            bytes = bytes.saturating_add(data.len());
        }
        bytes
    }

    pub fn allows_history_echo(&self, snapshot: &NativeSessionSnapshot, value: &Value) -> bool {
        self.accepts(snapshot, value)
            && field(value, "type") == "user"
            && field(value, "isReplay") == true
            && self
                .dispatch
                .iter()
                .chain(&self.steering)
                .any(|dispatch| field(value, "uuid").as_str() == Some(dispatch.id.as_str()))
    }

    fn turn_event(&mut self, snapshot: &mut NativeSessionSnapshot, value: &Value) {
        match field(value, "type").as_str() {
            Some("user") => self.user_message(snapshot, value),
            Some("stream_event") if self.accepted() => self.stream(snapshot, field(value, "event")),
            Some("assistant") if self.accepted() => self.assistant(snapshot, value),
            // After an acknowledged interrupt, the next result ends the stopped work even if
            // Claude never confirmed the turn's message.
            Some("result")
                if (self.accepted() || snapshot.stopping())
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
        let Some(dispatch) = self
            .dispatch
            .iter_mut()
            .chain(&mut self.steering)
            .find(|dispatch| field(value, "uuid").as_str() == Some(dispatch.id.as_str()))
        else {
            if self.accepted() {
                tool_results(snapshot, value);
            }
            return;
        };
        // The uuid names exactly this message. Claude may rewrite its content, for example by
        // downscaling an image, and rejecting the echo would hide the whole turn.
        if field(value, "isReplay") == true
            && field(value, "uuid").as_str() == Some(dispatch.id.as_str())
            && field(field(value, "message"), "role") == "user"
        {
            dispatch.acknowledgement = Acknowledgement::Accepted;
            snapshot.accept_first_turn(&dispatch.id);
            if snapshot.turn_id.is_none() {
                snapshot.turn_id = Some(dispatch.id.clone());
            }
            snapshot.message(
                dispatch.id.clone(),
                "user",
                crate::native_protocol::content_text(field(field(value, "message"), "content")),
                true,
            );
            if snapshot.pending_user_item.as_deref() == Some(dispatch.id.as_str()) {
                snapshot.pending_user_item = None;
            }
            // A steer Claude takes in mid-turn belongs to the running turn, which ends with one
            // result, so it no longer waits for a turn of its own.
            let id = dispatch.id.clone();
            if self.accepted()
                && self
                    .dispatch
                    .as_ref()
                    .is_some_and(|current| current.id != id)
            {
                self.steering.retain(|steer| steer.id != id);
            }
        } else if self.accepted() {
            tool_results(snapshot, value);
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
                let block = field(event, "content_block");
                self.tool_input = (field(block, "type") == "tool_use")
                    .then(|| field(block, "id").as_str())
                    .flatten()
                    .filter(|id| valid_wire_id(id))
                    .map(|id| (format!("claude-tool-{id}"), String::new()));
                if let Some(id) = self.block_id() {
                    content_block(snapshot, id, block, false);
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
                                snapshot.delta(id, "thinking", text);
                            }
                        }
                        Some("input_json_delta") => self.stream_tool_input(snapshot, delta),
                        _ => {}
                    }
                }
            }
            Some("content_block_stop") if field(event, "index").as_u64() == self.block_index => {
                self.block_index = None;
                self.tool_input = None;
            }
            _ => {}
        }
    }

    /// Shows a running tool's arguments as they arrive. A prefix that closes into JSON replaces
    /// the row's input; the completed block sets the whole input later.
    fn stream_tool_input(&mut self, snapshot: &mut NativeSessionSnapshot, delta: &Value) {
        /// Deliberate limit: a preview needs only the arguments' start, and each update copies
        /// the prefix. Raise it if summaries need later fields.
        const PREVIEW_BYTES: usize = 8 * 1024;
        let (Some((row, input)), Some(partial)) =
            (&mut self.tool_input, field(delta, "partial_json").as_str())
        else {
            return;
        };
        if input.len() > PREVIEW_BYTES {
            return;
        }
        input.push_str(partial);
        if let Some(preview) = ["", "\"}", "}", "\"]}", "]}"]
            .into_iter()
            .map(|suffix| format!("{input}{suffix}"))
            .find(|candidate| serde_json::from_str::<Value>(candidate).is_ok())
        {
            snapshot.tool_message(
                row.clone(),
                None,
                Some(preview),
                String::new(),
                NativeToolStatus::Running,
            );
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
        record_usage(snapshot, field(message, "usage"));
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
        record_context_window(snapshot, field(value, "modelUsage"));
        let outcome = match field(value, "terminal_reason").as_str() {
            Some("aborted_streaming" | "aborted_tools") => NativeTurnOutcome::Interrupted,
            _ if field(value, "subtype") == "success" && field(value, "is_error") == false => {
                NativeTurnOutcome::Succeeded
            }
            _ => NativeTurnOutcome::Failed,
        };
        if let Some(dispatch) = self.dispatch.take() {
            snapshot.finish_first_turn(&dispatch.id, outcome);
            // A steer can also abort tools; only a requested stop marks the turn.
            if outcome == NativeTurnOutcome::Interrupted && snapshot.stopping() {
                snapshot.interrupted_turn(&dispatch.id);
            }
        }
        snapshot.settle_running_tools();
        self.dispatch = self.steering.pop_front();
        self.message_id = None;
        self.block_index = None;
        snapshot.requests.clear();
        if let Some(dispatch) = &self.dispatch {
            snapshot.turn_id = Some(dispatch.id.clone());
            snapshot.status = NativeSessionStatus::Working;
            return;
        }
        snapshot.finish_turn(outcome == NativeTurnOutcome::Succeeded);
        snapshot.status = if outcome == NativeTurnOutcome::Failed {
            snapshot.error = Some(failure_reason(value));
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
        if let Some(reason) = snapshot.request_refusal(request) {
            snapshot.declined_request(id, reason.to_owned());
            return Ok(Some(control_error(id, reason)));
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
                subagent: None,
                wire_id: id.into(),
            });
            snapshot.status = NativeSessionStatus::Waiting;
            snapshot.changed();
        }
        Ok(None)
    }
}

/// Bootty sends PNGs, but Claude downscales large images and echoes them re-encoded.
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
                    && matches!(
                        field(field(block, "source"), "media_type").as_str(),
                        Some("image/png" | "image/jpeg" | "image/gif" | "image/webp")
                    )
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
        "set_permission_mode" if fields.len() == 1 => {
            let mode = field(params, "mode")
                .as_str()
                .filter(|mode| {
                    matches!(
                        *mode,
                        "default" | "acceptEdits" | "auto" | "bypassPermissions" | "plan"
                    )
                })
                .ok_or("Invalid Claude permission mode")?;
            json!({"subtype":method,"mode":mode})
        }
        "apply_flag_settings" if fields.len() == 1 => {
            let settings = field(params, "settings")
                .as_object()
                .filter(|settings| settings.len() == 1)
                .ok_or("Invalid Claude model settings")?;
            if let Some(fast) = settings.get("fastMode") {
                if !fast.is_boolean() {
                    return Err("Invalid Claude fast mode setting".to_owned());
                }
            } else {
                let effort = settings
                    .get("effortLevel")
                    .ok_or("Missing Claude effort setting")?;
                if !effort.is_null()
                    && !matches!(effort.as_str(), Some("low" | "medium" | "high" | "xhigh"))
                {
                    return Err(
                        "Claude cannot change to that effort in a running session".to_owned()
                    );
                }
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
    let fields = response
        .as_object()
        .filter(|fields| {
            fields
                .keys()
                .all(|key| matches!(key.as_str(), "decision" | "scope"))
        })
        .ok_or("Claude permission response must contain only a decision and its scope")?;
    let decision = fields
        .get("decision")
        .and_then(Value::as_str)
        .filter(|decision| matches!(*decision, "accept" | "decline"))
        .ok_or("Claude permission response must accept or decline")?;
    // A reusable scope applies only the rules Claude itself suggested for this request.
    let permissions = fields
        .get("scope")
        .map(|scope| {
            scope
                .as_str()
                .filter(|_| decision == "accept")
                .and_then(|scope| updated_permissions(&request.parameters, scope))
                .ok_or("Claude didn't offer that approval scope")
        })
        .transpose()?;
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
        let mut allow = json!({"behavior":"allow","updatedInput":field(&request.parameters,"input"),"toolUseID":tool});
        if let (Some(permissions), Some(allow)) = (permissions, allow.as_object_mut()) {
            allow.insert("updatedPermissions".to_owned(), permissions.into());
        }
        allow
    } else {
        json!({"behavior":"deny","message":"Denied by the user","toolUseID":tool})
    };
    Ok(
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":permission}}),
    )
}

/// A Claude approval's answer for `decision`, when Claude offered that choice. Allow for this
/// session applies Claude's suggestions for the session; Always allow keeps them where Claude
/// proposed.
#[must_use]
pub fn approval_response(
    parameters: &Value,
    decision: crate::NativeApprovalDecision,
) -> Option<Value> {
    use crate::NativeApprovalDecision as D;
    let scope = match decision {
        D::Deny => return Some(json!({"decision":"decline"})),
        D::AllowOnce => return Some(json!({"decision":"accept"})),
        D::AllowSession => "session",
        D::AlwaysAllow => "always",
    };
    updated_permissions(parameters, scope).map(|_| json!({"decision":"accept","scope":scope}))
}

/// Claude's own suggestions for a scope: every suggestion moved to the session, or the
/// suggestions as proposed when one outlives the session.
fn updated_permissions(parameters: &Value, scope: &str) -> Option<Vec<Value>> {
    let suggestions = field(parameters, "permission_suggestions")
        .as_array()
        .filter(|suggestions| !suggestions.is_empty())?;
    match scope {
        "session" => Some(
            suggestions
                .iter()
                .cloned()
                .map(|mut suggestion| {
                    if let Some(fields) = suggestion.as_object_mut() {
                        fields.insert("destination".to_owned(), "session".into());
                    }
                    suggestion
                })
                .collect(),
        ),
        "always" => suggestions
            .iter()
            .any(|suggestion| field(suggestion, "destination") != "session")
            .then(|| suggestions.clone()),
        _ => None,
    }
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

pub fn content_block(
    snapshot: &mut NativeSessionSnapshot,
    id: String,
    block: &Value,
    complete: bool,
) {
    match field(block, "type").as_str() {
        Some("text") => snapshot.message(
            id,
            "assistant",
            field(block, "text").as_str().unwrap_or_default().to_owned(),
            complete,
        ),
        Some("thinking") => snapshot.message(
            id,
            "thinking",
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

/// The context the latest model call filled, in the counters the context meter reads.
fn record_usage(snapshot: &mut NativeSessionSnapshot, usage: &Value) {
    let Some(input) = field(usage, "input_tokens").as_u64() else {
        return;
    };
    let count = |key| field(usage, key).as_u64().unwrap_or(0);
    let mut next = json!({
        "input": input,
        "output": count("output_tokens"),
        "cacheRead": count("cache_read_input_tokens"),
        "cacheWrite": count("cache_creation_input_tokens"),
    });
    if let Some(window) = snapshot
        .usage
        .as_ref()
        .and_then(|usage| usage.get("modelContextWindow"))
        && let Some(next) = next.as_object_mut()
    {
        next.insert("modelContextWindow".to_owned(), window.clone());
    }
    snapshot.usage = Some(next);
}

/// A turn's result reports each model's context window; the meter measures against the largest.
fn record_context_window(snapshot: &mut NativeSessionSnapshot, models: &Value) {
    let Some(window) = models.as_object().and_then(|models| {
        models
            .values()
            .filter_map(|model| field(model, "contextWindow").as_u64())
            .max()
    }) else {
        return;
    };
    if let Some(usage) = snapshot
        .usage
        .get_or_insert_with(|| json!({}))
        .as_object_mut()
    {
        usage.insert("modelContextWindow".to_owned(), window.into());
    }
}

pub fn tool_results(snapshot: &mut NativeSessionSnapshot, value: &Value) {
    // Claude rejects the tool it was running when a stop arrives; that tool was interrupted.
    let rejected = |id: &str| {
        field(value, "tool_result_meta")
            .as_array()
            .is_some_and(|meta| {
                meta.iter().any(|entry| {
                    field(entry, "id").as_str() == Some(id)
                        && field(entry, "non_execution_kind") == "user-rejected"
                })
            })
    };
    if let Some(blocks) = field(field(value, "message"), "content").as_array() {
        // A message's result describes its only tool: live output calls it `tool_use_result`,
        // the session file `toolUseResult`.
        let results = blocks
            .iter()
            .filter(|block| field(block, "type") == "tool_result")
            .count();
        let change = (results == 1)
            .then(|| {
                value
                    .get("tool_use_result")
                    .or_else(|| value.get("toolUseResult"))
                    .and_then(file_change)
            })
            .flatten();
        for block in blocks {
            if field(block, "type") == "tool_result"
                && let Some(id) = field(block, "tool_use_id")
                    .as_str()
                    .filter(|id| valid_wire_id(id))
            {
                let change = change.clone().filter(|_| field(block, "is_error") != true);
                snapshot.tool_message(
                    format!("claude-tool-{id}"),
                    change.as_ref().map(|_| "fileChange"),
                    change,
                    crate::native_protocol::content_text(field(block, "content")),
                    if snapshot.stopping() && rejected(id) {
                        NativeToolStatus::Interrupted
                    } else if field(block, "is_error") == true {
                        NativeToolStatus::Failed
                    } else {
                        NativeToolStatus::Completed
                    },
                );
            }
        }
    }
}

/// Claude's own patch for a file it edited or wrote. A new file has no patch; its content is all
/// added.
fn file_change(result: &Value) -> Option<String> {
    use std::fmt::Write as _;
    let path = field(result, "filePath").as_str()?;
    let hunks = field(result, "structuredPatch").as_array()?;
    let diff = if hunks.is_empty() {
        if field(result, "type") != "create" {
            return None;
        }
        let content = field(result, "content").as_str()?;
        let count = content.lines().count();
        if count == 0 {
            String::new()
        } else {
            std::iter::once(format!("@@ -0,0 +1,{count} @@\n"))
                .chain(content.lines().map(|line| format!("+{line}\n")))
                .collect()
        }
    } else {
        let mut diff = String::new();
        for hunk in hunks {
            let number = |key: &str| field(hunk, key).as_u64();
            _ = writeln!(
                diff,
                "@@ -{},{} +{},{} @@",
                number("oldStart")?,
                number("oldLines")?,
                number("newStart")?,
                number("newLines")?,
            );
            for line in field(hunk, "lines").as_array()? {
                diff.push_str(line.as_str()?);
                diff.push('\n');
            }
        }
        diff
    };
    Some(crate::native_protocol::file_change_input(path, &diff))
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

/// Claude's result names why a turn failed; keep that reason rather than a fixed sentence.
fn failure_reason(value: &Value) -> String {
    match field(value, "subtype").as_str() {
        Some("error_max_turns") => "Claude stopped at its turn limit".to_owned(),
        Some("error_max_budget_usd") => "Claude stopped at its budget limit".to_owned(),
        Some("error_max_structured_output_retries") => {
            "Claude could not produce valid structured output".to_owned()
        }
        _ => field(value, "errors")
            .as_array()
            .and_then(|errors| errors.iter().find_map(Value::as_str))
            .filter(|reason| !reason.trim().is_empty())
            .map_or_else(
                || "Claude reported a failed turn".to_owned(),
                |reason| crate::native_protocol::bounded_text(reason.to_owned()),
            ),
    }
}
