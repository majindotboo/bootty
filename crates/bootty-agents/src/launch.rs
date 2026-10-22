use bootty_control::CommandTarget;
use serde::{Deserialize, Serialize};

use crate::AgentKind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaunchShell {
    #[default]
    Posix,
    Windows,
}

impl LaunchShell {
    pub(crate) fn absolute_path(self, value: &str) -> bool {
        match self {
            Self::Posix => value.starts_with('/'),
            Self::Windows => {
                if matches!(value.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
                {
                    return true;
                }
                let Some(unc) = value
                    .strip_prefix(r"\\")
                    .or_else(|| value.strip_prefix("//"))
                else {
                    return false;
                };
                let mut parts = unc.split(['\\', '/']);
                parts.next().is_some_and(|part| !part.is_empty())
                    && parts.next().is_some_and(|part| !part.is_empty())
            }
        }
    }

    pub(crate) const fn local() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// Facts captured by the host before an asynchronous agent command starts.
#[derive(Clone, Debug, Default)]
pub struct AgentLaunchContext {
    pub new_tab: Option<CommandTarget>,
    pub pane: Option<String>,
    pub cwd: Option<String>,
    pub shell: LaunchShell,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentLaunch {
    pub program: String,
    pub cwd: Option<String>,
    pub arguments: Vec<String>,
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(default)]
    pub account_directory: Option<String>,
}

impl AgentLaunch {
    pub(crate) fn to_value(&self) -> serde_json::Value {
        serde_json::Value::Object(
            [
                ("program".to_owned(), self.program.clone().into()),
                (
                    "cwd".to_owned(),
                    self.cwd.clone().map_or(serde_json::Value::Null, Into::into),
                ),
                ("arguments".to_owned(), self.arguments.clone().into()),
                ("ephemeral".to_owned(), self.ephemeral.into()),
                (
                    "account_directory".to_owned(),
                    self.account_directory
                        .clone()
                        .map_or(serde_json::Value::Null, Into::into),
                ),
            ]
            .into_iter()
            .collect(),
        )
    }

    /// # Errors
    /// Returns invalid program/cwd controls, oversized values, or NUL in literal arguments.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_for_shell(LaunchShell::local())
    }

    pub(crate) fn validate_for_shell(&self, shell: LaunchShell) -> Result<(), String> {
        if self.program.is_empty() || self.program.starts_with('-') {
            return Err("Agent program must be an executable name or path".to_owned());
        }
        if self
            .account_directory
            .as_ref()
            .is_some_and(|directory| !shell.absolute_path(directory))
        {
            return Err("Agent account directory must be an absolute path".to_owned());
        }
        if self.arguments.len() > 64 {
            return Err("Agent launch accepts at most 64 arguments".to_owned());
        }
        let total = self.arguments.iter().map(String::len).fold(
            self.program
                .len()
                .saturating_add(self.cwd.as_ref().map_or(0, String::len))
                .saturating_add(self.account_directory.as_ref().map_or(0, String::len)),
            usize::saturating_add,
        );
        if total > 64 * 1024 {
            return Err("Agent launch exceeds 64 KiB".to_owned());
        }
        for value in std::iter::once(&self.program)
            .chain(self.cwd.iter())
            .chain(self.account_directory.iter())
        {
            if value.len() > 8192 || value.chars().any(char::is_control) {
                return Err(
                    "Agent launch values must be at most 8192 bytes without control characters"
                        .to_owned(),
                );
            }
        }
        if self
            .arguments
            .iter()
            .any(|value| value.len() > 8192 || value.contains('\0'))
        {
            return Err("Agent arguments must be at most 8192 bytes without NUL".to_owned());
        }
        Ok(())
    }

    /// Only reusable configuration options are retained. Prompts, credentials, arbitrary
    /// config overrides and session selectors must not enter retained metadata or be replayed.
    #[must_use]
    pub fn retained(&self, provider: AgentKind) -> Self {
        let valued: &[&str] = match provider {
            AgentKind::Pi => &[
                "--provider",
                "--model",
                "--thinking",
                "--session-dir",
                "--extension",
                "-e",
            ],
            AgentKind::Codex => &[
                "--model",
                "-m",
                "--profile",
                "-p",
                "--sandbox",
                "-s",
                "--ask-for-approval",
                "-a",
                "--add-dir",
            ],
            AgentKind::Claude => &["--model", "--agent", "--effort", "--permission-mode"],
        };
        let switches: &[&str] = match provider {
            AgentKind::Pi => &[],
            AgentKind::Codex => &["--no-alt-screen", "--search", "--oss"],
            AgentKind::Claude => &["--chrome", "--no-chrome"],
        };
        let mut arguments = Vec::new();
        let mut ephemeral = self.ephemeral;
        let mut source = self.arguments.iter();
        while let Some(arg) = source.next() {
            if arg == "--" {
                break;
            }
            if matches!(
                arg.as_str(),
                "--no-session" | "--no-session-persistence" | "--ephemeral"
            ) {
                ephemeral = true;
            }
            if valued.contains(&arg.as_str()) {
                if let Some(value) = source.next() {
                    arguments.extend([arg.clone(), value.clone()]);
                }
            } else if switches.contains(&arg.as_str())
                || arg
                    .split_once('=')
                    .is_some_and(|(key, _)| valued.contains(&key))
            {
                arguments.push(arg.clone());
            }
        }
        Self {
            program: self.program.clone(),
            cwd: self.cwd.clone(),
            arguments,
            ephemeral,
            account_directory: self.account_directory.clone(),
        }
    }

    /// # Errors
    /// Returns an error if persistence is disabled or the session selector is invalid.
    pub fn session_arguments(
        &self,
        provider: AgentKind,
        session: &str,
        fork: bool,
    ) -> Result<Vec<String>, String> {
        let retained = self.retained(provider);
        if retained.ephemeral {
            return Err("This agent disabled session persistence".to_owned());
        }
        if session.is_empty()
            || session.len() > 8192
            || session.starts_with('-')
            || session.chars().any(char::is_control)
        {
            return Err(
                "A valid explicit agent session ID or Pi session path is required".to_owned(),
            );
        }
        let mut arguments = retained.arguments;
        match provider {
            AgentKind::Pi => arguments.extend([
                if fork { "--fork" } else { "--session" }.to_owned(),
                session.to_owned(),
            ]),
            AgentKind::Codex => {
                arguments.splice(
                    0..0,
                    [
                        if fork { "fork" } else { "resume" }.to_owned(),
                        session.to_owned(),
                    ],
                );
            }
            AgentKind::Claude => {
                arguments.extend(["--resume".to_owned(), session.to_owned()]);
                if fork {
                    arguments.push("--fork-session".to_owned());
                }
            }
        }
        Ok(arguments)
    }

    /// # Errors
    /// Returns an error for invalid launch values, failed context serialization, or a Windows command line exceeding its limit.
    pub fn shell_command(&self, provider: AgentKind, shell: LaunchShell) -> Result<String, String> {
        self.validate_for_shell(shell)?;
        let context =
            serde_json::to_string(&self.retained(provider)).map_err(|error| error.to_string())?;
        match shell {
            LaunchShell::Posix => {
                let argv = std::iter::once(&self.program)
                    .chain(&self.arguments)
                    .map(|value| quote_posix(value))
                    .collect::<Vec<_>>()
                    .join(" ");
                let account =
                    self.account_directory
                        .as_ref()
                        .map_or_else(String::new, |directory| {
                            format!(
                                " {}",
                                quote_posix(&format!(
                                    "{}={directory}",
                                    provider.account_directory_variable()
                                ))
                            )
                        });
                let launch = format!(
                    "exec env BOOTTY_AGENT_LAUNCH_CONTEXT={}{account} {argv}",
                    quote_posix(&context)
                );
                Ok(self.cwd.as_ref().filter(|cwd| !cwd.is_empty()).map_or_else(
                    || launch.clone(),
                    |cwd| format!("cd {} && {launch}", quote_posix(cwd)),
                ))
            }
            LaunchShell::Windows => {
                let owner = std::env::current_exe().map_err(|error| error.to_string())?;
                let command = self
                    .windows_argv(&owner, provider, &self.arguments, Some(&context))?
                    .join(" ");
                if command.len() > 30_000 {
                    return Err("Agent launch exceeds the Windows command-line limit".to_owned());
                }
                Ok(command)
            }
        }
    }

    pub(crate) fn windows_argv(
        &self,
        owner: &std::path::Path,
        provider: AgentKind,
        arguments: &[String],
        context: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let mut environment = Vec::new();
        if let Some(account) = &self.account_directory {
            environment.push((
                provider.account_directory_variable().to_owned(),
                account.clone(),
            ));
        }
        if let Some(context) = context {
            environment.push(("BOOTTY_AGENT_LAUNCH_CONTEXT".to_owned(), context.to_owned()));
        }
        bootty_host::windows_terminal_argv(
            owner,
            &self.program,
            arguments,
            self.cwd.as_deref().filter(|cwd| !cwd.is_empty()),
            &environment,
        )
        .map_err(|error| error.to_string())
    }
}

pub fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
