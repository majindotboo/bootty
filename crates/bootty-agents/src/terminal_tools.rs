use std::{
    path::Path,
    sync::{Mutex, PoisonError},
    time::Instant,
};

use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget,
    TerminalToolOperation,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{AgentCommandExecutor, AgentKind, AgentLaunch, TerminalAgentService};

/// Process-local MCP configuration. Explicit provider tool configuration wins; this first
/// increment supports local Codex and Claude terminal launches only.
#[must_use]
pub fn terminal_tools_supported(provider: AgentKind, launch: &AgentLaunch) -> bool {
    if provider == AgentKind::Pi
        || Path::new(&launch.program)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(provider.default_program())
    {
        return false;
    }
    !launch.arguments.iter().any(|argument| match provider {
        AgentKind::Claude => [
            "--mcp-config",
            "--strict-mcp-config",
            "--safe-mode",
            "--tools",
            "--disallowedTools",
            "--disallowed-tools",
        ]
        .iter()
        .any(|flag| argument == flag || argument.starts_with(&format!("{flag}="))),
        AgentKind::Codex => {
            argument.contains("mcp_servers.bootty_terminal")
                || argument == "--remote"
                || argument.starts_with("--remote=")
        }
        AgentKind::Pi => true,
    })
}

/// Add only a stdio own-terminal read proxy; never alter approval or sandbox options.
/// A failed attachment preserves the original launch.
/// # Errors
/// Returns invalid executable paths or serialization errors.
pub fn attach_terminal_tool_arguments(
    launch: &mut AgentLaunch,
    provider: AgentKind,
    executable: &Path,
    binding_id: &str,
    attachment_id: &str,
    instance: &str,
) -> Result<(), String> {
    let executable = executable
        .to_str()
        .ok_or_else(|| "Bootty executable path is not UTF-8".to_owned())?;
    let arguments = [
        "agent-tools",
        "--provider",
        provider.default_program(),
        "--binding",
        binding_id,
        "--attachment",
        attachment_id,
        "--instance",
        instance,
    ];
    let options = match provider {
        AgentKind::Claude => vec![
            "--mcp-config".to_owned(),
            json!({"mcpServers":{"bootty_terminal":{"command":executable,"args":arguments}}})
                .to_string(),
        ],
        AgentKind::Codex => vec![
            "-c".to_owned(),
            format!("mcp_servers.bootty_terminal.command={}", json!(executable)),
            "-c".to_owned(),
            format!("mcp_servers.bootty_terminal.args={}", json!(arguments)),
        ],
        AgentKind::Pi => return Err("Pi terminal tools are not supported yet".to_owned()),
    };
    let mut attached = launch.clone();
    attached.arguments.splice(0..0, options);
    attached.validate()?;
    launch.arguments = attached.arguments;
    Ok(())
}

/// Correlation with an ephemeral own-terminal attachment, never an authentication credential.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalToolRequest {
    pub attachment_id: String,
    pub provider: AgentKind,
    pub binding_id: String,
    pub operation: TerminalToolOperation,
}

struct TerminalToolLease {
    attachment_id: String,
    provider: AgentKind,
    binding_id: String,
    target: Option<CommandTarget>,
}

/// Live own-terminal policy. Restart deliberately disables all prior attachments.
#[derive(Default)]
pub struct TerminalTools(Mutex<Vec<TerminalToolLease>>);

impl TerminalTools {
    pub fn reserve(&self, provider: AgentKind, binding_id: &str) -> Result<String, String> {
        if binding_id.is_empty() {
            return Err("Agent tools require a binding".to_owned());
        }
        let mut attachments = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if attachments.len() >= 128 {
            return Err("Terminal tool attachments are full".to_owned());
        }
        let id = uuid::Uuid::new_v4().to_string();
        attachments.push(TerminalToolLease {
            attachment_id: id.clone(),
            provider,
            binding_id: binding_id.to_owned(),
            target: None,
        });
        drop(attachments);
        Ok(id)
    }

    pub fn complete(
        &self,
        id: &str,
        provider: AgentKind,
        binding_id: &str,
        target: &CommandTarget,
    ) -> Result<(), String> {
        let mut attachments = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let attachment = attachments
            .iter_mut()
            .find(|attachment| attachment.attachment_id == id)
            .ok_or_else(|| "Terminal tool attachment was revoked".to_owned())?;
        if attachment.provider != provider
            || attachment.binding_id != binding_id
            || attachment
                .target
                .as_ref()
                .is_some_and(|prior| prior != target)
        {
            return Err(
                "Terminal tools cannot move to another provider, Space, or terminal".to_owned(),
            );
        }
        attachment.target = Some(target.clone());
        drop(attachments);
        Ok(())
    }

    pub fn revoke(&self, id: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|attachment| attachment.attachment_id != id);
    }

    pub fn invoke(
        &self,
        service: &TerminalAgentService,
        request: &TerminalToolRequest,
        executor: &dyn AgentCommandExecutor,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandOutcome {
        if cancellation.is_cancelled() {
            return CommandOutcome::cancelled();
        }
        if Instant::now() >= deadline {
            return CommandOutcome::deadline_exceeded();
        }
        let attachments = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(attachment) = attachments.iter().find(|attachment| {
            attachment.attachment_id == request.attachment_id
                && attachment.provider == request.provider
                && attachment.binding_id == request.binding_id
        }) else {
            return CommandOutcome::Denied {
                message:
                    "Terminal tools are disabled, revoked, or belong to another provider or Space"
                        .to_owned(),
            };
        };
        let target = attachment.target.clone();
        if let Some(target) = &target
            && service.record(target).is_none_or(|record| {
                record.provider != request.provider || record.binding_id != request.binding_id
            })
        {
            return CommandOutcome::Denied {
                message: "The agent terminal record is no longer valid".to_owned(),
            };
        }
        // A pending launch may advertise this fixed tool; it cannot read until the backend
        // returns an exact terminal and the retained record has committed.
        if request.operation == TerminalToolOperation::List && target.is_none() {
            return CommandOutcome::Success {
                value: json!({"enabled":true}),
                warnings: Vec::new(),
            };
        }
        let Some(target) = target else {
            return CommandOutcome::Unavailable {
                message: "The agent terminal is not ready".to_owned(),
            };
        };
        drop(attachments);
        let mut invocation = CommandInvocation::new("terminal.read", Vec::new(), Caller::Socket);
        invocation.target = Some(target);
        let outcome = executor.execute(invocation, deadline, cancellation);
        if request.operation == TerminalToolOperation::List
            && matches!(outcome, CommandOutcome::Success { .. })
        {
            CommandOutcome::Success {
                value: json!({"enabled":true}),
                warnings: Vec::new(),
            }
        } else {
            outcome
        }
    }
}
