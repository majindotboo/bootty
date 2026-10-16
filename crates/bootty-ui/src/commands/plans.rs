//! Carrying out a plan in a new conversation of the same session.

use bootty_agents::{AgentKind, NativeModelSelection, NativePermissionMode, NativeSessionRecord};
use bootty_control::{Caller, CommandInvocation, CommandTarget};

/// The `agents.native.tab` command that carries out `plan` in a new conversation of `record`'s
/// session.
///
/// It starts on `binding` with the same provider, account settings, model and permissions.
#[must_use]
pub fn implement_plan_invocation(
    record: &NativeSessionRecord,
    binding: CommandTarget,
    plan: &str,
) -> CommandInvocation {
    let config = &record.config;
    let mut arguments = vec![
        config.provider.to_string(),
        config.cwd.to_string_lossy().into_owned(),
        config.program.clone(),
        serde_json::to_string(&config.arguments).unwrap_or_else(|_| "[]".to_owned()),
        format!("implement-{}", record.id),
        config.profile.clone().unwrap_or_default(),
        record.task_identity.clone().unwrap_or_default(),
        "Implement the plan".to_owned(),
        format!("Implement this plan:\n\n{plan}"),
    ];
    if let Some(model) = &config.model {
        let selection = NativeModelSelection {
            plan_mode: Some(false),
            fast_mode: Some(config.fast_mode),
            model: model.clone(),
            reasoning_effort: config.reasoning_effort.clone(),
        };
        arguments.resize(11, String::new());
        arguments.push(serde_json::to_string(&selection).unwrap_or_default());
    }
    if config.provider != AgentKind::Pi
        && config.permissions != NativePermissionMode::ProviderDefault
    {
        arguments.resize(14, String::new());
        arguments.push(config.permissions.id().to_owned());
    }
    let mut invocation = CommandInvocation::new("agents.native.tab", arguments, Caller::Internal);
    invocation.target = Some(binding);
    invocation
}
