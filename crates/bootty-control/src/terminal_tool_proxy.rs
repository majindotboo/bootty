use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::CommandOutcome;

const REQUEST_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: usize = 1024 * 1024;
const TOOL_NAME: &str = "bootty_terminal_read";

/// The complete operation surface of the own-terminal MCP proxy.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalToolOperation {
    List,
    Read,
}

/// Serve bounded newline-delimited MCP requests. The host revalidates its live attachment
/// for every list and call; this transport never accepts command names or target overrides.
/// # Errors
/// Returns framing, stream, or response-size errors.
pub fn serve_terminal_tools(
    mut input: impl BufRead,
    mut output: impl Write,
    mut invoke: impl FnMut(TerminalToolOperation) -> CommandOutcome,
) -> io::Result<()> {
    loop {
        let mut line = String::new();
        if io::Read::take(
            &mut input,
            u64::try_from(REQUEST_LIMIT + 1).unwrap_or(u64::MAX),
        )
        .read_line(&mut line)?
            == 0
        {
            return Ok(());
        }
        if line.len() > REQUEST_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP request exceeds 64 KiB",
            ));
        }
        let response = serde_json::from_str(&line).map_or_else(
            |_| Some(error(&Value::Null, -32700, "Invalid JSON")),
            |request| terminal_tool_response(&request, &mut invoke),
        );
        if let Some(response) = response {
            let bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
            if bytes.len() > RESPONSE_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP response exceeds 1 MiB",
                ));
            }
            output.write_all(&bytes)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

fn terminal_tool_response(
    request: &Value,
    invoke: &mut impl FnMut(TerminalToolOperation) -> CommandOutcome,
) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || request.get("method").and_then(Value::as_str).is_none()
        || !(id.is_null() || id.is_number() || id.is_string())
    {
        return Some(error(&Value::Null, -32600, "Invalid JSON-RPC request"));
    }
    // MCP notifications never receive responses, including initialized/cancelled notifications.
    request.get("id")?;
    let params = request.get("params").unwrap_or(&Value::Null);
    let result = match request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "initialize" => {
            let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
                return Some(error(&id, -32602, "Missing MCP protocol version"));
            };
            let version = match requested {
                "2024-11-05" | "2025-03-26" | "2025-06-18" => requested,
                _ => "2025-06-18",
            };
            json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"bootty-terminal","version":env!("CARGO_PKG_VERSION")}})
        }
        "ping" => json!({}),
        "tools/list" => {
            let enabled = match invoke(TerminalToolOperation::List) {
                CommandOutcome::Success { value, .. } => {
                    value.get("enabled").and_then(Value::as_bool) == Some(true)
                }
                _ => false,
            };
            let tools = if enabled {
                vec![
                    json!({"name":TOOL_NAME,"description":"Read this agent's own terminal screen.",
                    "inputSchema":{"type":"object","properties":{},"additionalProperties":false},
                    "annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}),
                ]
            } else {
                Vec::new()
            };
            json!({"tools":tools})
        }
        "tools/call" => {
            if params.get("name").and_then(Value::as_str) != Some(TOOL_NAME)
                || params.get("arguments").is_some_and(|arguments| {
                    arguments
                        .as_object()
                        .is_none_or(|arguments| !arguments.is_empty())
                })
            {
                return Some(error(
                    &id,
                    -32602,
                    "Expected bootty_terminal_read with no arguments",
                ));
            }
            let outcome = invoke(TerminalToolOperation::Read);
            let (is_error, text) = match outcome {
                CommandOutcome::Success { value, .. } => (false, value.to_string()),
                outcome => (
                    true,
                    serde_json::to_value(outcome)
                        .unwrap_or(Value::Null)
                        .to_string(),
                ),
            };
            json!({"content":[{"type":"text","text":text}],"isError":is_error})
        }
        _ => return Some(error(&id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
