use bootty_control::{CommandInvocation, CommandOutcome};

use super::LaunchContext;

pub(super) fn validate(
    invocation: &CommandInvocation,
    context: &LaunchContext,
) -> Result<(), CommandOutcome> {
    if context.parent.is_none() {
        return Err(CommandOutcome::StaleTarget {
            message: "An agent split requires its captured terminal".to_owned(),
        });
    }
    if !matches!(
        invocation.arguments.first().map(String::as_str),
        Some("right" | "down")
    ) {
        return Err(CommandOutcome::Failed {
            code: "terminal_agent_failed".to_owned(),
            message: "Choose a right or down split".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn create_invocation(
    invocation: &CommandInvocation,
    context: &LaunchContext,
    argv: String,
    cwd: Option<String>,
) -> CommandInvocation {
    let mut create = CommandInvocation::new(
        "terminal.create_pane",
        vec![
            invocation.arguments.first().cloned().unwrap_or_default(),
            argv,
            cwd.unwrap_or_default(),
        ],
        invocation.caller,
    );
    create.target.clone_from(&context.parent);
    create
}
