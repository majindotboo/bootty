use std::io;

use anyhow::Result;
use bootty_control::{
    Caller, CommandInvocation, CommandOutcome, TerminalToolOperation, serve_terminal_tools,
};
use serde_json::json;

use crate::cli::AgentToolsArgs;

/// Run only the own-terminal MCP surface against the already-running owner.
/// # Errors
/// Returns bounded stdio framing or write failures.
pub fn run(args: &AgentToolsArgs) -> Result<()> {
    serve_terminal_tools(io::stdin().lock(), io::stdout().lock(), |operation| {
        invoke(args, operation)
    })?;
    Ok(())
}

fn invoke(args: &AgentToolsArgs, operation: TerminalToolOperation) -> CommandOutcome {
    let request = json!({"attachment_id":args.attachment,"provider":args.provider,"binding_id":args.binding,"operation":operation});
    let invocation = CommandInvocation::new(
        format!("agents.{}.tools", args.provider),
        vec![request.to_string()],
        Caller::Socket,
    );
    let owner = match bootty_control::running_instance() {
        Ok(Some(owner)) if owner.instance_id == args.instance => owner,
        Ok(_) => {
            return CommandOutcome::Unavailable {
                message: "The original Bootty terminal tool owner is unavailable".to_owned(),
            };
        }
        Err(error) => {
            return CommandOutcome::Unavailable {
                message: error.to_string(),
            };
        }
    };
    match bootty_control::invoke_instance(
        &owner,
        "command.invoke",
        json!({"invocation":invocation}),
    ) {
        Ok(response) => response
            .result
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_else(|| CommandOutcome::Unavailable {
                message: "The Bootty terminal tool owner rejected the request".to_owned(),
            }),
        Err(error) => CommandOutcome::Unavailable {
            message: error.to_string(),
        },
    }
}
