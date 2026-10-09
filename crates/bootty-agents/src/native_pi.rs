//! Pi's owned JSONL RPC process: no terminal emulation or global extension installation.

use std::path::Path;

use serde_json::{Value, json};

use crate::{
    NativeAgentRequest, NativeAgentSession, NativeSessionSnapshot, NativeSessionStatus,
    native_protocol::{
        NativeToolStatus, NativeTurnOutcome, bounded_text, content_text, field, string,
    },
    native_session::{lock, publish_change},
};

pub fn command(id: String, method: &str, params: &Value) -> Result<Value, String> {
    if !matches!(
        method,
        "get_state"
            | "get_messages"
            | "get_entries"
            | "prompt"
            | "steer"
            | "abort"
            | "get_available_models"
            | "get_commands"
            | "compact"
            | "set_model"
            | "set_thinking_level"
            | "switch_session"
    ) {
        return Err("Unsupported owned Pi RPC command".to_owned());
    }
    let mut fields = params
        .as_object()
        .cloned()
        .ok_or("Pi command parameters must be an object")?;
    if fields.contains_key("id") || fields.contains_key("type") {
        return Err("Pi command parameters cannot replace protocol identity".to_owned());
    }
    fields.insert("id".to_owned(), id.into());
    fields.insert("type".to_owned(), method.into());
    Ok(Value::Object(fields))
}

pub fn reply(value: &Value, method: &str) -> Result<Value, String> {
    if field(value, "type") != "response" || field(value, "command") != method {
        return Err("Pi response does not match the submitted command".to_owned());
    }
    match field(value, "success").as_bool() {
        Some(true) => Ok(value.get("data").cloned().unwrap_or_else(|| json!({}))),
        Some(false) => Err(bounded_text(
            field(value, "error")
                .as_str()
                .unwrap_or("Pi rejected the command")
                .to_owned(),
        )),
        None => Err("Pi response has no success outcome".to_owned()),
    }
}

impl NativeAgentSession {
    pub(crate) fn initialize_pi(&self) -> Result<(), String> {
        let mut state = self.rpc("get_state", json!({}))?;
        let id =
            valid_identity(field(&state, "sessionId")).ok_or("Pi returned no session identity")?;
        let file = valid_identity(field(&state, "sessionFile"))
            .filter(|file| Path::new(file).is_absolute())
            .ok_or("Pi returned no absolute persistent session file")?;
        if self
            .config
            .session_id
            .as_ref()
            .is_some_and(|expected| expected != &id)
            || self
                .config
                .session_file
                .as_ref()
                .is_some_and(|expected| expected != &file)
        {
            return Err("Pi resumed a different file; captured identity was preserved".to_owned());
        }
        if self.config.session_id.is_none() && lock(&self.snapshot).tool_server.is_some() {
            let commands = self.rpc("get_commands", json!({}))?;
            if !field(&commands, "commands")
                .as_array()
                .is_some_and(|commands| {
                    commands.iter().any(|command| {
                        field(command, "name") == crate::tool_bridge::PI_CHECKPOINT_COMMAND
                    })
                })
            {
                return Err("Pi did not register its session checkpoint".into());
            }
            let checkpoint = self.rpc(
                "prompt",
                json!({"message":format!("/{}", crate::tool_bridge::PI_CHECKPOINT_COMMAND)}),
            )?;
            // Older Pi RPC acknowledges preflight without a disposition. The subsequent
            // switch and identity check still require the exact checkpointed file.
            if field(&checkpoint, "disposition") != "handled"
                && !checkpoint
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty)
            {
                return Err("Pi did not handle its session checkpoint".into());
            }
            let switched = self.rpc("switch_session", json!({"sessionPath":file}))?;
            if field(&switched, "cancelled").as_bool() != Some(false) {
                return Err("Pi cancelled saving its session".into());
            }
            state = self.rpc("get_state", json!({}))?;
            if field(&state, "sessionId").as_str() != Some(&id)
                || field(&state, "sessionFile").as_str() != Some(&file)
            {
                return Err("Pi changed identity while saving its session".into());
            }
        }
        {
            let mut snapshot = lock(&self.snapshot);
            snapshot.session_id = Some(id);
            snapshot.session_file = Some(file);
            snapshot.status =
                if field(&state, "isStreaming") == true || field(&state, "isCompacting") == true {
                    NativeSessionStatus::Working
                } else {
                    NativeSessionStatus::Idle
                };
            snapshot.changed();
        }
        self.refresh_history_pi()?;
        Ok(())
    }

    pub(crate) fn send_prompt_pi(&self, prompt: &crate::NativePrompt) -> Result<(), String> {
        self.verify_pi_identity()?;
        if let Some(instructions) = prompt.compact_instructions()? {
            return self.compact_pi(&instructions);
        }
        let dispatch = {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status != NativeSessionStatus::Idle {
                return Err(
                    "Wait for the current Pi run or interrupt it before prompting".to_owned(),
                );
            }
            let dispatch = format!("pi-prompt-{}", snapshot.revision);
            snapshot.pi_dispatch_id = Some(dispatch.clone());
            let id = format!("local-user-{}", snapshot.revision);
            snapshot.pending_user_item = Some(id.clone());
            let now = Some(chrono::Utc::now().timestamp_millis());
            snapshot
                .with_message_times((now, now), |snapshot| snapshot.prompt_message(&id, prompt))?;
            snapshot.status = NativeSessionStatus::Working;
            snapshot.working_since = Some((self.clock)());
            snapshot.completed_turn = false;
            snapshot.error = None;
            snapshot.changed();
            dispatch
        };
        publish_change(&self.change_handler);
        let result = match self.rpc_prompt_pi(
            prompt.pi_parameters()?,
            prompt.requires_large_envelope(),
            &dispatch,
        ) {
            Ok(None) => Ok(()), // An observed question is delivered, but owns no turn receipt yet.
            Ok(Some(reply)) => accept_prompt_reply(&mut lock(&self.snapshot), &dispatch, Ok(reply)),
            Err(error) => accept_prompt_reply(&mut lock(&self.snapshot), &dispatch, Err(error)),
        };
        publish_change(&self.change_handler);
        result
    }

    pub(crate) fn interrupt_pi(&self) -> Result<(), String> {
        let (dispatch, active, dismissed) = {
            let mut snapshot = lock(&self.snapshot);
            let dispatch = snapshot.pi_dispatch_id.clone();
            let active = matches!(
                snapshot.status,
                NativeSessionStatus::Working | NativeSessionStatus::Waiting
            );
            let dismissed = !snapshot.requests.is_empty();
            // Pi waits for UI promises before acknowledging abort; cancel them without approving tools.
            while let Some(request) = snapshot.requests.first() {
                self.write(&ui_response(request, &json!({"cancelled":true}))?)?;
                snapshot.requests.remove(0);
            }
            if dismissed {
                snapshot.status = NativeSessionStatus::Working;
                snapshot.changed();
            }
            drop(snapshot);
            (dispatch, active, dismissed)
        };
        if dismissed {
            self.publish_input_wait();
            publish_change(&self.change_handler);
        }
        self.rpc("abort", json!({}))?;
        let mut snapshot = lock(&self.snapshot);
        if snapshot.pi_dispatch_id != dispatch {
            // Another admitted prompt owns the process now; this acknowledgement is for its predecessor.
            return Ok(());
        }
        if active
            && let Some(dispatch) = dispatch
            && let Some(receipt) = &mut snapshot.first_turn
            && receipt.id == dispatch
        {
            receipt.outcome = NativeTurnOutcome::Interrupted;
        }
        snapshot.status = NativeSessionStatus::Idle;
        snapshot.completed_turn = false;
        snapshot.error = None;
        snapshot.working_since = None;
        snapshot.turn_id = None;
        snapshot.requests.clear();
        snapshot.changed();
        drop(snapshot);
        publish_change(&self.change_handler);
        Ok(())
    }

    fn compact_pi(&self, instructions: &str) -> Result<(), String> {
        {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status != NativeSessionStatus::Idle {
                return Err("Wait for the current Pi run before compacting".into());
            }
            snapshot.status = NativeSessionStatus::Working;
            snapshot.working_since = Some((self.clock)());
            snapshot.changed();
        }
        publish_change(&self.change_handler);
        let result = self.rpc(
            "compact",
            if instructions.is_empty() {
                json!({})
            } else {
                json!({"customInstructions":instructions})
            },
        );
        {
            let mut snapshot = lock(&self.snapshot);
            if snapshot.status == NativeSessionStatus::Working {
                snapshot.status = if result.is_ok() {
                    NativeSessionStatus::Idle
                } else {
                    NativeSessionStatus::Error
                };
                snapshot.working_since = None;
                snapshot.error = result.as_ref().err().cloned();
                snapshot.changed();
            }
        }
        publish_change(&self.change_handler);
        result?;
        self.refresh_history_pi()?;
        Ok(())
    }

    pub(crate) fn verify_pi_identity(&self) -> Result<(), String> {
        let state = self.rpc("get_state", json!({}))?;
        let mut snapshot = lock(&self.snapshot);
        let matches = field(&state, "sessionId").as_str() == snapshot.session_id.as_deref()
            && field(&state, "sessionFile").as_str() == snapshot.session_file.as_deref();
        if matches && let Some(limit) = field(field(&state, "model"), "contextWindow").as_u64() {
            let usage = snapshot.usage.get_or_insert_with(|| json!({}));
            if usage.get("modelContextWindow").and_then(Value::as_u64) != Some(limit) {
                if let Some(usage) = usage.as_object_mut() {
                    usage.insert("modelContextWindow".to_owned(), limit.into());
                }
                snapshot.changed();
            }
        }
        drop(snapshot);
        if !matches {
            return Err(
                "Pi returned a different session; the saved session was preserved".to_owned(),
            );
        }
        Ok(())
    }

    pub(crate) fn refresh_history_pi(&self) -> Result<NativeSessionSnapshot, String> {
        self.verify_pi_identity()?;
        let history = self.rpc("get_messages", json!({}))?;
        self.verify_pi_identity()?;
        let messages = field(&history, "messages")
            .as_array()
            .ok_or("Pi history has no message array")?;
        let mut snapshot = lock(&self.snapshot);
        if snapshot.status != NativeSessionStatus::Idle {
            return Err("Wait for the current Pi run before refreshing history".to_owned());
        }
        let previous = std::mem::take(&mut snapshot.transcript);
        for message in messages {
            message_item(&mut snapshot, message, true);
        }
        snapshot.restore_image_references(&previous);
        snapshot.changed();
        let result = snapshot.clone();
        drop(snapshot);
        publish_change(&self.change_handler);
        Ok(result)
    }
}

fn valid_identity(value: &Value) -> Option<String> {
    string(value)
        .filter(|id| !id.is_empty() && id.len() <= 8192 && !id.chars().any(char::is_control))
}

pub fn ingest(snapshot: &mut NativeSessionSnapshot, value: &Value) -> Result<(), String> {
    if snapshot.session_id.is_none() {
        return Ok(());
    }
    snapshot.observe_computer_tool(value);
    match field(value, "type").as_str().unwrap_or_default() {
        "agent_start" => {
            snapshot.status = NativeSessionStatus::Working;
            snapshot.completed_turn = false;
            snapshot.error = None;
            let id = snapshot
                .pi_dispatch_id
                .clone()
                .unwrap_or_else(|| format!("pi-run-{}", snapshot.revision));
            if snapshot.pi_dispatch_id.is_some() {
                snapshot.accept_first_turn(&id);
            }
            snapshot.turn_id = Some(id);
        }
        "agent_settled" => {
            if let Some(id) = snapshot.turn_id.clone() {
                snapshot.finish_first_turn(
                    &id,
                    if snapshot.error.is_some() {
                        NativeTurnOutcome::Failed
                    } else {
                        NativeTurnOutcome::Succeeded
                    },
                );
            }
            snapshot.status = if snapshot.error.is_some() {
                NativeSessionStatus::Error
            } else {
                NativeSessionStatus::Idle
            };
            snapshot.completed_turn = snapshot.error.is_none() && snapshot.turn_id.is_some();
            snapshot.turn_id = None;
            snapshot.working_since = None;
            snapshot.pending_user_item = None;
            snapshot.requests.clear();
        }
        "message_start" | "message_end" => ingest_message(snapshot, value),
        "message_update" => {
            // Pi publishes the full partial message; an empty start is not a chat row.
            message_item(snapshot, field(value, "message"), false);
            let usage = field(value, "usage");
            if usage.is_object() && usage.to_string().len() <= 16 * 1024 {
                capture_usage(snapshot, usage);
            }
        }
        "tool_execution_start" | "tool_execution_update" | "tool_execution_end" => {
            if let Some(id) = valid_identity(field(value, "toolCallId")) {
                let complete = field(value, "type") == "tool_execution_end";
                let text = if field(value, "type") == "tool_execution_start" {
                    String::new()
                } else {
                    content_text(field(
                        field(value, if complete { "result" } else { "partialResult" }),
                        "content",
                    ))
                };
                let name = field(value, "toolName").as_str().unwrap_or_default();
                snapshot.pi_subagents(
                    &id,
                    name,
                    field(value, if complete { "result" } else { "partialResult" }),
                    complete,
                );
                snapshot.tool_message(
                    format!("tool-{id}"),
                    field(value, "toolName").as_str(),
                    value.get("args").map(Value::to_string),
                    text,
                    if complete {
                        if field(value, "isError") == true {
                            NativeToolStatus::Failed
                        } else {
                            NativeToolStatus::Completed
                        }
                    } else {
                        NativeToolStatus::Running
                    },
                );
            }
        }
        "extension_ui_request" => ui_request(snapshot, value)?,
        // agent_end may precede retries/compaction: only agent_settled ends the owned run.
        _ => return Ok(()),
    }
    snapshot.changed();
    Ok(())
}

fn ingest_message(snapshot: &mut NativeSessionSnapshot, value: &Value) {
    let complete = field(value, "type") == "message_end";
    let message = field(value, "message");
    message_item(snapshot, message, complete);
    if matches!(
        field(message, "stopReason").as_str(),
        Some("error" | "aborted")
    ) {
        if field(message, "stopReason") == "aborted"
            && let Some(id) = snapshot.turn_id.clone()
        {
            snapshot.finish_first_turn(&id, NativeTurnOutcome::Interrupted);
        }
        snapshot.error = Some(bounded_text(
            field(message, "errorMessage")
                .as_str()
                .unwrap_or("Pi run failed or was aborted")
                .to_owned(),
        ));
    }
}

pub fn message_item(snapshot: &mut NativeSessionSnapshot, message: &Value, complete: bool) {
    let Some(timestamp) = field(message, "timestamp").as_i64() else {
        return;
    };
    snapshot.with_message_times((Some(timestamp), None), |snapshot| {
        message_content(snapshot, message, complete);
    });
}

fn message_content(snapshot: &mut NativeSessionSnapshot, message: &Value, complete: bool) {
    let role = field(message, "role").as_str().unwrap_or_default();
    if !matches!(role, "user" | "assistant" | "toolResult" | "custom")
        || (role == "custom" && field(message, "display") != true)
    {
        return;
    }
    let Some(timestamp) = field(message, "timestamp").as_u64() else {
        return;
    };
    let id = if role == "toolResult" {
        valid_identity(field(message, "toolCallId")).map(|id| format!("tool-{id}"))
    } else {
        Some(format!("pi-{role}-{timestamp}"))
    };
    if let Some(id) = id {
        let content = field(message, "content");
        if role == "assistant" {
            let usage = field(message, "usage");
            if usage.is_object() && usage.to_string().len() <= 16 * 1024 {
                capture_usage(snapshot, usage);
            }
            let blocks = content.as_array().into_iter().flatten();
            let text = blocks
                .clone()
                .filter(|block| field(block, "type") == "text")
                .filter_map(|block| field(block, "text").as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let thinking = blocks
                .filter(|block| field(block, "type") == "thinking")
                .filter_map(|block| field(block, "thinking").as_str())
                .collect::<Vec<_>>()
                .join("\n");
            snapshot.message(format!("thinking-{id}"), "thinking", thinking, complete);
            snapshot.message(id, role, text, complete);
            for block in content.as_array().into_iter().flatten() {
                if field(block, "type") == "toolCall"
                    && let Some(id) = valid_identity(field(block, "id"))
                {
                    let id = format!("tool-{id}");
                    // Full assistant messages can be replayed after a result; retain its outcome.
                    if !snapshot
                        .transcript
                        .iter()
                        .any(|item| item.id == id && item.complete)
                    {
                        snapshot.tool_message(
                            id,
                            field(block, "name").as_str(),
                            block.get("arguments").map(Value::to_string),
                            String::new(),
                            NativeToolStatus::Running,
                        );
                    }
                }
            }
            return;
        }
        if role == "toolResult" {
            snapshot.pi_subagents(
                &id,
                field(message, "toolName").as_str().unwrap_or_default(),
                message,
                complete,
            );
            snapshot.tool_message(
                id.clone(),
                field(message, "toolName")
                    .as_str()
                    .map(|name| result_tool_name(name, message)),
                None,
                content_text(content),
                if !complete {
                    NativeToolStatus::Running
                } else if field(message, "isError") == true {
                    NativeToolStatus::Failed
                } else {
                    NativeToolStatus::Completed
                },
            );
            nested_tool_results(snapshot, &id, field(message, "details"));
            return;
        }
        snapshot.message(id, role, content_text(content), complete);
    }
}

fn nested_tool_results(snapshot: &mut NativeSessionSnapshot, parent: &str, details: &Value) {
    // Pi persists nested execution presentation in the parent tool result.
    let nested = field(details, "libtuiNestedCalls");
    if field(nested, "version") != 1 {
        return;
    }
    let prefix = format!("{parent}/");
    for call in field(nested, "calls")
        .as_array()
        .into_iter()
        .flatten()
        .take(256)
    {
        let Some(id) = valid_identity(field(call, "id")) else {
            continue;
        };
        let id = format!("tool-{id}");
        if !id.starts_with(&prefix) {
            continue;
        }
        let Some(name) = valid_identity(field(call, "name")) else {
            continue;
        };
        let Some(args) = call.get("args").filter(|args| args.is_object()) else {
            continue;
        };
        let status = match field(call, "status").as_str() {
            Some("running") => NativeToolStatus::Running,
            Some("succeeded") => NativeToolStatus::Completed,
            Some("failed") => NativeToolStatus::Failed,
            _ => continue,
        };
        snapshot.tool_message(
            id,
            Some(result_tool_name(&name, field(call, "result"))),
            Some(args.to_string()),
            content_text(field(field(call, "result"), "content")),
            status,
        );
    }
}

pub fn result_tool_name<'a>(name: &'a str, result: &Value) -> &'a str {
    // Saved Pi results retain the original server, which differs after a reconnect.
    // This is presentation only; image acceptance uses the live attachment and call ID.
    field(field(result, "details"), "server")
        .as_str()
        .and_then(|server| crate::tool_bridge::logical_pi_tool_name(server, name))
        .unwrap_or(name)
}

fn capture_usage(snapshot: &mut NativeSessionSnapshot, usage: &Value) {
    // Pi initializes partial and interrupted messages with unreported, zeroed counters.
    if !["input", "output", "cacheRead", "cacheWrite", "totalTokens"]
        .into_iter()
        .any(|field| {
            usage
                .get(field)
                .and_then(Value::as_u64)
                .is_some_and(|count| count > 0)
        })
    {
        return;
    }
    let limit = snapshot
        .usage
        .as_ref()
        .and_then(|usage| usage.get("modelContextWindow"))
        .cloned();
    let mut usage = usage.clone();
    if let Some(limit) = limit
        && let Some(usage) = usage.as_object_mut()
    {
        usage.insert("modelContextWindow".to_owned(), limit);
    }
    snapshot.usage = Some(usage);
}

pub fn accept_prompt_reply(
    snapshot: &mut NativeSessionSnapshot,
    dispatch: &str,
    reply: Result<Value, String>,
) -> Result<(), String> {
    let result = reply.and_then(|reply| match field(&reply, "disposition").as_str() {
        Some("started" | "queued") => Ok(false),
        Some("handled") => Ok(true),
        None if reply.as_object().is_some_and(serde_json::Map::is_empty) => Ok(false),
        _ => Err("Pi returned no recognized prompt disposition".to_owned()),
    });
    if snapshot.pi_dispatch_id.as_deref() != Some(dispatch)
        || snapshot.status == NativeSessionStatus::Stopped
    {
        return result.map(|_| ());
    }
    match &result {
        Err(error) => {
            snapshot.status = NativeSessionStatus::Error;
            snapshot.error = Some(bounded_text(error.clone()));
            snapshot.working_since = None;
        }
        Ok(false) => snapshot.accept_first_turn(dispatch),
        Ok(true)
            if snapshot.turn_id.is_none() && snapshot.status == NativeSessionStatus::Working =>
        {
            snapshot.status = NativeSessionStatus::Idle;
            snapshot.working_since = None;
            snapshot.pi_dispatch_id = None;
        }
        _ => {}
    }
    snapshot.changed();
    result.map(|_| ())
}

fn ui_request(snapshot: &mut NativeSessionSnapshot, value: &Value) -> Result<(), String> {
    let Some(id) = valid_identity(field(value, "id")) else {
        return Err("Pi UI request has no valid identity".to_owned());
    };
    let method = field(value, "method").as_str().unwrap_or_default();
    if matches!(method, "select" | "confirm" | "input" | "editor") {
        if value.to_string().len() > 16 * 1024 || snapshot.requests.len() >= 32 {
            return Err("Pi pending UI requests exceed the bounded view".to_owned());
        }
        if snapshot
            .requests
            .iter()
            .any(|request| request.wire_id == id)
        {
            return Err("Pi reused a pending UI request identity".to_owned());
        }
        snapshot.requests.push(NativeAgentRequest {
            id: format!("pi-request-{id}"),
            method: format!("pi.{method}"),
            parameters: value.clone(),
            from_attached_tools: false,
            wire_id: Value::String(id),
        });
        snapshot.status = NativeSessionStatus::Waiting;
    } else if method == "notify" {
        let text = field(value, "message")
            .as_str()
            .unwrap_or_default()
            .to_owned();
        snapshot.message(format!("pi-notice-{id}"), "notice", text, true);
    }
    Ok(())
}

pub fn ui_response(request: &NativeAgentRequest, response: &Value) -> Result<Value, String> {
    let fields = response
        .as_object()
        .filter(|fields| fields.len() == 1)
        .ok_or("Pi UI response requires exactly one answer")?;
    let valid = if fields.get("cancelled").is_some_and(|value| value == true) {
        true
    } else if request.method == "pi.confirm" {
        fields.get("confirmed").is_some_and(Value::is_boolean)
    } else {
        fields
            .get("value")
            .and_then(Value::as_str)
            .is_some_and(|value| {
                request.method != "pi.select"
                    || field(&request.parameters, "options")
                        .as_array()
                        .is_some_and(|options| options.iter().any(|option| option == value))
            })
    };
    if !valid {
        return Err("Pi response does not match the pending dialog".to_owned());
    }
    let mut fields = fields.clone();
    fields.insert("type".to_owned(), "extension_ui_response".into());
    fields.insert("id".to_owned(), request.wire_id.clone());
    Ok(Value::Object(fields))
}
