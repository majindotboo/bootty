use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AgentKind, NativeAgentSession, NativePermissionMode, native_protocol::field};

/// Provider-advertised choices. IDs are wire selectors, never display labels.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Default, legacy, favorite and speed capability vary independently"
)]
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
    /// Advertised by this model's provider catalog; absence does not imply support.
    #[serde(default)]
    pub supports_fast_mode: bool,
    /// What the provider says fast mode costs or gives, such as "2x speed, increased usage".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode_detail: Option<String>,
}

/// Catalog and effective permissions from the captured provider account and project.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeProviderCatalog {
    pub models: Vec<NativeModelOption>,
    /// Unrecognized policies remain inherited rather than being approximated.
    pub permissions: Option<NativePermissionMode>,
}

/// Durable settings for the next prompt; access permissions remain separately owned.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeModelSelection {
    /// Omission preserves the current collaboration mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
    /// Omission preserves speed for existing callers; explicit false selects normal speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    pub model: String,
    pub reasoning_effort: Option<String>,
}

fn codex_fast_mode(model: &Value) -> bool {
    field(model, "serviceTiers")
        .as_array()
        .is_some_and(|tiers| tiers.iter().any(|tier| field(tier, "id") == "fast"))
        || field(model, "additionalSpeedTiers")
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|tier| tier == "fast"))
}

/// Codex's description of the fast service tier it advertises for a model.
fn codex_fast_mode_detail(model: &Value) -> Option<String> {
    field(model, "serviceTiers")
        .as_array()?
        .iter()
        .find(|tier| field(tier, "id") == "fast" || field(tier, "name") == "Fast")
        .and_then(|tier| field(tier, "description").as_str())
        .map(str::trim)
        .filter(|detail| {
            !detail.is_empty() && detail.len() <= 128 && !detail.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

impl NativeAgentSession {
    /// Discover a captured launch's catalog without creating a conversation or submitting a turn.
    /// # Errors
    /// Returns invalid launch configuration, unsupported discovery, or provider errors.
    pub fn discover_models(
        config: crate::NativeSessionConfig,
    ) -> Result<Vec<NativeModelOption>, String> {
        Self::start_discovery(config)?.model_catalog(false)
    }

    /// Read model choices and inherited permissions without starting a conversation.
    /// # Errors
    /// Returns invalid launch configuration, unsupported discovery, or provider errors.
    pub fn discover_catalog(
        config: crate::NativeSessionConfig,
    ) -> Result<NativeProviderCatalog, String> {
        let session = Self::start_discovery(config)?;
        Ok(NativeProviderCatalog {
            models: session.model_catalog(false)?,
            permissions: session.configured_permissions(),
        })
    }

    fn start_discovery(mut config: crate::NativeSessionConfig) -> Result<Self, String> {
        // Discovery inherits the selected account and submits no turns or permission extensions.
        config.permissions = NativePermissionMode::ProviderDefault;
        config.prepare_fresh_identity()?;
        let session = Self::start_with_clock(config, std::sync::Arc::new(std::time::Instant::now))?;
        match session.config.provider {
            AgentKind::Codex => session.initialize_codex_transport()?,
            AgentKind::Claude => session.initialize()?,
            AgentKind::Pi => {}
        }
        Ok(session)
    }

    fn configured_permissions(&self) -> Option<NativePermissionMode> {
        match self.config.provider {
            AgentKind::Codex => {
                let response = self
                    .rpc(
                        "config/read",
                        json!({"cwd":self.config.cwd,"includeLayers":false}),
                    )
                    .ok()?;
                let config = field(&response, "config");
                let approval = field(config, "approval_policy").as_str()?;
                let sandbox = field(config, "sandbox_mode").as_str()?;
                let reviewer = field(config, "approvals_reviewer")
                    .as_str()
                    .unwrap_or("user");
                match (approval, sandbox, reviewer) {
                    ("never", "danger-full-access", _) => Some(NativePermissionMode::FullAccess),
                    ("untrusted", "read-only", _) => Some(NativePermissionMode::Supervised),
                    ("on-request", "workspace-write", "user") => {
                        Some(NativePermissionMode::AutoAcceptEdits)
                    }
                    ("on-request", "workspace-write", "auto_review") => {
                        Some(NativePermissionMode::Auto)
                    }
                    _ => None,
                }
            }
            AgentKind::Claude => {
                let response = self.rpc("get_settings", json!({})).ok()?;
                let permissions = field(field(&response, "effective"), "permissions");
                match field(permissions, "defaultMode").as_str()? {
                    "default" => Some(NativePermissionMode::Supervised),
                    "acceptEdits" => Some(NativePermissionMode::AutoAcceptEdits),
                    "auto" => Some(NativePermissionMode::Auto),
                    "bypassPermissions" => Some(NativePermissionMode::FullAccess),
                    _ => None,
                }
            }
            // Pi does not advertise an approval policy in its native catalog protocol.
            AgentKind::Pi => None,
        }
    }

    /// Discover through the exact initialized provider process and captured account.
    /// # Errors
    /// Returns unsupported discovery, malformed catalogs, or provider transport errors.
    pub fn models(&self) -> Result<Vec<NativeModelOption>, String> {
        self.model_catalog(true)
    }

    /// Read choices and the effective policy from this session's exact provider process.
    /// # Errors
    /// Returns provider catalog or transport errors.
    pub fn catalog(&self) -> Result<NativeProviderCatalog, String> {
        Ok(NativeProviderCatalog {
            models: self.models()?,
            permissions: if self.config.provider == AgentKind::Pi {
                None
            } else if self.config.permissions == NativePermissionMode::ProviderDefault {
                self.configured_permissions()
            } else {
                Some(self.config.permissions)
            },
        })
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
                            supports_fast_mode: codex_fast_mode(model),
                            fast_mode_detail: codex_fast_mode_detail(model),
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
                            supports_fast_mode: false,
                            fast_mode_detail: None,
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
        if let Some(fast) = selection.fast_mode {
            self.rpc("apply_flag_settings", json!({"settings":{"fastMode":fast}}))?;
            let settings = self.rpc("get_settings", json!({}))?;
            if field(field(&settings, "effective"), "fastMode").as_bool() != Some(fast) {
                return Err("Claude did not accept the selected speed".to_owned());
            }
        }
        self.rpc("set_model", json!({"model":selection.model}))?;
        if let Some(plan) = selection.plan_mode {
            self.apply_claude_plan(plan)?;
        }
        Ok(())
    }

    /// Plan mode is a Claude permission mode. Leaving it returns to the conversation's own
    /// policy. Claude is only told when the mode changes, so a mode it chose stays.
    fn apply_claude_plan(&self, plan: bool) -> Result<(), String> {
        if *super::native_session::lock(&self.claude_plan) == plan {
            return Ok(());
        }
        let mode = if plan {
            "plan"
        } else {
            self.config.permissions.claude().unwrap_or("default")
        };
        self.rpc("set_permission_mode", json!({"mode": mode}))?;
        *super::native_session::lock(&self.claude_plan) = plan;
        Ok(())
    }

    /// Claude left Plan mode itself, because its plan was approved.
    pub(crate) fn left_claude_plan(&self) {
        *super::native_session::lock(&self.claude_plan) = false;
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
        if let Some(plan) = selection.plan_mode {
            self.apply_pi_plan(plan)?;
        }
        self.verify_pi_identity()
    }

    /// Plan mode lives in Bootty's Pi extension, which starts with it off. Pi is only told when
    /// the mode changes.
    fn apply_pi_plan(&self, plan: bool) -> Result<(), String> {
        if *super::native_session::lock(&self.pi_plan) == plan {
            return Ok(());
        }
        if !self.has_pi_command(crate::tool_bridge::PI_PLAN_COMMAND)? {
            return Err("This Pi conversation can't plan without Bootty's tools".to_owned());
        }
        let switch = if plan { "on" } else { "off" };
        let response = self.rpc(
            "prompt",
            json!({"message":format!("/{} {switch}", crate::tool_bridge::PI_PLAN_COMMAND)}),
        )?;
        // Older Pi RPC acknowledges a command without a disposition.
        if field(&response, "disposition") != "handled"
            && !response.as_object().is_some_and(serde_json::Map::is_empty)
        {
            return Err("Pi did not switch plan mode".to_owned());
        }
        *super::native_session::lock(&self.pi_plan) = plan;
        Ok(())
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
            let display_name = selector(field(model, "displayName"))?;
            let resolved = match model.get("resolvedModel") {
                None | Some(Value::Null) => None,
                Some(value) => Some(selector(value)?),
            };
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
                    .and_then(|description| description.split_once('·').map(|(name, _)| name))
                    .map(str::trim)
                    .filter(|name| {
                        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
                    })
                    .map_or_else(
                        || {
                            if id == "default" {
                                resolved.unwrap_or(display_name)
                            } else {
                                display_name
                            }
                        },
                        str::to_owned,
                    ),
                id,
                reasoning_efforts: efforts,
                default_reasoning_effort: None,
                is_favorite: false,
                supports_fast_mode: field(model, "supportsFastMode") == true,
                fast_mode_detail: None,
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
