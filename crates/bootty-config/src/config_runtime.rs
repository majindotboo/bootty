use std::{sync::Arc, time::Instant};

use thiserror::Error;

use crate::{
    config::{
        BoottyConfig, ConfigDocument, ConfigResult, ConfigWriteOutcome, commit_config_document,
        load_or_create_config_document,
    },
    config_reload::ConfigHotReload,
    settings_schema::SettingsSchema,
};

/// A validated transition between two accepted product configurations.
pub struct ConfigChange {
    previous: BoottyConfig,
    current: BoottyConfig,
}

impl ConfigChange {
    /// The accepted configuration before this transition.
    #[must_use]
    pub const fn previous(&self) -> &BoottyConfig {
        &self.previous
    }

    /// The accepted configuration after this transition.
    #[must_use]
    pub const fn current(&self) -> &BoottyConfig {
        &self.current
    }
}

/// Failure while loading or validating a candidate configuration during reload.
#[derive(Debug, Error)]
pub enum ConfigRuntimeError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigLoadError),
    #[error("{0}")]
    Validation(String),
}

/// Owns the accepted configuration, its editable document, fixed schema, revision, and reload
/// state.
///
/// Product-specific acceptance is supplied by the caller so this owner stays independent of UI,
/// input-host, mux, and control crates. The callback runs before a reloaded configuration is
/// published and before a committed document replaces the file.
pub struct ConfigRuntime {
    current: BoottyConfig,
    configured_font_size: f32,
    document: ConfigDocument,
    hot_reload: ConfigHotReload,
    revision: u64,
    schema: Arc<SettingsSchema>,
}

impl ConfigRuntime {
    ///
    /// # Errors
    /// Returns an error if the editable config document cannot be read or parsed.
    pub fn new(config: BoottyConfig) -> ConfigResult<Self> {
        let document = load_or_create_config_document(&config.config_path)?;
        let hot_reload = ConfigHotReload::new(&config.config_path);
        Ok(Self {
            configured_font_size: config.font.size,
            current: config,
            document,
            hot_reload,
            revision: 0,
            schema: Arc::new(SettingsSchema::new(
                SettingsSchema::builtin().specs().to_vec(),
            )),
        })
    }

    #[must_use]
    pub const fn current(&self) -> &BoottyConfig {
        &self.current
    }

    #[must_use]
    pub const fn configured_font_size(&self) -> f32 {
        self.configured_font_size
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn settings_schema(&self) -> Arc<SettingsSchema> {
        Arc::clone(&self.schema)
    }

    #[must_use]
    pub const fn document(&self) -> &ConfigDocument {
        &self.document
    }

    /// Reload the file and publish its candidate only after the caller accepts it.
    ///
    /// # Errors
    /// Returns a config loading error or the caller's validation error; the accepted
    /// configuration remains unchanged.
    pub fn reload<T>(
        &mut self,
        validate: impl FnOnce(&BoottyConfig) -> Result<T, String>,
    ) -> Result<(ConfigChange, T), ConfigRuntimeError> {
        let next = self.hot_reload.reload_config()?;
        let document = load_or_create_config_document(&next.config_path)?;
        let validated = validate(&next).map_err(ConfigRuntimeError::Validation)?;
        let change = self.accept(next);
        self.document = document;
        Ok((change, validated))
    }

    /// Validate a complete editable document, replace it atomically, then publish its config.
    ///
    /// # Errors
    /// Returns an error if document validation or atomic replacement fails; the accepted
    /// configuration remains unchanged.
    pub fn commit_document<T>(
        &mut self,
        document: ConfigDocument,
        validate: impl FnOnce(&BoottyConfig) -> Result<T, String>,
    ) -> ConfigResult<(ConfigChange, ConfigDocument, ConfigWriteOutcome, T)> {
        let path = self.current.config_path.clone();
        let (accepted, validated) = commit_config_document(&path, document, validate)?;
        let change = self.accept(accepted.config);
        self.hot_reload.refresh_dependency_graph();
        self.document = accepted.document.clone();
        Ok((change, accepted.document, accepted.write_outcome, validated))
    }

    fn accept(&mut self, next: BoottyConfig) -> ConfigChange {
        self.revision = self.revision.wrapping_add(1);
        self.configured_font_size = next.font.size;
        let previous = std::mem::replace(&mut self.current, next);
        let current = self.current.clone();
        ConfigChange { previous, current }
    }

    #[must_use]
    pub fn reload_due(&mut self, now: Instant) -> bool {
        self.hot_reload.changed(now)
    }

    /// Apply a live layout width without changing the persisted document.
    pub fn set_sidebar_width(&mut self, width: f32) {
        self.revise_current(|config| config.chrome.sidebar_width = width);
    }

    /// Replace the live configuration for a transient preview without changing the configured
    /// font-size baseline used by zoom reset.
    pub fn replace_preview_config(&mut self, config: BoottyConfig) {
        self.revise_current(|current| *current = config);
    }

    /// Apply a live zoom size while retaining the last accepted persisted font size as reset
    /// baseline.
    pub fn set_font_size(&mut self, size: f32) {
        self.revise_current(|config| config.font.size = size);
    }

    fn revise_current(&mut self, mutate: impl FnOnce(&mut BoottyConfig)) {
        self.revision = self.revision.wrapping_add(1);
        mutate(&mut self.current);
    }
}
