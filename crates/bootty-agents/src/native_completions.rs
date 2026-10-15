//! Provider-advertised composer commands and skills.

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
    App,
    Plugin,
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
    #[serde(default)]
    pub icon_url: Option<String>,
}

/// An explicit app or plugin chip in authored input, resolved against the captured account.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeProviderMention {
    pub option: NativeCompletionOption,
    pub prompt_range: std::ops::Range<usize>,
}

impl NativeProviderMention {
    #[must_use]
    pub fn id(&self) -> String {
        format!(
            "provider:{}",
            self.option.path.as_deref().unwrap_or_default()
        )
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeCompletionCatalog {
    pub options: Vec<NativeCompletionOption>,
    #[serde(default)]
    pub plan_mode: bool,
    #[serde(default)]
    pub goals: bool,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub unavailable: Vec<NativeCompletionKind>,
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
        if self.config.provider == AgentKind::Codex {
            *lock(&self.completion_catalog) = None;
        }
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
                let mut catalog = decode_codex(&response, &cwd.to_string_lossy())?;
                let thread = bound.then(|| self.snapshot().session_id).flatten();
                self.append_installed_apps(&mut catalog, thread.as_deref());
                match self.rpc("plugin/installed", json!({"cwds":[cwd]})) {
                    Ok(response) => match decode_codex_plugins(&response) {
                        Ok(options) => catalog.options.extend(options),
                        Err(_) => catalog.unavailable.push(NativeCompletionKind::Plugin),
                    },
                    Err(error) if unsupported_catalog(&error) => {}
                    Err(_) => catalog.unavailable.push(NativeCompletionKind::Plugin),
                }
                catalog.plan_mode = self
                    .rpc("collaborationMode/list", json!({}))
                    .ok()
                    .and_then(|response| field(&response, "data").as_array().cloned())
                    .is_some_and(|modes| modes.iter().any(|mode| field(mode, "mode") == "plan"));
                catalog.goals = self
                    .rpc("experimentalFeature/list", json!({}))
                    .ok()
                    .and_then(|response| field(&response, "data").as_array().cloned())
                    .is_some_and(|features| {
                        features.iter().any(|feature| {
                            field(feature, "name") == "goals" && field(feature, "enabled") == true
                        })
                    });
                if catalog.goals
                    && let Some(thread) = &thread
                {
                    catalog.goal = self
                        .rpc("thread/goal/get", json!({"threadId":thread}))
                        .ok()
                        .and_then(|response| text(field(field(&response, "goal"), "objective")));
                }
                bounded(&catalog.options)?;
                catalog
            }
            AgentKind::Claude => return Err("Claude did not advertise its command catalog".into()),
        };
        *lock(&self.completion_catalog) = Some(catalog.clone());
        Ok(catalog)
    }

    /// Set or clear a goal through the public provider protocol with user provenance.
    /// # Errors
    /// Returns unsupported accounts, invalid objectives, stopped sessions, or provider rejection.
    pub fn set_goal(&self, objective: &str) -> Result<(), String> {
        if self.config.provider != AgentKind::Codex || !self.completions()?.goals {
            return Err("Goals are not available for this provider account".into());
        }
        if objective.len() > 8192 || objective.contains('\0') {
            return Err("Goal must be at most 8192 bytes".into());
        }
        let thread = self
            .snapshot()
            .session_id
            .ok_or("This conversation has no provider thread")?;
        let objective = objective.trim();
        let result = if objective.is_empty() {
            self.rpc(
                "thread/goal/clear",
                json!({"threadId":thread,"origin":"user"}),
            )
        } else {
            self.rpc(
                "thread/goal/set",
                json!({"threadId":thread,"origin":"user","objective":objective}),
            )
        }
        .map_err(|_| "Couldn’t update this conversation’s goal. Try again.".to_owned())?;
        if objective.is_empty() {
            if !field(&result, "cleared").is_boolean() {
                return Err("The provider did not clear the goal".into());
            }
        } else if field(field(&result, "goal"), "objective") != objective {
            return Err("The provider did not confirm the requested goal".into());
        }
        *lock(&self.completion_catalog) = None;
        Ok(())
    }

    fn append_installed_apps(&self, catalog: &mut NativeCompletionCatalog, thread: Option<&str>) {
        // Match Codex's installed, callable snapshot; the list fallback supports older servers.
        let response = match self.rpc(
            "app/installed",
            json!({"threadId":thread,"forceRefresh":true}),
        ) {
            Ok(response) => response,
            Err(error) if unsupported_catalog(&error) => {
                self.append_codex_apps(catalog, thread);
                return;
            }
            Err(_) => {
                catalog.unavailable.push(NativeCompletionKind::App);
                return;
            }
        };
        let Some(apps) = field(&response, "apps")
            .as_array()
            .filter(|apps| bounded(apps).is_ok())
        else {
            catalog.unavailable.push(NativeCompletionKind::App);
            return;
        };
        let mut options: Vec<_> = apps
            .iter()
            .filter(|app| field(app, "enabled") == true && field(app, "callable") == true)
            .filter_map(|app| {
                let id = name(field(app, "id"))?;
                Some(NativeCompletionOption {
                    kind: NativeCompletionKind::App,
                    display_name: text(field(app, "runtimeName")).unwrap_or_else(|| id.clone()),
                    name: id.clone(),
                    path: Some(format!("app://{id}")),
                    description: None,
                    argument_hint: None,
                    scope: None,
                    icon_url: None,
                })
            })
            .collect();
        for batch in options.chunks_mut(100) {
            let ids: Vec<_> = batch.iter().map(|option| &option.name).collect();
            let Ok(metadata) = self.rpc(
                "app/read",
                json!({"appIds":ids,"threadId":thread,"includeTools":false}),
            ) else {
                continue;
            };
            let Some(apps) = field(&metadata, "apps")
                .as_array()
                .filter(|apps| bounded(apps).is_ok())
            else {
                continue;
            };
            for option in batch {
                if let Some(app) = apps
                    .iter()
                    .find(|app| field(app, "id") == option.name.as_str())
                {
                    if let Some(label) = text(field(app, "name")) {
                        option.display_name = label;
                    }
                    option.description = text(field(app, "description"));
                    option.icon_url = icon_url(field(app, "iconUrl"));
                }
            }
        }
        catalog.options.extend(options);
    }

    fn append_codex_apps(&self, catalog: &mut NativeCompletionCatalog, thread: Option<&str>) {
        let mut cursor = None::<String>;
        let mut seen = std::collections::HashSet::new();
        // Bound catalog pagination; a larger provider catalog needs a server-side search UI.
        for _ in 0..32 {
            let response = match self.rpc(
                "app/list",
                json!({"threadId":thread,"cursor":cursor,"limit":100}),
            ) {
                Ok(response) => response,
                Err(error) if unsupported_catalog(&error) => return,
                Err(_) => {
                    catalog.unavailable.push(NativeCompletionKind::App);
                    return;
                }
            };
            if let Ok(options) = decode_codex_apps(&response) {
                catalog.options.extend(options);
            } else {
                catalog.unavailable.push(NativeCompletionKind::App);
                return;
            }
            cursor = text(field(&response, "nextCursor"));
            let Some(next) = &cursor else { return };
            if !seen.insert(next.clone()) {
                break;
            }
        }
        catalog.unavailable.push(NativeCompletionKind::App);
    }
}

fn icon_url(value: &Value) -> Option<String> {
    text(value).filter(|url| url.starts_with("https://"))
}

fn unsupported_catalog(error: &str) -> bool {
    error.contains("-32601")
        || error.to_ascii_lowercase().contains("unknown method")
        || error.to_ascii_lowercase().contains("unknown variant")
}

fn decode_codex_apps(value: &Value) -> Result<Vec<NativeCompletionOption>, String> {
    let apps = field(value, "data")
        .as_array()
        .ok_or("Malformed app catalog")?;
    bounded(apps)?;
    Ok(apps
        .iter()
        .filter(|app| field(app, "isAccessible") == true && field(app, "isEnabled") != false)
        .filter_map(|app| {
            let id = name(field(app, "id"))?;
            let label = text(field(app, "name"))?;
            Some(NativeCompletionOption {
                icon_url: icon_url(field(app, "logoUrl")),
                kind: NativeCompletionKind::App,
                name: id.clone(),
                display_name: label,
                description: text(field(app, "description")),
                argument_hint: None,
                path: Some(format!("app://{id}")),
                scope: None,
            })
        })
        .collect())
}

fn decode_codex_plugins(value: &Value) -> Result<Vec<NativeCompletionOption>, String> {
    let marketplaces = field(value, "marketplaces")
        .as_array()
        .ok_or("Malformed plugin catalog")?;
    bounded(marketplaces)?;
    let mut options = Vec::new();
    for marketplace in marketplaces {
        let plugins = field(marketplace, "plugins")
            .as_array()
            .ok_or("Malformed plugin catalog")?;
        bounded(plugins)?;
        for plugin in plugins.iter().filter(|plugin| {
            field(plugin, "installed") == true
                && field(plugin, "enabled") == true
                && field(plugin, "availability") != "DISABLED_BY_ADMIN"
                && field(plugin, "disabledReason").is_null()
        }) {
            let Some(id) = name(field(plugin, "id")) else {
                continue;
            };
            let Some(label) = text(field(plugin, "name")) else {
                continue;
            };
            let interface = field(plugin, "interface");
            options.push(NativeCompletionOption {
                icon_url: icon_url(field(interface, "composerIconUrl")),
                kind: NativeCompletionKind::Plugin,
                name: id.clone(),
                display_name: text(field(interface, "displayName")).unwrap_or(label),
                description: text(field(interface, "shortDescription")),
                argument_hint: None,
                path: Some(format!("plugin://{id}")),
                scope: text(field(marketplace, "name")),
            });
        }
    }
    bounded(&options)?;
    Ok(options)
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
                icon_url: None,
                // Claude marks its own commands. The rest come from skills, plugins and custom
                // commands, which Claude runs the same way: as a skill.
                kind: if field(command, "builtin") == true {
                    NativeCompletionKind::Command
                } else {
                    NativeCompletionKind::Skill
                },
                display_name: name.clone(),
                name,
                description: text(field(command, "description")),
                argument_hint: text(field(command, "argumentHint")),
                path: None,
                scope: None,
            });
        }
    }
    Ok(NativeCompletionCatalog {
        options,
        // Plan is one of Claude's own permission modes.
        plan_mode: true,
        ..NativeCompletionCatalog::default()
    })
}

fn decode_pi(value: &Value) -> Result<NativeCompletionCatalog, String> {
    let commands = field(value, "commands")
        .as_array()
        .ok_or("Pi returned no command catalog")?;
    bounded(commands)?;
    let mut options = vec![NativeCompletionOption {
        icon_url: None,
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
            icon_url: None,
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
    Ok(NativeCompletionCatalog {
        options,
        ..NativeCompletionCatalog::default()
    })
}

fn decode_codex(value: &Value, cwd: &str) -> Result<NativeCompletionCatalog, String> {
    let entries = field(value, "data")
        .as_array()
        .ok_or("Codex returned no skill catalog")?;
    bounded(entries)?;
    let mut options = vec![NativeCompletionOption {
        icon_url: None,
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
                icon_url: None,
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
    Ok(NativeCompletionCatalog {
        options,
        ..NativeCompletionCatalog::default()
    })
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
