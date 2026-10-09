//! First-hand MCP decisions; accepting a tool permission never creates a reusable grant.
use serde_json::Value;

use crate::native_protocol::field;

pub fn is_approval(parameters: &Value) -> bool {
    let schema = field(parameters, "requestedSchema");
    let required = field(schema, "required");
    field(parameters, "mode") == "form"
        && field(schema, "type") == "object"
        && field(schema, "properties")
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
        && (required.is_null() || required.as_array().is_some_and(Vec::is_empty))
}

pub fn validate_response(parameters: &Value, response: &Value) -> Result<(), String> {
    if response.as_object().is_none_or(|fields| {
        fields
            .keys()
            .any(|key| !matches!(key.as_str(), "action" | "content"))
    }) {
        return Err("MCP decisions require an explicit action and optional content".into());
    }
    match field(response, "action").as_str() {
        Some("decline" | "cancel") if field(response, "content").is_null() => Ok(()),
        Some("accept") if is_approval(parameters) => {
            if field(response, "content")
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
            {
                Ok(())
            } else {
                Err("MCP tool approval requires an empty content object".into())
            }
        }
        // Form fields and authenticated URL/device proofs need their own input adapters.
        // Keep these requests pending and cancellable until those adapters are supported.
        Some("accept") => {
            Err("This MCP request requires structured input or authentication".into())
        }
        _ => Err("MCP requests require accept, decline or cancel without invented content".into()),
    }
}
