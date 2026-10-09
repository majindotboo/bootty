use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AgentKind, NativeAgentSession, native_protocol::field};

/// Provider-advertised choices. IDs are wire selectors, never display labels.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeModelOption {
    pub id: String,
    pub display_name: String,
    pub reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: Option<String>,
    pub is_default: bool,
    #[serde(default)]
    pub is_legacy: bool,
    #[serde(default)]
    pub is_favorite: bool,
}

/// Durable settings for the next prompt; access permissions remain separately owned.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelSelection {
    pub model: String,
    pub reasoning_effort: Option<String>,
}

impl NativeAgentSession {
    /// Discover a captured launch's catalog without creating a conversation or submitting a turn.
    /// # Errors
    /// Returns invalid launch configuration, unsupported discovery, or provider errors.
    pub fn discover_models(
        mut config: crate::NativeSessionConfig,
    ) -> Result<Vec<NativeModelOption>, String> {
        // Catalog discovery submits no turns and creates no permission extension on either host.
        config.permissions = crate::NativePermissionMode::ProviderDefault;
        config.prepare_fresh_identity()?;
        let session = Self::start_with_clock(config, std::sync::Arc::new(std::time::Instant::now))?;
        match session.config.provider {
            AgentKind::Codex => session.initialize_codex_transport()?,
            AgentKind::Claude => session.initialize()?,
            AgentKind::Pi => {}
        }
        session.model_catalog(false)
    }

    /// Discover through the exact initialized provider process and captured account.
    /// # Errors
    /// Returns unsupported discovery, malformed catalogs, or provider transport errors.
    pub fn models(&self) -> Result<Vec<NativeModelOption>, String> {
        self.model_catalog(true)
    }

    fn model_catalog(&self, bound: bool) -> Result<Vec<NativeModelOption>, String> {
        match self.config.provider {
            AgentKind::Codex => {
                let mut models = Vec::new();
                let mut cursor = Value::Null;
                // Bound pagination even if a provider repeats its cursor.
                for _ in 0..16 {
                    let response = self.rpc("model/list", json!({"cursor":cursor,"limit":100}))?;
                    for model in field(&response, "data")
                        .as_array()
                        .ok_or("Provider returned no model catalog")?
                    {
                        if field(model, "hidden") == true {
                            continue;
                        }
                        models.push(NativeModelOption {
                            id: selector(field(model, "model"))?,
                            display_name: selector(field(model, "displayName"))?,
                            reasoning_efforts: field(model, "supportedReasoningEfforts")
                                .as_array()
                                .ok_or("Provider returned no reasoning capabilities")?
                                .iter()
                                .map(|effort| selector(field(effort, "reasoningEffort")))
                                .collect::<Result<_, _>>()?,
                            default_reasoning_effort: field(model, "defaultReasoningEffort")
                                .as_str()
                                .map(str::to_owned),
                            is_default: field(model, "isDefault") == true,
                            is_legacy: field(model, "isLegacy") == true
                                || known_legacy_model(
                                    field(model, "model").as_str().unwrap_or_default(),
                                ),
                            is_favorite: false,
                        });
                    }
                    if models.len() > 1024 {
                        return Err("Provider model catalog exceeds 1024 choices".to_owned());
                    }
                    cursor = field(&response, "nextCursor").clone();
                    if cursor.is_null() {
                        self.apply_codex_defaults(&mut models);
                        return Ok(models);
                    }
                    selector(&cursor)?;
                }
                Err("Provider model catalog exceeds 16 pages".to_owned())
            }
            AgentKind::Pi => {
                if bound {
                    self.verify_pi_identity()?;
                }
                let response = self.rpc("get_available_models", json!({}))?;
                let state = self.rpc("get_state", json!({}))?;
                let current = field(&state, "model");
                let models = field(&response, "models")
                    .as_array()
                    .filter(|models| models.len() <= 1024)
                    .ok_or("Provider returned an invalid model catalog")?;
                models
                    .iter()
                    .map(|model| {
                        let current = field(model, "id") == field(current, "id")
                            && field(model, "provider") == field(current, "provider");
                        Ok(NativeModelOption {
                            id: format!(
                                "{}/{}",
                                selector(field(model, "provider"))?,
                                selector(field(model, "id"))?
                            ),
                            display_name: selector(field(model, "name"))?,
                            reasoning_efforts: pi_reasoning_efforts(model),
                            default_reasoning_effort: current
                                .then(|| field(&state, "thinkingLevel").as_str().map(str::to_owned))
                                .flatten(),
                            is_default: current,
                            is_legacy: field(model, "isLegacy") == true
                                || known_legacy_model(
                                    field(model, "id").as_str().unwrap_or_default(),
                                ),
                            is_favorite: false,
                        })
                    })
                    .collect()
            }
            AgentKind::Claude => {
                let mut options = super::native_session::lock(&self.claude_models)
                    .clone()
                    .ok_or("Claude returned no model catalog")?;
                if bound {
                    // Flag settings cannot change live effort to max. Keep CLI-only max
                    // in launch discovery; expose it live when Claude adds that control.
                    for model in &mut options {
                        model.reasoning_efforts.retain(|effort| effort != "max");
                    }
                }
                Ok(options)
            }
        }
    }

    fn apply_codex_defaults(&self, models: &mut [NativeModelOption]) {
        // Resolve the configured account and project defaults rather than the recommendation.
        let Ok(response) = self.rpc(
            "config/read",
            json!({"cwd":self.config.cwd,"includeLayers":false}),
        ) else {
            return;
        };
        let config = field(&response, "config");
        let configured = field(config, "model")
            .as_str()
            .filter(|model| models.iter().any(|option| option.id == *model));
        for option in models {
            if let Some(model) = configured {
                option.is_default = option.id == model;
            }
            if option.is_default
                && let Some(effort) = field(config, "model_reasoning_effort").as_str()
                && option
                    .reasoning_efforts
                    .iter()
                    .any(|supported| supported == effort)
            {
                option.default_reasoning_effort = Some(effort.to_owned());
            }
        }
    }

    pub(crate) fn apply_claude_selection(
        &self,
        selection: &NativeModelSelection,
    ) -> Result<(), String> {
        if selection.reasoning_effort.as_deref() == Some("max") {
            if self.config.model.as_ref() != Some(&selection.model)
                || self.config.reasoning_effort.as_deref() != Some("max")
            {
                return Err(
                    "Claude max effort must be selected when starting the session".to_owned(),
                );
            }
        } else if let Some(effort) = &selection.reasoning_effort {
            self.rpc(
                "apply_flag_settings",
                json!({"settings":{"effortLevel":selection.reasoning_effort}}),
            )?;
            let settings = self.rpc("get_settings", json!({}))?;
            if field(field(&settings, "effective"), "effortLevel").as_str() != Some(effort.as_str())
            {
                return Err("Claude did not accept the selected reasoning effort".to_owned());
            }
        }
        self.rpc("set_model", json!({"model":selection.model}))?;
        Ok(())
    }

    pub(crate) fn apply_pi_selection(
        &self,
        selection: &NativeModelSelection,
    ) -> Result<(), String> {
        self.verify_pi_identity()?;
        let (provider, model) = selection
            .model
            .split_once('/')
            .ok_or("Pi model selection requires provider/model identity")?;
        let response = self.rpc("set_model", json!({"provider":provider,"modelId":model}))?;
        if field(&response, "id") != model || field(&response, "provider") != provider {
            return Err("Pi acknowledged a different model".to_owned());
        }
        if let Some(effort) = &selection.reasoning_effort {
            self.rpc("set_thinking_level", json!({"level":effort}))?;
            let state = self.rpc("get_state", json!({}))?;
            if field(&state, "thinkingLevel") != effort.as_str() {
                return Err("Pi did not accept the selected thinking level".to_owned());
            }
        }
        self.verify_pi_identity()
    }
}

// Known legacy model families. Unknown/custom models
// stay visible; replace this fallback when transports advertise lifecycle metadata.
fn known_legacy_model(id: &str) -> bool {
    matches!(
        id,
        "gpt-6-sol"
            | "gpt-5.5"
            | "claude-fable-5"
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-opus-4-5"
            | "claude-sonnet-4-6"
            | "claude-haiku-4-5"
    ) || id.starts_with("gpt-5.6-")
}

fn selector(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= 8192 && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| "Provider returned an invalid model selector".to_owned())
}

fn pi_reasoning_efforts(model: &Value) -> Vec<String> {
    if field(model, "reasoning") != true {
        return vec!["off".to_owned()];
    }
    // Pi's protocol levels; xhigh/max require explicit provider model mappings.
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .filter(|level| {
            let mapped = field(model, "thinkingLevelMap").get(*level);
            mapped != Some(&Value::Null) && (!matches!(*level, "xhigh" | "max") || mapped.is_some())
        })
        .map(str::to_owned)
        .collect()
}

/// Decode only choices and capabilities advertised by the owned Claude handshake.
pub fn decode_claude(response: &Value) -> Result<Vec<NativeModelOption>, String> {
    let models = field(response, "models")
        .as_array()
        .filter(|models| models.len() <= 1024)
        .ok_or("Claude returned an invalid model catalog")?;
    let default = models
        .iter()
        .find(|model| field(model, "value") == "default");
    let resolved_default = default.and_then(|model| field(model, "resolvedModel").as_str());
    let default_alias = resolved_default.and_then(|resolved| {
        models.iter().find(|model| {
            field(model, "value") != "default" && field(model, "resolvedModel") == resolved
        })
    });
    models
        .iter()
        .filter(|model| field(model, "value") != "default" || default_alias.is_none())
        .map(|model| {
            let id = selector(field(model, "value"))?;
            let efforts = if field(model, "supportsEffort") == true {
                field(model, "supportedEffortLevels")
                    .as_array()
                    .filter(|levels| levels.len() <= 16)
                    .ok_or("Claude returned invalid reasoning capabilities")?
                    .iter()
                    .map(selector)
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            Ok(NativeModelOption {
                is_default: default_alias.map_or(id == "default", |alias| {
                    field(alias, "value").as_str() == Some(id.as_str())
                }),
                is_legacy: known_legacy_model(
                    field(model, "resolvedModel").as_str().unwrap_or(&id),
                ),
                display_name: field(model, "description")
                    .as_str()
                    .filter(|_| id != "default")
                    .and_then(|description| description.split('·').next())
                    .map(str::trim)
                    .filter(|name| {
                        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
                    })
                    .map_or_else(
                        || {
                            if id == "default" {
                                selector(field(model, "resolvedModel"))
                            } else {
                                selector(field(model, "displayName"))
                            }
                        },
                        |name| Ok(name.to_owned()),
                    )?,
                id,
                reasoning_efforts: efforts,
                default_reasoning_effort: None,
                is_favorite: false,
            })
        })
        .collect()
}

pub fn apply_claude_effort(options: &mut [NativeModelOption], catalog: &Value, settings: &Value) {
    let effective = field(settings, "effective");
    for option in options {
        let resolved = field(catalog, "models")
            .as_array()
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| field(model, "value").as_str() == Some(option.id.as_str()))
            })
            .and_then(|model| field(model, "resolvedModel").as_str());
        let effort = resolved
            .and_then(|model| {
                field(
                    field(field(effective, "modelSettings"), model),
                    "effortLevel",
                )
                .as_str()
            })
            .or_else(|| field(effective, "effortLevel").as_str());
        if let Some(effort) = effort.filter(|effort| {
            option
                .reasoning_efforts
                .iter()
                .any(|supported| supported == *effort)
        }) {
            option.default_reasoning_effort = Some(effort.to_owned());
        }
    }
}
