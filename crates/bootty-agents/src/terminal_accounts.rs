use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentKind, AgentLaunch};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalAccountStatus {
    pub provider: AgentKind,
    pub authenticated: Option<bool>,
    pub detail: Option<String>,
}

/// Query only provider-supported readiness, discarding every credential and unknown field.
/// # Errors
/// Returns unsupported discovery, subprocess or malformed response errors.
pub fn terminal_account_status(
    provider: AgentKind,
    program: &str,
    provider_id: Option<&str>,
) -> Result<TerminalAccountStatus, String> {
    let authenticated = match provider {
        AgentKind::Claude => {
            let output = crate::terminal_process::query_output(
                program,
                &["auth", "status", "--json"],
                || false,
            )?;
            let value: Value =
                serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
            value
                .get("loggedIn")
                .and_then(Value::as_bool)
                .ok_or("Claude did not report account readiness")?
        }
        AgentKind::Codex => {
            let output =
                crate::terminal_process::query_output(program, &["login", "status"], || false)?;
            let text = String::from_utf8_lossy(&output.stderr);
            if text.trim().starts_with("Logged in") {
                true
            } else if text.contains("Not logged in") {
                false
            } else {
                return Err("Codex did not report account readiness".to_owned());
            }
        }
        AgentKind::Pi => {
            let provider_id = provider_id
                .filter(|id| !id.is_empty() && id.len() <= 256 && !id.starts_with('-'))
                .ok_or("Choose a Pi model provider to check its account")?;
            let output = crate::terminal_process::query_output(
                program,
                &[
                    "auth",
                    "check",
                    "--provider",
                    provider_id,
                    "--json",
                    "--no-refresh",
                ],
                || false,
            )?;
            let value: Value =
                serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
            match value.get("status").and_then(Value::as_str) {
                Some("ready") => true,
                Some("not_ready") => false,
                _ => return Err("Pi did not report valid account readiness".to_owned()),
            }
        }
    };
    Ok(TerminalAccountStatus {
        provider,
        authenticated: Some(authenticated),
        detail: None,
    })
}

/// Login always uses the provider's own terminal UI and existing account store.
#[must_use]
pub fn terminal_account_launch(provider: AgentKind, program: &str, logout: bool) -> AgentLaunch {
    let arguments: Vec<String> = match provider {
        AgentKind::Codex => {
            if logout {
                vec!["logout"]
            } else {
                vec!["login", "--device-auth"]
            }
        }
        AgentKind::Claude => vec!["auth", if logout { "logout" } else { "login" }],
        // Pi's supported login/logout UI is its interactive slash command menu.
        AgentKind::Pi => Vec::new(),
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    AgentLaunch {
        program: program.to_owned(),
        cwd: None,
        arguments,
        ephemeral: true,
    }
}
