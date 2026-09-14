use base64::Engine as _;
use bootty_control::CommandTarget;
use serde::{Deserialize, Serialize};

use crate::AgentKind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaunchShell {
    #[default]
    Posix,
    Windows,
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
            ]
            .into_iter()
            .collect(),
        )
    }

    /// # Errors
    /// Returns an error for an invalid program, oversized arguments, or control characters in launch values.
    pub fn validate(&self) -> Result<(), String> {
        if self.program.is_empty() || self.program.starts_with('-') {
            return Err("Agent program must be an executable name or path".to_owned());
        }
        if self.arguments.len() > 64 {
            return Err("Agent launch accepts at most 64 arguments".to_owned());
        }
        let total = self.arguments.iter().map(String::len).fold(
            self.program
                .len()
                .saturating_add(self.cwd.as_ref().map_or(0, String::len)),
            usize::saturating_add,
        );
        if total > 64 * 1024 {
            return Err("Agent launch exceeds 64 KiB".to_owned());
        }
        for value in std::iter::once(&self.program)
            .chain(self.cwd.iter())
            .chain(self.arguments.iter())
        {
            if value.len() > 8192 || value.chars().any(char::is_control) {
                return Err(
                    "Agent launch values must be at most 8192 bytes without control characters"
                        .to_owned(),
                );
            }
        }
        Ok(())
    }

    /// Only reusable configuration options are retained. Prompts, credentials, arbitrary
    /// config overrides and session selectors must not enter hook state or be replayed.
    #[must_use]
    pub fn retained(&self, provider: AgentKind) -> Self {
        let valued: &[&str] = match provider {
            AgentKind::Pi => &["--provider", "--model", "--thinking", "--session-dir"],
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
        self.validate()?;
        let context =
            serde_json::to_string(&self.retained(provider)).map_err(|error| error.to_string())?;
        match shell {
            LaunchShell::Posix => {
                let argv = std::iter::once(&self.program)
                    .chain(&self.arguments)
                    .map(|value| quote_posix(value))
                    .collect::<Vec<_>>()
                    .join(" ");
                let launch = format!(
                    "exec env BOOTTY_AGENT_LAUNCH_CONTEXT={} {argv}",
                    quote_posix(&context)
                );
                Ok(self.cwd.as_ref().filter(|cwd| !cwd.is_empty()).map_or_else(
                    || launch.clone(),
                    |cwd| format!("cd {} && {launch}", quote_posix(cwd)),
                ))
            }
            LaunchShell::Windows => {
                let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
                let argv = std::iter::once(&self.program)
                    .chain(&self.arguments)
                    .map(|value| quote(value))
                    .collect::<Vec<_>>()
                    .join(" ");
                let cwd = self
                    .cwd
                    .as_ref()
                    .filter(|cwd| !cwd.is_empty())
                    .map(|cwd| format!("Set-Location -LiteralPath {}; ", quote(cwd)))
                    .unwrap_or_default();
                let script = format!(
                    "$ErrorActionPreference='Stop'; {cwd}$env:BOOTTY_AGENT_LAUNCH_CONTEXT={}; & {argv}; exit $LASTEXITCODE",
                    quote(&context)
                );
                let bytes = script
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>();
                let command = format!(
                    "powershell.exe -NoProfile -EncodedCommand {}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                );
                if command.len() > 30_000 {
                    return Err("Agent launch exceeds the Windows command-line limit".to_owned());
                }
                Ok(command)
            }
        }
    }
}

pub fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
