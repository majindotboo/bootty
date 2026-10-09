use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ConfigLoadError;

/// Launch preferences only. Providers retain ownership of credentials and conversation history.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct AgentProfileConfig {
    pub name: String,
    pub directory: Option<String>,
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct AgentProviderConfig {
    pub enabled: bool,
    pub program: String,
    pub default_model: String,
    pub default_effort: String,
    pub fast_mode: bool,
    /// Empty selects the provider's existing account store and default launch preferences.
    pub selected: String,
    pub profiles: BTreeMap<String, AgentProfileConfig>,
}

impl Default for AgentProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            program: String::new(),
            default_model: String::new(),
            default_effort: String::new(),
            fast_mode: false,
            selected: String::new(),
            profiles: BTreeMap::new(),
        }
    }
}

impl AgentProviderConfig {
    #[must_use]
    pub fn selected_profile(&self) -> Option<&AgentProfileConfig> {
        self.profiles.get(&self.selected)
    }

    pub(super) fn validate(&self) -> Result<(), ConfigLoadError> {
        if [&self.default_model, &self.default_effort]
            .into_iter()
            .any(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(ConfigLoadError::new(
                "Agent model and effort defaults must be bounded identifiers",
            ));
        }
        if self.profiles.len() > 16
            || (!self.selected.is_empty() && !self.profiles.contains_key(&self.selected))
        {
            return Err(ConfigLoadError::new(
                "Agent providers support sixteen profiles and require an existing selected profile",
            ));
        }
        if self.program.starts_with('-')
            || self.program.len() > 8192
            || self.program.chars().any(char::is_control)
        {
            return Err(ConfigLoadError::new(
                "Agent program must be an executable name or path",
            ));
        }
        for (id, profile) in &self.profiles {
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || profile.name.trim().is_empty()
                || profile.name.len() > 256
            {
                return Err(ConfigLoadError::new(
                    "Agent profiles need a simple stable id and a display name",
                ));
            }
            if profile.directory.as_ref().is_some_and(|directory| {
                directory.is_empty()
                    || directory.len() > 8192
                    || directory.chars().any(char::is_control)
                    || !std::path::Path::new(directory).is_absolute()
            }) {
                return Err(ConfigLoadError::new(
                    "Agent account directories must be absolute paths",
                ));
            }
            if profile.arguments.len() > 64
                || profile
                    .arguments
                    .iter()
                    .any(|argument| argument.len() > 8192 || argument.contains('\0'))
                || profile.arguments.iter().map(String::len).sum::<usize>() > 64 * 1024
            {
                return Err(ConfigLoadError::new(
                    "Agent profile arguments exceed the launch limits",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentProvidersConfig {
    #[serde(rename = "allow-spawn")]
    pub allow_spawn: bool,
    #[serde(rename = "default-provider")]
    pub default_provider: String,
    #[serde(rename = "quick-model")]
    pub quick_model: String,
    pub codex: AgentProviderConfig,
    pub claude: AgentProviderConfig,
    pub pi: AgentProviderConfig,
}

impl Default for AgentProvidersConfig {
    fn default() -> Self {
        Self {
            allow_spawn: false,
            default_provider: "codex".to_owned(),
            quick_model: "gpt-6-luna".to_owned(),
            codex: AgentProviderConfig::default(),
            claude: AgentProviderConfig::default(),
            pi: AgentProviderConfig::default(),
        }
    }
}

impl AgentProvidersConfig {
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&AgentProviderConfig> {
        match id {
            "codex" => Some(&self.codex),
            "claude" => Some(&self.claude),
            "pi" => Some(&self.pi),
            _ => None,
        }
    }

    pub(super) fn validate(&self) -> Result<(), ConfigLoadError> {
        if !matches!(self.default_provider.as_str(), "codex" | "claude" | "pi") {
            return Err(ConfigLoadError::new(
                "Default agent provider must be codex, claude or pi",
            ));
        }
        if self.pi.fast_mode {
            return Err(ConfigLoadError::new("Pi does not support fast mode"));
        }
        if self.quick_model.trim().is_empty()
            || self.quick_model.len() > 256
            || self.quick_model.chars().any(char::is_control)
        {
            return Err(ConfigLoadError::new(
                "Quick model must be a model identifier",
            ));
        }
        for provider in [&self.codex, &self.claude, &self.pi] {
            provider.validate()?;
        }
        Ok(())
    }
}
