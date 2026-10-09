//! Provider-advertised composer commands and skills.

/* Provider completion behavior adapted from T3 Code.
MIT License

Copyright (c) 2026 T3 Tools Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AgentKind, NativeAgentSession, NativeSessionConfig, native_protocol::field,
    native_session::lock,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeCompletionKind {
    Command,
    Skill,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeCompletionOption {
    pub kind: NativeCompletionKind,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub path: Option<String>,
    pub scope: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeCompletionCatalog {
    pub options: Vec<NativeCompletionOption>,
}

impl NativeAgentSession {
    /// Probe one captured launch without creating a thread or submitting a prompt.
    /// # Errors
    /// Returns invalid launch, malformed capabilities or provider transport failures.
    pub fn discover_completions(
        mut config: NativeSessionConfig,
    ) -> Result<NativeCompletionCatalog, String> {
        if config.remote.is_none() {
            config.cwd = std::fs::canonicalize(&config.cwd).map_err(|e| e.to_string())?;
        }
        config.permissions = crate::NativePermissionMode::ProviderDefault;
        config.prepare_fresh_identity()?;
        let session = Self::start_with_clock(config, std::sync::Arc::new(std::time::Instant::now))?;
        match session.config.provider {
            AgentKind::Codex => session.initialize_codex_transport()?,
            AgentKind::Claude => session.initialize()?,
            AgentKind::Pi => {}
        }
        session.completion_catalog(false)
    }

    /// Read capabilities through this conversation's exact provider process and account.
    /// # Errors
    /// Returns malformed capability data, stale Pi identity or transport failures.
    pub fn completions(&self) -> Result<NativeCompletionCatalog, String> {
        self.completion_catalog(true)
    }

    fn completion_catalog(&self, bound: bool) -> Result<NativeCompletionCatalog, String> {
        let cached = lock(&self.completion_catalog).clone();
        if let Some(catalog) = cached {
            return Ok(catalog);
        }
        let catalog = match self.config.provider {
            AgentKind::Pi => {
                if bound {
                    self.verify_pi_identity()?;
                }
                let response = self.rpc("get_commands", json!({}))?;
                decode_pi(&response)?
            }
            AgentKind::Codex => {
                let cwd = if self.config.remote.is_some() {
                    self.config.cwd.clone()
                } else {
                    std::fs::canonicalize(&self.config.cwd).map_err(|e| e.to_string())?
                };
                let response = self.rpc("skills/list", json!({"cwds":[cwd]}))?;
                decode_codex(&response, &cwd.to_string_lossy())?
            }
            AgentKind::Claude => return Err("Claude did not advertise its command catalog".into()),
        };
        *lock(&self.completion_catalog) = Some(catalog.clone());
        Ok(catalog)
    }
}

pub fn decode_claude(value: &Value) -> Result<NativeCompletionCatalog, String> {
    let commands = field(value, "commands")
        .as_array()
        .ok_or("Claude returned no command catalog")?;
    bounded(commands)?;
    let mut options = Vec::new();
    for command in commands {
        if let Some(name) = name(field(command, "name")) {
            options.push(NativeCompletionOption {
                kind: NativeCompletionKind::Command,
                display_name: name.clone(),
                name,
                description: text(field(command, "description")),
                argument_hint: text(field(command, "argumentHint")),
                path: None,
                scope: None,
            });
        }
    }
    Ok(NativeCompletionCatalog { options })
}

fn decode_pi(value: &Value) -> Result<NativeCompletionCatalog, String> {
    let commands = field(value, "commands")
        .as_array()
        .ok_or("Pi returned no command catalog")?;
    bounded(commands)?;
    let mut options = vec![NativeCompletionOption {
        kind: NativeCompletionKind::Command,
        name: "compact".into(),
        display_name: "compact".into(),
        description: Some("Summarize the conversation and reduce context usage".into()),
        argument_hint: Some("Optional instructions".into()),
        path: None,
        scope: None,
    }];
    for command in commands {
        let Some(command_name) = name(field(command, "name")) else {
            continue;
        };
        if matches!(
            command_name.as_str(),
            "compact" | crate::tool_bridge::PI_CHECKPOINT_COMMAND
        ) {
            continue;
        }
        let skill = field(command, "source") == "skill";
        let name = if skill {
            command_name
                .strip_prefix("skill:")
                .unwrap_or(&command_name)
                .into()
        } else {
            command_name
        };
        let source = field(command, "sourceInfo");
        let interface = field(command, "interface");
        options.push(NativeCompletionOption {
            kind: if skill {
                NativeCompletionKind::Skill
            } else {
                NativeCompletionKind::Command
            },
            display_name: text(field(command, "displayName"))
                .or_else(|| text(field(source, "displayName")))
                .or_else(|| text(field(interface, "displayName")))
                .unwrap_or_else(|| name.clone()),
            description: text(field(command, "shortDescription"))
                .or_else(|| text(field(command, "description")))
                .or_else(|| text(field(interface, "shortDescription"))),
            argument_hint: None,
            path: if skill {
                text(field(source, "path")).or_else(|| text(field(command, "path")))
            } else {
                None
            },
            scope: text(field(source, "scope")).or_else(|| text(field(command, "location"))),
            name,
        });
    }
    Ok(NativeCompletionCatalog { options })
}

fn decode_codex(value: &Value, cwd: &str) -> Result<NativeCompletionCatalog, String> {
    let entries = field(value, "data")
        .as_array()
        .ok_or("Codex returned no skill catalog")?;
    bounded(entries)?;
    let mut options = vec![NativeCompletionOption {
        kind: NativeCompletionKind::Command,
        name: "compact".into(),
        display_name: "compact".into(),
        description: Some("Compact this conversation".into()),
        argument_hint: None,
        path: None,
        scope: None,
    }];
    for entry in entries.iter().filter(|entry| field(entry, "cwd") == cwd) {
        let skills = field(entry, "skills")
            .as_array()
            .ok_or("Codex returned malformed skills")?;
        bounded(skills)?;
        for skill in skills {
            if field(skill, "enabled") != true {
                continue;
            }
            let Some(name) = name(field(skill, "name")) else {
                continue;
            };
            let interface = field(skill, "interface");
            options.push(NativeCompletionOption {
                kind: NativeCompletionKind::Skill,
                display_name: text(field(interface, "displayName")).unwrap_or_else(|| name.clone()),
                description: text(field(skill, "shortDescription"))
                    .or_else(|| text(field(interface, "shortDescription")))
                    .or_else(|| text(field(skill, "description"))),
                argument_hint: None,
                path: text(field(skill, "path")),
                scope: text(field(skill, "scope")),
                name,
            });
        }
    }
    bounded(&options)?;
    Ok(NativeCompletionCatalog { options })
}

fn bounded<T>(items: &[T]) -> Result<(), String> {
    if items.len() > 2048 {
        Err("Provider completion catalog exceeds 2048 choices".into())
    } else {
        Ok(())
    }
}
fn name(value: &Value) -> Option<String> {
    let name = text(value)?;
    (!name.chars().any(char::is_whitespace) && !name.starts_with(['/', '$', '@'])).then_some(name)
}
fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| {
            !text.trim().is_empty() && text.len() <= 8192 && !text.chars().any(char::is_control)
        })
        .map(str::to_owned)
}
