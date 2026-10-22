//! A provider's MCP connectors. The provider owns them: Bootty lists them through the provider's
//! own CLI, and adds them and signs in to them in a terminal the provider runs.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::{AgentKind, AgentLaunch};

/// One MCP server a provider connects to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderConnector {
    pub name: String,
    /// Where it runs: its URL, or the command that starts it.
    pub target: String,
    /// What the provider says about it, like "Connected" or "Needs authentication".
    pub status: String,
    pub needs_sign_in: bool,
}

/// What Bootty last learned of a provider's connectors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum ConnectorListing {
    Listing,
    Listed(Vec<ProviderConnector>),
    Failed(String),
}

/// Something done to a provider's connectors in a terminal the provider runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorAction<'a> {
    /// Adds a connector reached at a URL, or started by a command and its arguments.
    Add {
        name: &'a str,
        target: &'a str,
    },
    SignIn {
        name: &'a str,
    },
}

/// Whether a provider manages MCP connectors through its CLI. Pi's come from its extensions.
#[must_use]
pub const fn manages_connectors(provider: AgentKind) -> bool {
    matches!(provider, AgentKind::Codex | AgentKind::Claude)
}

/// Lists a provider's connectors in `account_directory`, or its default account.
/// # Errors
/// Returns a provider without connectors, a CLI that failed, or output it didn't shape as
/// documented.
pub fn terminal_connectors(
    provider: AgentKind,
    program: &str,
    account_directory: Option<&str>,
) -> Result<Vec<ProviderConnector>, String> {
    let _provider_use = crate::ProviderUse::acquire(program)?;
    let (arguments, timeout): (&[&str], _) = match provider {
        AgentKind::Codex => (&["mcp", "list", "--json"], Duration::from_secs(10)),
        // Claude connects to every server to report its health.
        AgentKind::Claude => (&["mcp", "list"], Duration::from_secs(45)),
        AgentKind::Pi => return Err("Pi's MCP servers come from its extensions".to_owned()),
    };
    let mut command = bootty_host::local_command(program);
    if let Some(directory) = account_directory {
        command.env(provider.account_directory_variable(), directory);
    }
    command.args(arguments);
    let output = crate::terminal_process::query_output_command(command, timeout, || false)?;
    if !output.successful {
        return Err(format!(
            "{} couldn't list its connectors",
            provider.display_name()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    match provider {
        AgentKind::Codex => parse_codex_connectors(&text),
        AgentKind::Claude => Ok(parse_claude_connectors(&text)),
        AgentKind::Pi => Ok(Vec::new()),
    }
}

/// Reads `codex mcp list --json`.
/// # Errors
/// Returns output that isn't the documented JSON list.
pub fn parse_codex_connectors(text: &str) -> Result<Vec<ProviderConnector>, String> {
    let servers: Vec<Value> = serde_json::from_str(text)
        .map_err(|error| format!("Codex listed its connectors unreadably: {error}"))?;
    Ok(servers
        .iter()
        .filter_map(|server| {
            let name = server.get("name")?.as_str()?.to_owned();
            let transport = server.get("transport");
            let field = |key: &str| transport.and_then(|t| t.get(key)).and_then(Value::as_str);
            let target = field("url").map_or_else(
                || {
                    std::iter::once(field("command").unwrap_or_default())
                        .chain(
                            transport
                                .and_then(|t| t.get("args"))
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str),
                        )
                        .collect::<Vec<_>>()
                        .join(" ")
                },
                str::to_owned,
            );
            let auth = server
                .get("auth_status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let enabled = server.get("enabled").and_then(Value::as_bool) != Some(false);
            let needs_sign_in = auth == "not_logged_in";
            let status = if !enabled {
                "Turned off"
            } else if needs_sign_in {
                "Needs authentication"
            } else if matches!(auth, "o_auth" | "oauth" | "bearer_token") {
                "Signed in"
            } else {
                "Configured"
            };
            Some(ProviderConnector {
                name,
                target,
                status: status.to_owned(),
                needs_sign_in,
            })
        })
        .collect())
}

/// Reads `claude mcp list`, one server a line: `name: target (TRANSPORT) - ✔ Connected`.
#[must_use]
pub fn parse_claude_connectors(text: &str) -> Vec<ProviderConnector> {
    text.lines()
        .filter_map(|line| {
            // Names hold colons, as in `plugin:linear:linear`; URLs never hold ": ".
            let (name, rest) = line.split_once(": ")?;
            let (target, status) = rest.rsplit_once(" - ")?;
            let status = status
                .trim_start_matches(|character: char| !character.is_alphanumeric())
                .trim()
                .to_owned();
            Some(ProviderConnector {
                name: name.trim().to_owned(),
                target: target.trim().to_owned(),
                needs_sign_in: status.to_ascii_lowercase().contains("auth"),
                status,
            })
        })
        .collect()
}

/// The provider's own command for `action`, to run in a terminal it owns.
/// # Errors
/// Returns a provider without connectors, or a connector without a name or target.
pub fn terminal_connector_launch(
    provider: AgentKind,
    program: &str,
    action: ConnectorAction<'_>,
) -> Result<AgentLaunch, String> {
    if !manages_connectors(provider) {
        return Err(format!(
            "{} has no connectors to manage",
            provider.display_name()
        ));
    }
    let name = match action {
        ConnectorAction::Add { name, .. } | ConnectorAction::SignIn { name } => name.trim(),
    };
    if name.is_empty() || name.starts_with('-') {
        return Err("Name the connector".to_owned());
    }
    let mut arguments = vec!["mcp".to_owned()];
    match action {
        ConnectorAction::SignIn { .. } => {
            arguments.extend(["login".to_owned(), name.to_owned()]);
        }
        ConnectorAction::Add { target, .. } => {
            let target = target.trim();
            if target.is_empty() {
                return Err("Give the connector's URL or command".to_owned());
            }
            arguments.push("add".to_owned());
            let url = target.starts_with("https://") || target.starts_with("http://");
            match (provider, url) {
                (AgentKind::Claude, true) => arguments.extend([
                    "--transport".to_owned(),
                    "http".to_owned(),
                    name.to_owned(),
                    target.to_owned(),
                ]),
                (_, true) => {
                    arguments.extend([name.to_owned(), "--url".to_owned(), target.to_owned()]);
                }
                // A command splits at spaces; quoted arguments need adding in the provider's
                // own terminal until Bootty has a shell-word parser.
                (_, false) => {
                    arguments.extend([name.to_owned(), "--".to_owned()]);
                    arguments.extend(target.split_whitespace().map(str::to_owned));
                }
            }
        }
    }
    Ok(AgentLaunch {
        program: program.to_owned(),
        cwd: None,
        arguments,
        ephemeral: true,
        account_directory: None,
    })
}
