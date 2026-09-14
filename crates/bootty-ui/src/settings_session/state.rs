use num_traits::ToPrimitive as _;
use std::{collections::BTreeMap, sync::Arc};

use bootty_config::{
    config::{BoottyConfig, ConfigDocument, StatusSegment},
    settings_schema::{SettingValue, SettingsSchema},
};

use crate::settings_session::{
    DefaultRemote, DraftWriteback, FontFeatureDraft, ModuleOutcome, RemoteDraft,
    RemoteEditorSnapshot, RemoteProfile, SettingsEffect, SettingsOutcome, StatusSegmentEdit,
    dedupe_font_features, remotes::RemoteState, status_segments::apply_status_segment_edit,
};

#[derive(Clone, Debug, Default)]
pub struct Catalogs {
    /// Installed font families reported by the native host text system.
    pub font_families: Arc<[String]>,
    pub status_modules: Vec<String>,
    pub top_status_segments: Vec<StatusSegment>,
    pub bottom_status_segments: Vec<StatusSegment>,
    pub remotes: Vec<RemoteProfile>,
    pub default_remote: DefaultRemote,
    pub environment: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentDraft {
    pub name: String,
    pub value: String,
}

#[derive(Clone)]
pub struct AcceptedSettings {
    pub revision: u64,
    pub config: Arc<BoottyConfig>,
    pub document: ConfigDocument,
    pub schema: Arc<SettingsSchema>,
}

/// Editable config draft and asynchronous operation results.
pub struct SettingsSession {
    accepted_revision: u64,
    defaults: BoottyConfig,
    config: Arc<BoottyConfig>,
    font_families: Arc<[String]>,
    writeback: DraftWriteback,
    status_modules: Vec<String>,
    top_status_segments: Vec<StatusSegment>,
    bottom_status_segments: Vec<StatusSegment>,
    remotes: RemoteState,
    integration_errors: BTreeMap<String, String>,
    unscoped_integration_error: Option<String>,
    environment: Vec<EnvironmentDraft>,
    environment_draft_dirty: bool,
    effects: Vec<SettingsEffect>,
}

impl SettingsSession {
    #[must_use]
    pub fn new(accepted: AcceptedSettings, catalogs: Catalogs) -> Self {
        let writeback = DraftWriteback::new(accepted.document, accepted.schema);
        let mut remotes = RemoteState::default();
        remotes.set_profiles(catalogs.remotes);
        remotes.set_default(catalogs.default_remote);
        let environment = environment_drafts(catalogs.environment);
        Self {
            accepted_revision: accepted.revision,
            defaults: BoottyConfig::default(),
            config: accepted.config,
            font_families: catalogs.font_families,
            writeback,
            status_modules: catalogs.status_modules,
            top_status_segments: catalogs.top_status_segments,
            bottom_status_segments: catalogs.bottom_status_segments,
            remotes,
            integration_errors: BTreeMap::new(),
            unscoped_integration_error: None,
            environment,
            environment_draft_dirty: false,
            effects: Vec::new(),
        }
    }

    /// Reconcile a new accepted snapshot without replacing an unsubmitted local draft.
    pub fn reconcile_accepted(&mut self, accepted: AcceptedSettings) {
        if accepted.revision < self.accepted_revision {
            return;
        }
        self.config = accepted.config;
        if accepted.revision == self.accepted_revision {
            return;
        }
        self.accepted_revision = accepted.revision;
        self.writeback.set_schema(accepted.schema);
        self.writeback.reconcile(accepted.document);
    }

    pub fn set_catalogs(&mut self, catalogs: Catalogs) {
        self.font_families = catalogs.font_families;
        self.status_modules = catalogs.status_modules;
        self.top_status_segments = catalogs.top_status_segments;
        self.bottom_status_segments = catalogs.bottom_status_segments;
        self.remotes.set_profiles(catalogs.remotes);
        self.remotes.set_default(catalogs.default_remote);
        if !self.environment_draft_dirty {
            self.environment = environment_drafts(catalogs.environment);
        }
    }

    /// Surface validation from a structured editor without mutating the draft.
    pub fn reject(&mut self, error: impl Into<String>) {
        self.writeback.reject(error);
    }

    pub fn set_value(&mut self, id: &str, value: &SettingValue) -> bool {
        let Some(spec) = self.writeback_schema().get(id).cloned() else {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        };
        self.writeback.write(&spec, value);
        self.queue_document_submission();
        true
    }

    /// Select the terminal backend without ever leaving an invalid native default remote behind.
    pub fn set_multiplexer_backend(&mut self, backend: &str) -> bool {
        let path = ["multiplexer", "backend"];
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback.reject("unknown setting multiplexer.backend");
            return false;
        }
        self.writeback.set_multiplexer_backend(backend);
        self.queue_document_submission();
        true
    }

    /// Write a leaf owned by a custom settings editor through the same schema-checked draft.
    pub fn set_custom_value(&mut self, id: &str, value: &SettingValue) -> bool {
        let path = id.split('.').collect::<Vec<_>>();
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        }
        match value {
            SettingValue::Bool(value) => self.writeback.set_bool(&path, *value),
            SettingValue::Number(value) => self.writeback.set_f32(&path, *value),
            SettingValue::Text(value) | SettingValue::Token(value) => {
                self.writeback.set_str(&path, value);
            }
        }
        self.queue_document_submission();
        true
    }

    pub fn set_custom_u16(&mut self, id: &str, value: u16) -> bool {
        let path = id.split('.').collect::<Vec<_>>();
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        }
        self.writeback.set_u16(&path, value);
        self.queue_document_submission();
        true
    }

    pub fn set_custom_i64(&mut self, id: &str, value: i64) -> bool {
        let path = id.split('.').collect::<Vec<_>>();
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        }
        self.writeback.set_i64(&path, value);
        self.queue_document_submission();
        true
    }

    /// Persist the structured session environment through `ConfigDocument`'s typed table writer.
    pub fn set_environment(&mut self, entries: Vec<(String, String)>) -> bool {
        let path = ["session", "env"];
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback.reject("unknown setting session.env");
            return false;
        }
        if entries.is_empty() {
            self.writeback.remove(&path);
        } else {
            self.writeback.set_env(&path, &entries);
        }
        self.environment = environment_drafts(entries);
        self.queue_document_submission();
        true
    }

    pub fn set_environment_name(&mut self, index: usize, name: String) -> bool {
        let Some(entry) = self.environment.get_mut(index) else {
            return false;
        };
        entry.name = name;
        self.environment_draft_dirty = true;
        self.submit_environment_draft();
        true
    }

    pub fn set_environment_value(&mut self, index: usize, value: String) -> bool {
        let Some(entry) = self.environment.get_mut(index) else {
            return false;
        };
        entry.value = value;
        self.environment_draft_dirty = true;
        self.submit_environment_draft();
        true
    }

    pub fn add_environment_variable(&mut self) {
        self.environment.push(EnvironmentDraft::default());
        self.environment_draft_dirty = true;
    }

    pub fn remove_environment_variable(&mut self, index: usize) -> bool {
        if index >= self.environment.len() {
            return false;
        }
        self.environment.remove(index);
        self.environment_draft_dirty = true;
        self.submit_environment_draft();
        true
    }

    pub fn move_environment_variable(&mut self, index: usize, offset: isize) -> bool {
        let Some(target) = index.checked_add_signed(offset) else {
            return false;
        };
        if index >= self.environment.len() || target >= self.environment.len() {
            return false;
        }
        let entry = self.environment.remove(index);
        self.environment.insert(target, entry);
        self.environment_draft_dirty = true;
        self.submit_environment_draft();
        true
    }

    /// Persist status modules as their real structured config value, rather than a lossy summary.
    pub fn set_status_segments(&mut self, top: bool, segments: Vec<StatusSegment>) -> bool {
        let path = if top {
            ["chrome", "top-segment"]
        } else {
            ["chrome", "bottom-segment"]
        };
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback
                .reject(format!("unknown setting {}", path.join(".")));
            return false;
        }
        if segments.is_empty() {
            self.writeback.remove(&path);
        } else if top {
            self.writeback.set_top_status_segments(&segments);
        } else {
            self.writeback.set_bottom_status_segments(&segments);
        }
        if top {
            self.top_status_segments = segments;
        } else {
            self.bottom_status_segments = segments;
        }
        self.queue_document_submission();
        true
    }

    pub fn edit_status_segments(&mut self, top: bool, edit: StatusSegmentEdit) -> bool {
        let mut segments = if top {
            self.top_status_segments.clone()
        } else {
            self.bottom_status_segments.clone()
        };
        if let Err(error) = apply_status_segment_edit(&mut segments, edit) {
            self.writeback.reject(error);
            return false;
        }
        self.set_status_segments(top, segments)
    }

    #[must_use]
    pub fn status_segments(&self, top: bool) -> &[StatusSegment] {
        if top {
            &self.top_status_segments
        } else {
            &self.bottom_status_segments
        }
    }

    /// Whether a row names a schema-owned value or structured editor.
    #[must_use]
    pub fn can_reset(&self, id: &str) -> bool {
        self.writeback_schema()
            .allows_write_path(&id.split('.').collect::<Vec<_>>())
    }

    /// Whether the value shown by Settings equals its built-in default.
    #[must_use]
    pub fn is_default(&self, id: &str) -> bool {
        let current = if self.writeback.was_removed(id) {
            &self.defaults
        } else {
            &self.config
        };
        let defaults = &self.defaults;
        if let Some(spec) = self.writeback_schema().get(id)
            && let Some(equal) = self.writeback.scalar_is_default(spec, current, defaults)
        {
            return equal;
        }
        let path = id.split('.').collect::<Vec<_>>();
        let document = self.writeback.document();
        if let (Some(value), Some(default)) =
            (custom_scalar(current, id), custom_scalar(defaults, id))
        {
            return self
                .writeback
                .custom_scalar_is_default(&path, &value, &default);
        }
        if let (Some(value), Some(default)) = (custom_list(current, id), custom_list(defaults, id))
        {
            let explicit = document.string_array(&path);
            let value = explicit.as_deref().unwrap_or(value);
            return if matches!(id, "font.family" | "font.ui-family") {
                font_stacks_equal(value, default)
            } else {
                value == default
            };
        }
        match id {
            "font.features" => self.font_features_are_default(current, defaults),
            "chrome.top-segment" => {
                let value = if self.writeback.has_edit(id) && !self.writeback.was_removed(id) {
                    &self.top_status_segments
                } else {
                    &current.chrome.top_segments
                };
                *value == defaults.chrome.top_segments
            }
            "chrome.bottom-segment" => {
                let value = if self.writeback.has_edit(id) && !self.writeback.was_removed(id) {
                    &self.bottom_status_segments
                } else {
                    &current.chrome.bottom_segments
                };
                *value == defaults.chrome.bottom_segments
            }
            "session.env" => {
                if self.environment_draft_dirty || self.writeback.has_edit(id) {
                    self.environment
                        .iter()
                        .map(|entry| (&entry.name, &entry.value))
                        .eq(defaults
                            .session
                            .env
                            .iter()
                            .map(|(name, value)| (name, value)))
                } else {
                    current.session.env == defaults.session.env
                }
            }
            "font.cell-width" => {
                document
                    .f64_at(&path)
                    .and_then(|value| value.to_f32())
                    .or(current.font.cell_width)
                    == defaults.font.cell_width
            }
            "font.cell-height" => {
                document
                    .f64_at(&path)
                    .and_then(|value| value.to_f32())
                    .or(current.font.cell_height)
                    == defaults.font.cell_height
            }
            _ => color_is_default(
                document,
                &self.config,
                defaults,
                id,
                self.writeback.was_removed(id),
            )
            .unwrap_or_else(|| !document.contains(&path)),
        }
    }

    fn font_features_are_default(&self, current: &BoottyConfig, defaults: &BoottyConfig) -> bool {
        let features = self.font_features().unwrap_or_else(|| {
            current
                .font
                .features
                .iter()
                .copied()
                .map(FontFeatureDraft::from)
                .collect()
        });
        features
            == defaults
                .font
                .features
                .iter()
                .copied()
                .map(FontFeatureDraft::from)
                .collect::<Vec<_>>()
    }

    pub fn remove_value(&mut self, id: &str) -> bool {
        let Some(spec) = self.writeback_schema().get(id).cloned() else {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        };
        if id == "multiplexer.backend" {
            let backend = bootty_config::config::MultiplexerBackendConfig::default()
                .to_string()
                .to_ascii_lowercase();
            self.writeback.set_multiplexer_backend(&backend);
        }
        for legacy in &spec.supersedes {
            self.writeback
                .remove(&legacy.iter().map(AsRef::as_ref).collect::<Vec<_>>());
        }
        self.writeback.remove(&spec.path_parts());
        if id == "session.env" {
            self.environment.clear();
            self.environment_draft_dirty = false;
        }
        self.queue_document_submission();
        true
    }

    pub fn remove_custom_value(&mut self, id: &str) -> bool {
        let path = id.split('.').collect::<Vec<_>>();
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        }
        self.writeback.remove(&path);
        self.queue_document_submission();
        true
    }

    #[must_use]
    pub fn value(&self, id: &str) -> Option<SettingValue> {
        self.writeback_schema()
            .get(id)
            .and_then(|spec| self.writeback.value_of(spec))
    }

    #[must_use]
    pub fn string_list(&self, id: &str) -> Option<Vec<String>> {
        let spec = self.writeback_schema().get(id)?;
        self.writeback.string_array(&spec.path_parts())
    }

    pub fn set_string_list(&mut self, id: &str, values: &[String]) -> bool {
        let Some(spec) = self.writeback_schema().get(id).cloned() else {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        };
        let path = spec.path_parts();
        if values.is_empty() {
            self.writeback.remove(&path);
        } else {
            self.writeback.set_strings(&path, values);
        }
        self.queue_document_submission();
        true
    }

    #[must_use]
    pub fn font_features(&self) -> Option<Vec<FontFeatureDraft>> {
        self.string_list("font.features").map(|settings| {
            settings
                .iter()
                .filter_map(|setting| FontFeatureDraft::parse(setting).ok())
                .collect()
        })
    }

    /// Validate, deduplicate, and persist OpenType features as one typed document submission.
    pub fn set_font_features(&mut self, features: Vec<FontFeatureDraft>) -> bool {
        let features = dedupe_font_features(features);
        let settings = features
            .iter()
            .map(FontFeatureDraft::setting)
            .collect::<Result<Vec<_>, _>>();
        let settings = match settings {
            Ok(settings) => settings,
            Err(error) => {
                self.writeback.reject(error);
                return false;
            }
        };
        self.set_string_list("font.features", &settings)
    }

    /// Read the explicit ANSI palette override, including the legacy dark-branch location.
    #[must_use]
    pub fn ansi_palette(&self, id: &str) -> Option<Vec<String>> {
        let path = id.split('.').collect::<Vec<_>>();
        self.writeback.string_array(&path).or_else(|| {
            (id == "appearance.dark.colors.palette")
                .then(|| self.writeback.string_array(&["colors", "palette"]))
                .flatten()
        })
    }

    /// Persist the ANSI palette as one typed array and retire its legacy dark-branch location.
    pub fn set_ansi_palette(&mut self, id: &str, values: &[String]) -> bool {
        let path = id.split('.').collect::<Vec<_>>();
        if !self.writeback.schema().allows_write_path(&path) {
            self.writeback.reject(format!("unknown setting {id}"));
            return false;
        }
        if id == "appearance.dark.colors.palette" {
            self.writeback.remove(&["colors", "palette"]);
        }
        if values.is_empty() {
            self.writeback.remove(&path);
        } else {
            self.writeback.set_strings(&path, values);
        }
        self.queue_document_submission();
        true
    }

    #[must_use]
    pub const fn draft_document(&self) -> &ConfigDocument {
        self.writeback.document()
    }

    pub fn install_integration(&mut self, identity: String, module: String, id: String) {
        self.effects.push(SettingsEffect::InstallIntegration {
            identity,
            module,
            id,
        });
    }

    pub fn uninstall_integration(&mut self, identity: String, module: String, id: String) {
        self.effects.push(SettingsEffect::UninstallIntegration {
            identity,
            module,
            id,
        });
    }

    /// Return the last native integration failure for a provider, if any.
    #[must_use]
    pub fn integration_error(&self, identity: &str) -> Option<&str> {
        self.integration_errors
            .get(identity)
            .map(String::as_str)
            .or(self.unscoped_integration_error.as_deref())
    }

    pub fn select_remote(&mut self, id: &str) -> bool {
        self.remotes.select(id)
    }

    pub fn new_remote(&mut self, id: String) {
        self.remotes.new_draft(id);
    }

    pub fn edit_remote(&mut self, draft: RemoteDraft) {
        self.remotes.set_draft(draft);
    }

    pub fn save_remote(&mut self) -> bool {
        let Some(profile) = self.remotes.validated_draft() else {
            return false;
        };
        self.effects.push(SettingsEffect::UpsertRemote(profile));
        true
    }

    pub fn remove_remote(&mut self, id: String) {
        self.effects.push(SettingsEffect::RemoveRemote { id });
    }

    pub fn clear_default_remote(&mut self) {
        self.effects.push(self.remotes.clear_default());
    }

    pub fn edit_default_remote(&mut self, field: &str, value: String) -> bool {
        self.remotes.edit_default(field, value)
    }

    pub fn edit_remote_argument(&mut self, id: &str, index: usize, value: String) -> bool {
        self.remotes.edit_argument(id, index, value)
    }

    pub fn add_remote_argument(&mut self, id: &str) -> bool {
        self.remotes.add_argument(id)
    }

    pub fn remove_remote_argument(&mut self, id: &str, index: usize) -> bool {
        self.remotes.remove_argument(id, index)
    }

    pub fn save_default_remote(&mut self) -> bool {
        if !self.selected_backend_supports_remote() {
            self.remotes
                .reject_default("Choose herdr, rmux, or tmux before saving a default remote.");
            return false;
        }
        let Some(effect) = self.remotes.save_default() else {
            return false;
        };
        self.effects.push(effect);
        true
    }

    pub fn test_remote(&mut self) {
        if let Some(effect) = self.remotes.test() {
            self.effects.push(effect);
        }
    }

    pub fn test_remote_with_fields(&mut self, id: &str, fields: Vec<(String, String)>) -> bool {
        let Some(effect) = self.remotes.test_with_fields(id, fields) else {
            return false;
        };
        self.effects.push(effect);
        true
    }

    pub fn apply_outcome(&mut self, outcome: SettingsOutcome) {
        match outcome {
            SettingsOutcome::DocumentAccepted {
                revision,
                document,
                warning,
            } => {
                self.accepted_revision = self.accepted_revision.max(revision);
                self.writeback.accept(document, warning);
                self.environment_draft_dirty = false;
            }
            SettingsOutcome::DocumentRejected(error) => self.writeback.reject(error),
            SettingsOutcome::Module(ModuleOutcome::IntegrationUpdated { identity }) => {
                self.integration_errors.remove(&identity);
                self.unscoped_integration_error = None;
            }
            SettingsOutcome::Module(ModuleOutcome::Failed { identity, message }) => {
                if let Some(identity) = identity {
                    self.integration_errors.insert(identity, message);
                } else {
                    self.unscoped_integration_error = Some(message);
                }
            }
            SettingsOutcome::Remote(outcome) => self.remotes.apply(outcome),
        }
    }

    pub fn take_effects(&mut self) -> Vec<SettingsEffect> {
        std::mem::take(&mut self.effects)
    }

    #[must_use]
    pub fn environment(&self) -> &[EnvironmentDraft] {
        &self.environment
    }

    #[must_use]
    pub fn write_error(&self) -> Option<&str> {
        self.writeback.last_error()
    }

    #[must_use]
    pub const fn font_families(&self) -> &Arc<[String]> {
        &self.font_families
    }

    #[must_use]
    pub fn status_modules(&self) -> &[String] {
        &self.status_modules
    }

    #[must_use]
    pub fn remotes(&self) -> RemoteEditorSnapshot {
        self.remotes.snapshot()
    }

    fn queue_document_submission(&mut self) {
        if let Some(document) = self.writeback.take_submission() {
            self.effects.push(SettingsEffect::SubmitDocument(document));
        }
    }

    const fn writeback_schema(&self) -> &Arc<SettingsSchema> {
        // DraftWriteback deliberately owns schema mutation. This accessor avoids mirroring it in
        // the session; it can be removed once SettingsSchema offers an id-to-path DTO projection.
        self.writeback.schema()
    }

    fn selected_backend_supports_remote(&self) -> bool {
        matches!(
            self.writeback
                .document()
                .str_at(&["multiplexer", "backend"]),
            Some("herdr" | "rmux" | "tmux")
        )
    }

    fn submit_environment_draft(&mut self) {
        let mut names = std::collections::HashSet::new();
        let mut entries = Vec::with_capacity(self.environment.len());
        for entry in &self.environment {
            // An empty row is an unfinished addition/rename, not a failed write.
            if entry.name.is_empty() {
                return;
            }
            if !valid_environment_name(&entry.name) {
                self.writeback.reject("Environment variable names must start with a letter or underscore and contain only letters, digits, or underscores.");
                return;
            }
            if !names.insert(entry.name.clone()) {
                self.writeback.reject(format!(
                    "Environment variable {} appears more than once. Use a unique name.",
                    entry.name
                ));
                return;
            }
            entries.push((entry.name.clone(), entry.value.clone()));
        }
        self.set_environment(entries);
    }
}

fn environment_drafts(entries: Vec<(String, String)>) -> Vec<EnvironmentDraft> {
    entries
        .into_iter()
        .map(|(name, value)| EnvironmentDraft { name, value })
        .collect()
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn custom_scalar(config: &BoottyConfig, id: &str) -> Option<SettingValue> {
    use bootty_config::config::config_token;
    Some(match id {
        "appearance.mode" => SettingValue::Token(config_token(&config.appearance.mode)?),
        "appearance.light.theme" => {
            SettingValue::Token(config.appearance.light.theme.clone().unwrap_or_default())
        }
        "appearance.dark.theme" | "theme" => {
            SettingValue::Token(config.appearance.dark.theme.clone().unwrap_or_default())
        }
        "cursor.style" => SettingValue::Token(
            match config.cursor.style {
                Some(bootty_config::config::CursorStyleConfig::Bar) => "bar",
                Some(bootty_config::config::CursorStyleConfig::Block) => "block",
                Some(bootty_config::config::CursorStyleConfig::Underline) => "underline",
                Some(bootty_config::config::CursorStyleConfig::HollowBlock) => "hollow-block",
                None => "default",
            }
            .into(),
        ),
        "font.ui-use-terminal-family" => SettingValue::Bool(config.font.ui_use_terminal_family),
        "chrome.top-bar" => SettingValue::Bool(config.chrome.top_bar),
        "chrome.notched-fullscreen-black-chrome" => {
            SettingValue::Bool(config.chrome.notched_fullscreen_black_chrome)
        }
        "input.hide-mouse-pointer-while-typing" => {
            SettingValue::Bool(config.input.hide_mouse_pointer_while_typing)
        }
        "input.copy-on-select" => SettingValue::Bool(config.input.copy_on_select),
        "input.macos-option-as-alt" => SettingValue::Token(
            match config.input.macos_option_as_alt {
                bootty_config::config::MacosOptionAsAltConfig::None => "none",
                bootty_config::config::MacosOptionAsAltConfig::Left => "left",
                bootty_config::config::MacosOptionAsAltConfig::Right => "right",
                bootty_config::config::MacosOptionAsAltConfig::Both => "both",
            }
            .into(),
        ),
        "input.preset" => SettingValue::Token(config.input.preset.as_str().into()),
        "input.prefix" => SettingValue::Text(config.input.prefix.clone().unwrap_or_default()),
        "multiplexer.backend" => {
            SettingValue::Token(config.multiplexer.backend.to_string().to_ascii_lowercase())
        }
        "session.max-scrollback" => {
            SettingValue::Number(config.session.max_scrollback.to_f32().unwrap_or(f32::MAX))
        }
        "window.fullscreen-top-offset" => {
            SettingValue::Number(config.window.fullscreen_top_offset.unwrap_or(0.0))
        }
        _ => return None,
    })
}

fn custom_list<'a>(config: &'a BoottyConfig, id: &str) -> Option<&'a [String]> {
    Some(match id {
        "font.family" => &config.font.family,
        "font.ui-family" => &config.font.ui_family,
        "sidebar.session-modules" => &config.sidebar.session_modules,
        "sidebar.modules" => &config.sidebar.modules,
        "input.modifier-remap" => &config.input.modifier_remap,
        "input.keybind" => &config.input.keybind,
        "input.sidebar-keybind" => &config.input.sidebar_keybind,
        "input.backend-keybind.herdr" => &config.input.backend_keybinds.herdr,
        "input.backend-keybind.native" => &config.input.backend_keybinds.native,
        "input.backend-keybind.rmux" => &config.input.backend_keybinds.rmux,
        "input.backend-keybind.tmux" => &config.input.backend_keybinds.tmux,
        _ => return None,
    })
}

fn font_stacks_equal(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left == right || {
                let database = crate::font_database::system_font_database();
                let resolve = |name: &str| {
                    crate::font_database::query_font_id(
                        database,
                        &[fontdb::Family::Name(name)],
                        crate::terminal_text::FontStyle::Regular,
                    )
                };
                resolve(left).is_some_and(|id| Some(id) == resolve(right))
            }
        })
}

fn color_is_default(
    document: &ConfigDocument,
    current: &BoottyConfig,
    defaults: &BoottyConfig,
    id: &str,
    removed: bool,
) -> Option<bool> {
    use bootty_config::color::Color;
    let path = id.split('.').collect::<Vec<_>>();
    let (current, defaults, leaf) = match path.as_slice() {
        ["appearance", "light", "colors", leaf] => (
            &current.appearance.light.colors,
            &current.appearance.light.theme_colors,
            *leaf,
        ),
        ["appearance", "dark", "colors", leaf] | ["colors", leaf] => (
            &current.appearance.dark.colors,
            &current.appearance.dark.theme_colors,
            *leaf,
        ),
        ["chrome" | "sidebar", _] => {
            let select = |config: &BoottyConfig| match id {
                "chrome.status-background" => config.chrome.status_background,
                "chrome.pane-divider-color" => config.chrome.pane_divider_color,
                "chrome.pane-focus-border-color" => config.chrome.pane_focus_border_color,
                "sidebar.background" => config.sidebar.background,
                "sidebar.foreground" => config.sidebar.foreground,
                "sidebar.selected" => config.sidebar.selected,
                "sidebar.hover" => config.sidebar.hover,
                "sidebar.border" => config.sidebar.border,
                _ => None,
            };
            return Some(document.str_at(&path).map_or_else(
                || removed || select(current) == select(defaults),
                |value| Color::from_hex(value).is_ok_and(|value| Some(value) == select(defaults)),
            ));
        }
        _ => return None,
    };
    let legacy = ["colors", leaf];
    let path = if !document.contains(&path)
        && id.starts_with("appearance.dark.colors.")
        && document.contains(&legacy)
    {
        legacy.as_slice()
    } else {
        path.as_slice()
    };
    if leaf == "palette" {
        return Some(document.string_array(path).map_or_else(
            || removed || current.palette == defaults.palette,
            |values| {
                values
                    .iter()
                    .map(|value| Color::from_hex(value))
                    .collect::<Result<Vec<_>, _>>()
                    .is_ok_and(|value| value == defaults.palette)
            },
        ));
    }
    if matches!(leaf, "palette-generate" | "palette-harmonious") {
        let (value, default) = if leaf == "palette-generate" {
            (current.palette_generate, defaults.palette_generate)
        } else {
            (current.palette_harmonious, defaults.palette_harmonious)
        };
        return Some(
            document
                .bool_at(path)
                .unwrap_or(if removed { default } else { value })
                == default,
        );
    }
    let select = |colors: &bootty_config::config::ColorConfig| match leaf {
        "background" => colors.background,
        "foreground" => colors.foreground,
        "cursor" => colors.cursor,
        "cursor-text" => colors.cursor_text,
        "selection-background" => colors.selection_background,
        "selection-foreground" => colors.selection_foreground,
        "highlight-background" => colors.highlight_background,
        "highlight-foreground" => colors.highlight_foreground,
        "pointer-foreground" => colors.pointer_foreground,
        "pointer-background" => colors.pointer_background,
        "tektronix-foreground" => colors.tektronix_foreground,
        "tektronix-background" => colors.tektronix_background,
        "tektronix-cursor" => colors.tektronix_cursor,
        _ => None,
    };
    Some(document.str_at(path).map_or_else(
        || removed || select(current) == select(defaults),
        |value| Color::from_hex(value).is_ok_and(|value| Some(value) == select(defaults)),
    ))
}
