use crate::{AgentKind, AgentLaunch, PiAccountSelector};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TerminalAccountStatus {
    pub provider: AgentKind,
    pub authenticated: Option<bool>,
    pub detail: Option<String>,
    pub account: Option<String>,
    pub auth_method: Option<String>,
    pub subscription: Option<String>,
}

/// Query only provider-supported readiness, discarding every credential and unknown field.
/// # Errors
/// Returns unsupported discovery, subprocess or malformed response errors.
pub fn terminal_account_status(
    provider: AgentKind,
    program: &str,
    provider_id: Option<&str>,
) -> Result<TerminalAccountStatus, String> {
    terminal_account_status_in(provider, program, provider_id, None)
}

/// # Errors
/// Returns provider readiness errors without retaining authentication payloads.
pub fn terminal_account_status_in(
    provider: AgentKind,
    program: &str,
    provider_id: Option<&str>,
    directory: Option<&str>,
) -> Result<TerminalAccountStatus, String> {
    let selector = provider_id.map(PiAccountSelector::provider);
    terminal_account_status_with_pi_selector_in(provider, program, selector.as_ref(), directory)
}

/// Query the exact typed Pi selector; other providers use their own account protocol.
/// # Errors
/// Returns unsupported scope, subprocess or malformed response errors without credential payloads.
pub fn terminal_account_status_with_pi_selector_in(
    provider: AgentKind,
    program: &str,
    selector: Option<&PiAccountSelector>,
    directory: Option<&str>,
) -> Result<TerminalAccountStatus, String> {
    let environment = directory.map(|directory| (provider.account_directory_variable(), directory));
    match provider {
        AgentKind::Claude => {
            let output = crate::terminal_process::query_output_in(
                program,
                &["auth", "status", "--json"],
                environment,
                || false,
            )?;
            crate::terminal_account_response::claude(&output.stdout, output.successful)
        }
        AgentKind::Codex => crate::terminal_codex_account::query(program, directory),
        AgentKind::Pi => {
            let scope = crate::terminal_pi_account::scope(selector, directory)?;
            let mut arguments = vec!["auth", "check"];
            if scope.explicit_provider {
                arguments.extend(["--provider", scope.provider.as_str()]);
            }
            if let Some(model) = &scope.model {
                arguments.extend(["--model", model]);
            }
            arguments.extend(["--json", "--no-refresh"]);
            let output = crate::terminal_process::query_output_in(
                program,
                &arguments,
                Some((provider.account_directory_variable(), &scope.directory)),
                || false,
            )?;
            let mut result = crate::terminal_account_response::pi(
                &output.stdout,
                output.successful,
                &scope.provider,
            )?;
            if result.authenticated == Some(true) {
                result.detail = Some(format!(
                    "Pi model provider: {}. Pi does not report subscription information.",
                    scope.provider
                ));
            }
            Ok(result)
        }
    }
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
        account_directory: None,
    }
}
