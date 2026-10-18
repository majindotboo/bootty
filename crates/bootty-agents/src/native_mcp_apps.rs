//! A Codex conversation's MCP apps reach their own servers through its thread.

use serde_json::{Value, json};

use crate::{AgentKind, McpAppTool, McpAppToolCall, NativeAgentSession};

impl NativeAgentSession {
    /// Reads `uri` from `server` for the app that showed the call `origin`.
    /// # Errors
    /// Returns a conversation without a Codex thread, or the server's refusal.
    pub fn read_mcp_app_resource(
        &self,
        server: &str,
        uri: &str,
        origin: &str,
    ) -> Result<Value, String> {
        let thread = self.mcp_app_thread()?;
        self.rpc(
            "mcpServer/resource/read",
            json!({"threadId": thread, "server": server, "uri": uri, "originCallId": origin}),
        )
        .map_err(|error| format!("Couldn't read {uri} from {server}: {error}"))
    }

    /// Calls `tool` on `server` for one of its apps. A tool its server doesn't mark read-only
    /// runs only once `approved`; until then this returns what to ask.
    /// # Errors
    /// Returns a conversation without a Codex thread, a tool kept from apps, or the server's
    /// refusal.
    pub fn call_mcp_app_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: &Value,
        approved: bool,
    ) -> Result<McpAppToolCall, String> {
        let thread = self.mcp_app_thread()?;
        let status = self
            .rpc(
                "mcpServerStatus/list",
                json!({"threadId": thread, "serverName": server, "detail": "toolsAndAuthOnly"}),
            )
            .map_err(|error| format!("Couldn't read {server}'s tools: {error}"))?;
        let definition = status
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(server))
            .and_then(|entry| entry.get("tools"))
            .and_then(|tools| tools.get(tool))
            .ok_or_else(|| format!("{server} has no tool named {tool}"))?;
        let found = McpAppTool::from_definition(tool, definition)?;
        if !found.read_only && !approved {
            return Ok(McpAppToolCall::Approve { title: found.title });
        }
        self.rpc(
            "mcpServer/tool/call",
            json!({"threadId": thread, "server": server, "tool": tool, "arguments": arguments}),
        )
        .map(McpAppToolCall::Result)
        .map_err(|error| format!("Couldn't run {tool} on {server}: {error}"))
    }

    fn mcp_app_thread(&self) -> Result<String, String> {
        if self.config.provider != AgentKind::Codex {
            return Err("Only Codex conversations show MCP apps".to_owned());
        }
        self.snapshot()
            .session_id
            .ok_or_else(|| "This conversation has no Codex thread yet".to_owned())
    }
}
