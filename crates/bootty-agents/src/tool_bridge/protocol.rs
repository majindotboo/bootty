//! Bounded MCP messages. Tool inputs cannot choose a command, caller, path or target.

use std::time::{Duration, Instant};

use bootty_computer::{ComputerAction, ComputerResultSnapshot};
use bootty_control::CommandOutcome;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    AgentCommandExecutor,
    tool_policy::{NativeToolRead, ToolCapture, ToolLease, WorkspaceToolRead},
    tool_spawn::ToolSpawnRequest,
};

pub use bootty_host::private_stdio::{MAX_TOOL_IMAGE_RESPONSE_BYTES, MAX_TOOL_MESSAGE_BYTES};
const MAX_LINES: u32 = 4096;

/// One authority-bound protocol endpoint; both private transport and tests use this owner.
pub struct ToolProtocol {
    lease: ToolLease,
}

impl ToolProtocol {
    #[must_use]
    pub const fn new(lease: ToolLease) -> Self {
        Self { lease }
    }

    /// Notifications never execute tools. The caller injects the deadline clock at the transport.
    #[must_use]
    pub fn handle(
        &self,
        bytes: &[u8],
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> Option<Value> {
        if bytes.len() > MAX_TOOL_MESSAGE_BYTES {
            return Some(error(&Value::Null, -32600, "MCP request exceeds 1 MiB"));
        }
        let request: Request = match serde_json::from_slice(bytes) {
            Ok(request) => request,
            Err(_) => return Some(error(&Value::Null, -32600, "Invalid MCP request")),
        };
        if request.jsonrpc != "2.0" {
            return Some(error(
                &request.id.unwrap_or(Value::Null),
                -32600,
                "Expected JSON-RPC 2.0",
            ));
        }
        let id = request.id?;
        if !valid_id(&id) {
            return Some(error(&Value::Null, -32600, "Invalid request ID"));
        }
        let (result, image) = match request.method.as_str() {
            "initialize" => (
                json!({
                    "protocolVersion":"2025-06-18", "capabilities":{"tools":{}},
                    "serverInfo":{"name":"Bootty", "version":env!("CARGO_PKG_VERSION")},
                    "instructions":"Read your exact launched terminal. Capture and typed creation tools require explicit host permission. Arbitrary commands, paths, accounts and targets are unavailable.",
                }),
                false,
            ),
            "ping" => (json!({}), false),
            "tools/list" => (self.catalog(), false),
            "tools/call" => self.call(request.params, now, commands),
            _ => return Some(error(&id, -32601, "Unsupported MCP method")),
        };
        let response = json!({"jsonrpc":"2.0", "id":id, "result":result});
        let limit = if image {
            MAX_TOOL_IMAGE_RESPONSE_BYTES
        } else {
            MAX_TOOL_MESSAGE_BYTES
        };
        // Include the exact ID envelope and transport newline, including JSON escaping.
        if serde_json::to_vec(&response).map_or(true, |bytes| bytes.len() >= limit) {
            return Some(error(
                &id,
                -32603,
                if image {
                    "MCP image response exceeds 12 MiB"
                } else {
                    "MCP response exceeds 1 MiB"
                },
            ));
        }
        Some(response)
    }

    pub(super) fn catalog(&self) -> Value {
        let mut tools = Vec::new();
        if self.lease.workspace_read_enabled() {
            tools.push(json!({"name":"get_workspace_info","description":"Read the name, mux backend and host label of your exact captured Bootty Space. Other Spaces, paths and account configuration are unavailable.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"outputSchema":native_output_schema("get_workspace_info"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
            tools.push(json!({"name":"list_terminals","description":"List at most 128 observed terminal panes in your captured Space, excluding native agent carriers. Includes names and opaque targets, without process or directory fields or other Spaces. Detached tasks are not started.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"outputSchema":native_output_schema("list_terminals"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
        }
        if self.lease.native_agents_enabled() {
            tools.push(json!({"name":"list_agents","description":"List compact agent lifecycle metadata in your captured Space. Account paths and other Spaces are unavailable.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"outputSchema":native_output_schema("list_agents"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
            tools.push(json!({"name":"list_profiles","description":"List at most 16 configured profile IDs and display names for your captured provider, together with your retained profile ID. This does not grant access to other accounts or change your profile. Account paths, arguments and executables are private.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"outputSchema":native_output_schema("list_profiles"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
        }
        if self.lease.native_reads_enabled() {
            tools.push(json!({"name":"get_agent_activity","description":"Read accepted recent messages and tool lifecycle summaries in your exact captured conversation, most recent first. Text is bounded to 8 KiB per entry; older provider pages are not fetched.","inputSchema":{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":crate::MAX_NATIVE_ACTIVITY_ITEMS,"default":16}},"additionalProperties":false},"outputSchema":native_output_schema("get_agent_activity"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
            tools.push(json!({"name":"get_agent_status","description":"Read your exact Bootty conversation's accepted lifecycle, working state and pending approval/input flags.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"outputSchema":native_output_schema("get_agent_status"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
            tools.push(json!({"name":"list_models","description":"List models and reasoning choices advertised by your exact provider account. Omit provider or use your current provider; other accounts are unavailable.","inputSchema":{"type":"object","properties":{"provider":{"type":"string","enum":[self.lease.scope().provider]}},"additionalProperties":false},"outputSchema":native_output_schema("list_models"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
            tools.push(json!({"name":"list_providers","description":"List the provider captured by your conversation's host grant and its permission choices. Other providers and accounts are outside this grant.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"outputSchema":native_output_schema("list_providers"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
            tools.push(json!({"name":"inspect_provider","description":"Read your captured provider's model, effort, fast mode and configured permissions. Account and executable paths are private. This does not change permissions or test authentication.","inputSchema":{"type":"object","properties":{"provider":{"type":"string","enum":[self.lease.scope().provider]}},"additionalProperties":false},"outputSchema":native_output_schema("inspect_provider"),"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
        }
        if self.lease.enabled(None) {
            tools.push(json!({"name":"terminal_read", "description":"Read retained rendered state of your exact launched terminal, including history if requested.",
                "inputSchema":{"type":"object", "properties":{
                    "scope":{"type":"string", "enum":["screen","history"], "default":"screen"},
                    "max_lines":{"type":"integer", "minimum":1, "maximum":MAX_LINES, "default":MAX_LINES}}, "additionalProperties":false},
                "annotations":{"readOnlyHint":true, "destructiveHint":false, "openWorldHint":false}}));
        }
        if self.lease.browser_attachments_supported()
            || self.lease.enabled(Some(ToolCapture::Browser))
        {
            tools.push(json!({"name":"browser_snapshot","description":"Read the exact browser page attached to this conversation.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
        }
        if !self.lease.revoked() && self.lease.application_mentions_supported() {
            // Advertise before the first prompt so MCP clients can cache the catalog. Authority
            // arrives only through a user mention; absent or expired references are rejected.
            tools.push(json!({"name":"computer_snapshot","description":"Capture a PNG of an application window explicitly mentioned by the user. application is its opaque prompt reference; omitting it captures only the host-granted Bootty window.","inputSchema":{"type":"object","properties":{"application":{"type":"string","minLength":1,"maxLength":256}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}));
            tools.push(json!({"name":"computer_input","description":"Focus, click, type, press a key, move or scroll inside an exact application window explicitly mentioned by the user. OS input permission and focused-window checks apply.","inputSchema":{"type":"object","properties":{"application":{"type":"string","minLength":1,"maxLength":256},"action":application_action_schema()},"required":["application","action"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}));
        }
        if self.lease.spawn_enabled() {
            let labels = json!({
                "name":{"type":"string", "minLength":1,"maxLength":256},
                "title":{"type":"string", "minLength":1,"maxLength":256}
            });
            tools.push(json!({"name":"spawn_shell", "description":"Create a detached default shell task in your exact captured Space and project. Preserve parent focus; arbitrary commands and paths are unavailable.",
                "inputSchema":{"type":"object","properties":labels,"required":["name"],"additionalProperties":false},
                "annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}));
            tools.extend(spawned_terminal_tools());
            if self.lease.native_reads_enabled() {
                for (name, description, destructive) in [
                    (
                        "interrupt_spawned_agent",
                        "Interrupt the current turn of a native child created under your exact live parent grant. Other agents and children from older grants are unavailable.",
                        false,
                    ),
                    (
                        "stop_spawned_agent",
                        "Stop a native child created under your exact live parent grant, retaining its conversation and backend pane. Other agents and children from older grants are unavailable.",
                        true,
                    ),
                ] {
                    tools.push(json!({"name":name,"description":description,
                        "inputSchema":{"type":"object","properties":{
                            "id":{"type":"string","minLength":1,"maxLength":256},
                            "generation":{"type":"integer","minimum":1}},
                            "required":["id","generation"],"additionalProperties":false},
                        "annotations":{"readOnlyHint":false,"destructiveHint":destructive,"openWorldHint":false}}));
                }
            }
            tools.push(json!({"name":"spawn_agent", "description":"Create a detached agent task with a typed prompt, using your captured provider, profile and project. Its tools are read only and revoked with your parent grant.",
                "inputSchema":{"type":"object","properties":{
                    "name":{"type":"string","minLength":1,"maxLength":256},
                    "title":{"type":"string","minLength":1,"maxLength":256},
                    "provider":{"type":"string","enum":[self.lease.scope().provider]},
                    "profile":{"type":"string","minLength":1,"maxLength":256},
                    "prompt":{"type":"string","minLength":1,"maxLength":65536}},
                    "required":["name","provider","prompt"],"additionalProperties":false},
                "annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}));
        }
        json!({"tools":tools})
    }

    fn call(
        &self,
        params: Value,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> (Value, bool) {
        let call: ToolCall = match serde_json::from_value(params) {
            Ok(call) => call,
            Err(_) => {
                return (
                    tool_error(
                        "Invalid tool call; command, caller, targets and paths are host-owned",
                    ),
                    false,
                );
            }
        };
        if matches!(
            call.name.as_str(),
            "get_agent_status"
                | "get_agent_activity"
                | "list_models"
                | "list_agents"
                | "list_providers"
                | "list_profiles"
                | "inspect_provider"
        ) {
            return (
                self.native_read(&call.name, call.arguments, now, commands),
                false,
            );
        }
        let workspace_read = match call.name.as_str() {
            "get_workspace_info" => Some(WorkspaceToolRead::Info),
            "list_terminals" => Some(WorkspaceToolRead::Terminals),
            _ => None,
        };
        if let Some(read) = workspace_read {
            let result = self.workspace_read(read, &call.arguments, now, commands);
            return (result, false);
        }
        if matches!(
            call.name.as_str(),
            "interrupt_spawned_agent" | "stop_spawned_agent"
        ) {
            return (
                self.control_child(&call.name, call.arguments, now, commands),
                false,
            );
        }
        if matches!(call.name.as_str(), "spawn_shell" | "spawn_agent") {
            return (
                self.spawn(call.name.as_str(), call.arguments, now, commands),
                false,
            );
        }
        if matches!(
            call.name.as_str(),
            "read_spawned_terminal"
                | "paste_spawned_terminal"
                | "submit_spawned_terminal"
                | "interrupt_spawned_terminal"
                | "close_spawned_terminal"
        ) {
            return (
                self.spawned_terminal(&call.name, call.arguments, now, commands),
                false,
            );
        }
        if call.name == "computer_input"
            || call.name == "computer_snapshot" && call.arguments.get("application").is_some()
        {
            return self.application_call(&call.name, call.arguments, now, commands);
        }
        self.capture_read(call, now, commands)
    }

    fn capture_read(
        &self,
        call: ToolCall,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> (Value, bool) {
        let (arguments, capture) = match call.name.as_str() {
            "terminal_read" => {
                let read: TerminalRead = match serde_json::from_value(call.arguments) {
                    Ok(read) => read,
                    Err(_) => {
                        return (
                            tool_error("Expected screen/history and at most 4096 lines"),
                            false,
                        );
                    }
                };
                if !(1..=MAX_LINES).contains(&read.max_lines) {
                    return (tool_error("Expected 1–4096 lines"), false);
                }
                (
                    vec![
                        "plain".to_owned(),
                        match read.scope {
                            ReadScope::Screen => "screen",
                            ReadScope::History => "history",
                        }
                        .to_owned(),
                        read.max_lines.to_string(),
                    ],
                    None,
                )
            }
            "browser_snapshot" | "computer_snapshot"
                if call
                    .arguments
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty) =>
            {
                (
                    Vec::new(),
                    Some(if call.name == "browser_snapshot" {
                        ToolCapture::Browser
                    } else {
                        ToolCapture::Computer
                    }),
                )
            }
            _ => {
                return (
                    tool_error("Tool or input is not available for this launch"),
                    false,
                );
            }
        };
        let Some(deadline) = now.checked_add(Duration::from_secs(5)) else {
            return (tool_error("Tool deadline is unavailable"), false);
        };
        let outcome = self.lease.invoke(arguments, capture, commands, deadline);
        outcome_result(&outcome, capture == Some(ToolCapture::Computer))
    }
    fn application_call(
        &self,
        name: &str,
        arguments: Value,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> (Value, bool) {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Snapshot {
            application: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            application: String,
            action: ComputerAction,
        }
        let parsed = if name == "computer_snapshot" {
            serde_json::from_value::<Snapshot>(arguments)
                .map(|input| (input.application, ComputerAction::Snapshot))
        } else {
            serde_json::from_value::<Input>(arguments)
                .map(|input| (input.application, input.action))
        };
        let Ok((reference, action)) = parsed else {
            return (
                tool_error("Expected an application reference and a typed action"),
                false,
            );
        };
        if name == "computer_input" && action.is_capture() {
            return (
                tool_error("Use computer_snapshot to capture the mentioned window"),
                false,
            );
        }
        if reference.is_empty() || reference.len() > 256 {
            return (tool_error("Invalid application reference"), false);
        }
        let Some(deadline) = now.checked_add(Duration::from_secs(5)) else {
            return (tool_error("Tool deadline is unavailable"), false);
        };
        let image = action.is_capture();
        outcome_result(
            &self
                .lease
                .invoke_application(&reference, &action, commands, deadline),
            image,
        )
    }
    fn spawn(
        &self,
        name: &str,
        arguments: Value,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> Value {
        let Value::Object(mut arguments) = arguments else {
            return tool_error("Spawn inputs must be a typed object");
        };
        if arguments.contains_key("kind") {
            return tool_error("Spawn kind is selected by the tool catalog");
        }
        arguments.insert(
            "kind".to_owned(),
            Value::String(
                if name == "spawn_shell" {
                    "shell"
                } else {
                    "agent"
                }
                .to_owned(),
            ),
        );
        let request = match serde_json::to_vec(&arguments)
            .map_err(|error| error.to_string())
            .and_then(|bytes| ToolSpawnRequest::parse(&bytes))
        {
            Ok(request) => request,
            Err(message) => return tool_error(&message),
        };
        let Some(deadline) = now.checked_add(Duration::from_secs(5)) else {
            return tool_error("Tool deadline is unavailable");
        };
        let outcome = match self.lease.invoke_spawn(&request, commands, deadline) {
            CommandOutcome::Success { value, warnings } => CommandOutcome::Success {
                value: spawn_receipt(value),
                warnings,
            },
            outcome => outcome,
        };
        let failed = !matches!(outcome, CommandOutcome::Success { .. });
        let text = serde_json::to_string(&outcome)
            .unwrap_or_else(|_| "Tool response could not be serialized".to_owned());
        json!({"content":[{"type":"text", "text":text}], "isError":failed})
    }

    fn spawned_terminal(
        &self,
        name: &str,
        arguments: Value,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> Value {
        let Value::Object(mut arguments) = arguments else {
            return tool_error("An exact created Terminal is required");
        };
        if arguments.contains_key("operation") {
            return tool_error("Terminal operation is selected by the tool catalog");
        }
        if name != "paste_spawned_terminal" && arguments.contains_key("text") {
            return tool_error("Only terminal paste accepts text");
        }
        let operation = match name {
            "read_spawned_terminal" => "read",
            "paste_spawned_terminal" => "paste",
            "submit_spawned_terminal" => "submit",
            "interrupt_spawned_terminal" => "interrupt",
            _ => "close",
        };
        arguments.insert("operation".into(), json!(operation));
        let request = match serde_json::to_vec(&arguments)
            .map_err(|error| error.to_string())
            .and_then(|bytes| crate::ToolTerminalRequest::parse(&bytes))
        {
            Ok(request) => request,
            Err(message) => return tool_error(&message),
        };
        let Some(deadline) = now.checked_add(Duration::from_secs(5)) else {
            return tool_error("Tool deadline is unavailable");
        };
        let outcome = self
            .lease
            .invoke_spawned_terminal(&request, commands, deadline);
        outcome_result(&outcome, false).0
    }

    fn control_child(
        &self,
        name: &str,
        arguments: Value,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> Value {
        let Value::Object(mut arguments) = arguments else {
            return tool_error("Child identity and generation are required");
        };
        if arguments.contains_key("operation") {
            return tool_error("Child operation is selected by the tool catalog");
        }
        arguments.insert(
            "operation".into(),
            json!(if name == "interrupt_spawned_agent" {
                "interrupt"
            } else {
                "stop"
            }),
        );
        let request = match serde_json::to_vec(&arguments)
            .map_err(|error| error.to_string())
            .and_then(|bytes| crate::ToolChildControlRequest::parse(&bytes))
        {
            Ok(request) => request,
            Err(message) => return tool_error(&message),
        };
        let Some(deadline) = now.checked_add(Duration::from_secs(5)) else {
            return tool_error("Tool deadline is unavailable");
        };
        let outcome = self
            .lease
            .invoke_child_control(&request, commands, deadline);
        outcome_result(&outcome, false).0
    }

    fn workspace_read(
        &self,
        read: WorkspaceToolRead,
        arguments: &Value,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> Value {
        if !arguments.as_object().is_some_and(serde_json::Map::is_empty) {
            return tool_error("Workspace identity and targets are captured by the host");
        }
        let Some(deadline) = now.checked_add(Duration::from_secs(5)) else {
            return tool_error("Tool deadline is unavailable");
        };
        let outcome = self.lease.invoke_workspace(read, commands, deadline);
        let CommandOutcome::Success { value, .. } = outcome else {
            return outcome_result(&outcome, false).0;
        };
        let text = serde_json::to_string(&value)
            .unwrap_or_else(|_| "Workspace result could not be serialized".into());
        json!({"content":[{"type":"text","text":text}],"structuredContent":value,"isError":false})
    }

    fn native_read(
        &self,
        name: &str,
        arguments: Value,
        now: Instant,
        commands: &dyn AgentCommandExecutor,
    ) -> Value {
        let Ok(request) = serde_json::from_value::<NativeRead>(arguments) else {
            return tool_error("Agent identity, account and targets are captured by the host");
        };
        if request.provider.is_some_and(|provider| {
            !matches!(name, "list_models" | "inspect_provider")
                || provider != self.lease.scope().provider
        }) {
            return tool_error("Provider discovery is limited to your captured provider account");
        }
        if request.limit.is_some_and(|limit| {
            name != "get_agent_activity" || !(1..=crate::MAX_NATIVE_ACTIVITY_ITEMS).contains(&limit)
        }) {
            return tool_error("Only activity accepts a limit, between 1 and 32");
        }
        let Some(deadline) = now.checked_add(Duration::from_secs(5)) else {
            return tool_error("Tool deadline is unavailable");
        };
        let outcome = self.lease.invoke_native(
            match name {
                "get_agent_status" => NativeToolRead::Status,
                "get_agent_activity" => NativeToolRead::Activity {
                    limit: request.limit.unwrap_or(16),
                },
                "list_agents" => NativeToolRead::Agents,
                "list_providers" | "inspect_provider" => NativeToolRead::Provider,
                "list_profiles" => NativeToolRead::Profiles,
                _ => NativeToolRead::Models,
            },
            commands,
            deadline,
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            return outcome_result(&outcome, false).0;
        };
        let structured = match name {
            "list_providers" => json!({"providers":[value]}),
            "list_models" | "list_agents" => {
                let Some(items) = value.as_array() else {
                    return tool_error("Agent owner returned an invalid catalog");
                };
                let count = items.len();
                if name == "list_models" {
                    json!({"count":count,"provider":self.lease.scope().provider,"models":value})
                } else {
                    json!({"count":count,"agents":value})
                }
            }
            _ => value,
        };
        let text = serde_json::to_string(&structured)
            .unwrap_or_else(|_| "Agent result could not be serialized".into());
        json!({"content":[{"type":"text","text":text}],"structuredContent":structured,"isError":false})
    }
}

fn spawn_receipt(value: Value) -> Value {
    let Value::Object(mut value) = value else {
        return value;
    };
    let native = value.remove("native").map(|native| json!({
        "id":native.get("id"),"generation":native.get("generation"),
        "title":native.get("title"),"provider":native.get("config").and_then(|config|config.get("provider")),
        "status":native.get("snapshot").and_then(|snapshot|snapshot.get("status")),
        "spawn_parent":native.get("spawn_parent"),
    }));
    let agent = value.remove("agent").map(|agent| {
        json!({
            "provider":agent.get("provider"),"target":agent.get("target"),
            "status":agent.get("observation").and_then(|observation|observation.get("status")),
        })
    });
    // Shared host callers retain full records. MCP keeps only issued IDs and public metadata.
    value.retain(|key, _| {
        matches!(
            key.as_str(),
            "identity" | "task_id" | "title" | "created" | "terminal"
        )
    });
    if let Some(native) = native {
        value.insert("native".into(), native);
    }
    if let Some(agent) = agent {
        value.insert("agent".into(), agent);
    }
    Value::Object(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRead {
    #[serde(default)]
    provider: Option<crate::AgentKind>,
    #[serde(default)]
    limit: Option<usize>,
}

fn native_output_schema(name: &str) -> Value {
    let agent = json!({"type":"object","properties":{
        "id":{"type":"string"},"title":{"type":"string"},"provider":{"type":"string"},
        "generation":{"type":"integer","minimum":1},"spawn_parent":{"type":"object"},
        "status":{"type":"string"},"approval":{"type":"boolean"},"input":{"type":"boolean"}},
        "required":["id","title","status"]});
    match name {
        "list_profiles" => json!({"type":"object","properties":{
            "provider":{"type":"string"},"captured_profile":{"type":["string","null"]},
            "profiles":{"type":"array","maxItems":16,"items":{"type":"object","properties":{
                "id":{"type":"string","maxLength":64},"name":{"type":"string","maxLength":256}},
                "required":["id","name"],"additionalProperties":false}}},
            "required":["provider","captured_profile","profiles"],"additionalProperties":false}),
        "get_workspace_info" => json!({"type":"object","properties":{
            "name":{"type":"string"},"backend":{"type":"string"},"host":{"type":"string"}},
            "required":["name","backend","host"],"additionalProperties":false}),
        "list_terminals" => json!({"type":"object","properties":{
            "terminals":{"type":"array","maxItems":128,"items":{"type":"object","properties":{
                "name":{"type":"string"},"session":{"type":"string"},"target":{"type":"object"}},
                "required":["name","session","target"],"additionalProperties":false}},
            "truncated":{"type":"boolean"}},"required":["terminals","truncated"],"additionalProperties":false}),
        "list_providers" | "inspect_provider" => {
            let provider = json!({"type":"object","properties":{
                "provider":{"type":"string"},"profile":{"type":["string","null"]},"model":{"type":["string","null"]},
                "reasoning_effort":{"type":["string","null"]},"fast_mode":{"type":"boolean"},
                "permissions":{"type":"string"},"permission_modes":{"type":"array","items":{"type":"string"}}},
                "required":["provider","profile","model","reasoning_effort","fast_mode","permissions","permission_modes"]});
            if name == "list_providers" {
                json!({"type":"object","properties":{"providers":{"type":"array","maxItems":1,"items":provider}},"required":["providers"]})
            } else {
                provider
            }
        }
        "get_agent_activity" => json!({"type":"object","properties":{
            "id":{"type":"string"},"provider":{"type":"string"},"status":{"type":"string"},
            "total":{"type":"integer","minimum":0},"items":{"type":"array","maxItems":crate::MAX_NATIVE_ACTIVITY_ITEMS,
                "items":{"type":"object","properties":{"id":{"type":"string"},"role":{"type":"string"},
                "text":{"type":"string"},"text_truncated":{"type":"boolean"},"complete":{"type":"boolean"},
                "tool":{"type":["object","null"],"properties":{"name":{"type":"string"},"status":{"type":"string"}}}},
                "required":["id","role","text","text_truncated","complete","tool"]}}},
            "required":["id","provider","status","total","items"]}),
        "list_agents" => {
            json!({"type":"object","properties":{"count":{"type":"integer","minimum":0},"agents":{"type":"array","items":agent}},"required":["count","agents"]})
        }
        "list_models" => {
            json!({"type":"object","properties":{"count":{"type":"integer","minimum":0},"provider":{"type":"string"},"models":{"type":"array","items":{"type":"object","properties":{
            "id":{"type":"string"},"display_name":{"type":"string"},"reasoning_efforts":{"type":"array","items":{"type":"string"}},"is_default":{"type":"boolean"},"is_favorite":{"type":"boolean"},"is_legacy":{"type":"boolean"}},"required":["id","display_name","reasoning_efforts"]}}},"required":["count","provider","models"]})
        }
        _ => agent,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCall {
    name: String,
    #[serde(default = "empty_arguments")]
    arguments: Value,
    #[serde(default)]
    _meta: Value,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ReadScope {
    #[default]
    Screen,
    History,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TerminalRead {
    scope: ReadScope,
    max_lines: u32,
}

impl Default for TerminalRead {
    fn default() -> Self {
        Self {
            scope: ReadScope::Screen,
            max_lines: MAX_LINES,
        }
    }
}

fn empty_arguments() -> Value {
    json!({})
}

fn valid_id(id: &Value) -> bool {
    id.as_u64().is_some()
        || id
            .as_str()
            .is_some_and(|id| !id.is_empty() && id.len() <= 128)
}

fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text", "text":message}], "isError":true})
}

fn image_result(snapshot: &ComputerResultSnapshot) -> Value {
    let (target, source, width, height) = snapshot.geometry();
    let caption = format!(
        "Window {}: PNG {}x{} pixels; source desktop points x={}, y={}, width={}, height={}. Pixel coordinates map to this source rectangle.",
        target.window_id, width, height, source.x, source.y, source.width, source.height,
    );
    json!({"content":[{"type":"image","mimeType":"image/png","data":snapshot.png_base64()},
        {"type":"text","text":caption}],"isError":false})
}

fn outcome_result(outcome: &CommandOutcome, image: bool) -> (Value, bool) {
    if image && let CommandOutcome::Success { value, .. } = outcome {
        return serde_json::from_value::<ComputerResultSnapshot>(value.clone()).map_or_else(
            |_| {
                (
                    tool_error("Computer capture returned an invalid image"),
                    false,
                )
            },
            |snapshot| (image_result(&snapshot), true),
        );
    }
    let failed = !matches!(outcome, CommandOutcome::Success { .. });
    let text = serde_json::to_string(outcome)
        .unwrap_or_else(|_| "Tool response could not be serialized".to_owned());
    (
        json!({"content":[{"type":"text","text":text}],"isError":failed}),
        false,
    )
}

fn application_action_schema() -> Value {
    let number = json!({"type":"number"});
    let mut variants = Vec::new();
    for (name, fields) in [
        ("focus", vec![]),
        (
            "click",
            vec![
                ("x", number.clone()),
                ("y", number.clone()),
                (
                    "button",
                    json!({"type":"string","enum":["left","right","middle"]}),
                ),
            ],
        ),
        ("move", vec![("x", number.clone()), ("y", number.clone())]),
        (
            "type_text",
            vec![(
                "text",
                json!({"type":"string","minLength":1,"maxLength":4096}),
            )],
        ),
        (
            "key",
            vec![
                (
                    "key",
                    json!({"type":"string","description":"return, tab, escape, backspace, delete, arrows, home, end, page_up, page_down, a-z, 0-9, or f1-f20"}),
                ),
                (
                    "modifiers",
                    json!({"type":"array","maxItems":4,"items":{"type":"string","enum":["command","control","option","shift"]}}),
                ),
            ],
        ),
        (
            "scroll",
            vec![
                ("x", number.clone()),
                ("y", number),
                (
                    "delta_x",
                    json!({"type":"integer","minimum":-10000,"maximum":10000}),
                ),
                (
                    "delta_y",
                    json!({"type":"integer","minimum":-10000,"maximum":10000}),
                ),
            ],
        ),
    ] {
        let mut properties = serde_json::Map::from_iter([("action".into(), json!({"const":name}))]);
        let mut required = vec!["action"];
        for (field, schema) in fields {
            required.push(field);
            properties.insert(field.into(), schema);
        }
        variants.push(json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}));
    }
    json!({"oneOf":variants})
}

fn spawned_terminal_tools() -> Vec<Value> {
    let mut tools = Vec::new();
    for (name, description, readonly, destructive) in [
        (
            "read_spawned_terminal",
            "Read up to 128 retained screen lines from a shell created under your current parent grant.",
            true,
            false,
        ),
        (
            "paste_spawned_terminal",
            "Paste literal text into a shell created under your current parent grant without submitting it. Other terminals are unavailable.",
            false,
            false,
        ),
        (
            "submit_spawned_terminal",
            "Send the terminal's encoded Enter key to a shell created under your current parent grant.",
            false,
            false,
        ),
        (
            "interrupt_spawned_terminal",
            "Send Ctrl-C to a shell created under your current parent grant.",
            false,
            false,
        ),
        (
            "close_spawned_terminal",
            "Close a shell pane created under your current parent grant. This ends its process; other terminals are unavailable.",
            false,
            true,
        ),
    ] {
        let mut properties = serde_json::Map::from_iter([(
            "terminal".to_owned(),
            json!({
                    "type":"object","properties":{
                        "kind":{"type":"string","enum":["terminal"]},
                        "handle":{"type":"string","minLength":1,"maxLength":8192},
                        "generation":{"type":"string","pattern":"^[1-9][0-9]{0,19}$"}},
                    "required":["kind","handle","generation"],"additionalProperties":false}),
        )]);
        let mut required = vec!["terminal"];
        if name == "paste_spawned_terminal" {
            properties.insert(
                "text".to_owned(),
                json!({"type":"string","maxLength":65536}),
            );
            required.push("text");
        }
        tools.push(json!({"name":name,"description":description,
                    "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
                    "annotations":{"readOnlyHint":readonly,"destructiveHint":destructive,"openWorldHint":false}}));
    }
    tools
}
