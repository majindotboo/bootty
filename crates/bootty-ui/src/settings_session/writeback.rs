use num_traits::ToPrimitive as _;

use std::{collections::HashSet, sync::Arc};

use bootty_config::{
    color::Color,
    config::{BoottyConfig, ConfigDocument, ConfigResult, StatusSegment},
    settings_schema::{SettingKind, SettingSpec, SettingValue, SettingsSchema},
};

/// Schema-checked draft document. Persistence remains an application effect.
pub struct DraftWriteback {
    document: ConfigDocument,
    dirty: bool,
    edited: HashSet<String>,
    submission_pending: bool,
    last_error: Option<String>,
    schema: Arc<SettingsSchema>,
}

impl DraftWriteback {
    #[must_use]
    pub fn new(document: ConfigDocument, schema: Arc<SettingsSchema>) -> Self {
        Self {
            document,
            dirty: false,
            edited: HashSet::new(),
            submission_pending: false,
            last_error: None,
            schema,
        }
    }

    pub fn set_schema(&mut self, schema: Arc<SettingsSchema>) {
        self.schema = schema;
    }

    #[must_use]
    pub const fn schema(&self) -> &Arc<SettingsSchema> {
        &self.schema
    }

    #[must_use]
    pub const fn document(&self) -> &ConfigDocument {
        &self.document
    }

    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn reject(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    pub fn reconcile(&mut self, document: ConfigDocument) {
        if !self.dirty {
            self.document = document;
            self.edited.clear();
        }
    }

    pub fn accept(&mut self, document: ConfigDocument, warning: Option<String>) {
        self.document = document;
        self.dirty = false;
        self.edited.clear();
        self.submission_pending = false;
        self.last_error = warning;
    }

    pub fn take_submission(&mut self) -> Option<ConfigDocument> {
        std::mem::take(&mut self.submission_pending).then(|| self.document.clone())
    }

    #[must_use]
    pub fn value_of(&self, spec: &SettingSpec) -> Option<SettingValue> {
        self.value_at(spec, &spec.path_parts())
    }

    fn value_at(&self, spec: &SettingSpec, path: &[&str]) -> Option<SettingValue> {
        match &spec.kind {
            SettingKind::Bool => self.document.bool_at(path).map(SettingValue::Bool),
            SettingKind::FontStyle => self
                .document
                .str_at(path)
                .map(|value| SettingValue::Token(value.to_owned()))
                .or_else(|| self.document.bool_at(path).map(SettingValue::Bool)),
            SettingKind::Text { .. } => self
                .document
                .str_at(path)
                .map(|value| SettingValue::Text(value.to_owned())),
            SettingKind::Number { .. } => self
                .document
                .f64_at(path)
                .and_then(|value| value.to_f32())
                .map(SettingValue::Number),
            SettingKind::Choice { .. } => self
                .document
                .str_at(path)
                .map(|value| SettingValue::Token(value.to_owned())),
            SettingKind::Custom(_) => None,
        }
    }

    /// Compare persisted scalar values with their schema-owned typed default.
    pub(super) fn scalar_is_default(
        &self,
        spec: &SettingSpec,
        current: &BoottyConfig,
        defaults: &BoottyConfig,
    ) -> Option<bool> {
        let default = spec.default_value(defaults)?;
        let path = spec.path_parts();
        let path = if self.document.contains(&path) {
            path
        } else if let Some(legacy) = spec.supersedes.iter().find(|legacy| {
            self.document
                .contains(&legacy.iter().map(AsRef::as_ref).collect::<Vec<_>>())
        }) {
            legacy.iter().map(AsRef::as_ref).collect()
        } else {
            return Some(spec.default_value(current)? == default);
        };
        let value = self.value_at(spec, &path);
        Some(value.is_some_and(|value| {
            value == default
                || (matches!(spec.kind, SettingKind::FontStyle)
                    && value == SettingValue::Token(String::new())
                    && default == SettingValue::Token("auto".into()))
        }))
    }

    pub(super) fn custom_scalar_is_default(
        &self,
        path: &[&str],
        current: &SettingValue,
        default: &SettingValue,
    ) -> bool {
        let explicit = match default {
            SettingValue::Bool(_) => self.document.bool_at(path).map(SettingValue::Bool),
            SettingValue::Number(_) => self
                .document
                .f64_at(path)
                .and_then(|value| value.to_f32())
                .map(SettingValue::Number),
            SettingValue::Text(_) => self
                .document
                .str_at(path)
                .map(|value| SettingValue::Text(value.into())),
            SettingValue::Token(_) => self
                .document
                .str_at(path)
                .map(|value| SettingValue::Token(value.into())),
        };
        if self.document.contains(path) {
            explicit.as_ref() == Some(default)
        } else {
            current == default
        }
    }

    pub fn write(&mut self, spec: &SettingSpec, value: &SettingValue) {
        for legacy in &spec.supersedes {
            let path = legacy.iter().map(AsRef::as_ref).collect::<Vec<_>>();
            self.remove(&path);
        }
        let path = spec.path_parts();
        match value {
            SettingValue::Bool(value) => self.set_bool(&path, *value),
            SettingValue::Number(value) => self.set_f32(&path, *value),
            SettingValue::Text(value) | SettingValue::Token(value) => self.set_str(&path, value),
        }
    }

    pub fn set_bool(&mut self, path: &[&str], value: bool) {
        self.mutate_setting(path, |document| document.set_bool(path, value));
    }

    pub fn set_f32(&mut self, path: &[&str], value: f32) {
        self.mutate_setting(path, |document| document.set_f32(path, value));
    }

    pub fn set_u16(&mut self, path: &[&str], value: u16) {
        self.mutate_setting(path, |document| document.set_i64(path, i64::from(value)));
    }

    pub fn set_i64(&mut self, path: &[&str], value: i64) {
        self.mutate_setting(path, |document| document.set_i64(path, value));
    }

    pub fn set_str(&mut self, path: &[&str], value: &str) {
        self.mutate_setting(path, |document| document.set_str(path, value));
    }

    /// A native terminal backend cannot have a default remote. Keep the pair in one document
    /// mutation so persistence validates and publishes only the complete configuration.
    pub fn set_multiplexer_backend(&mut self, backend: &str) {
        let path = ["multiplexer", "backend"];
        self.mutate_setting(&path, |document| {
            document.set_str(&path, backend)?;
            if backend == "native" {
                document.remove_multiplexer_remote()?;
            }
            Ok(())
        });
    }

    pub fn set_strings(&mut self, path: &[&str], value: &[String]) {
        self.mutate_setting(path, |document| document.set_strings(path, value));
    }

    pub fn set_env(&mut self, path: &[&str], value: &[(String, String)]) {
        self.mutate_setting(path, |document| document.set_env(path, value));
    }

    pub fn set_top_status_segments(&mut self, value: &[StatusSegment]) {
        let path = ["chrome", "top-segment"];
        self.mutate_setting(&path, |document| document.set_top_status_segments(value));
    }

    pub fn set_bottom_status_segments(&mut self, value: &[StatusSegment]) {
        let path = ["chrome", "bottom-segment"];
        self.mutate_setting(&path, |document| document.set_bottom_status_segments(value));
    }

    pub fn set_color(&mut self, path: &[&str], color: Color) {
        let value = if color.a == 0xff {
            format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
        } else {
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                color.r, color.g, color.b, color.a
            )
        };
        self.set_str(path, &value);
    }

    pub fn remove(&mut self, path: &[&str]) {
        let id = path.join(".");
        let removed = self.document.contains(path) || self.was_removed(&id);
        self.mutate_setting(path, |document| document.remove(path));
        // Removing an absent root key cannot change a value inherited from another file.
        if !removed {
            self.edited.remove(&id);
        }
    }

    pub(super) fn has_edit(&self, id: &str) -> bool {
        self.edited.iter().any(|path| {
            id == path
                || id
                    .strip_prefix(path)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    }

    pub(super) fn was_removed(&self, id: &str) -> bool {
        self.has_edit(id) && !self.document.contains(&id.split('.').collect::<Vec<_>>())
    }

    #[must_use]
    pub fn string_array(&self, path: &[&str]) -> Option<Vec<String>> {
        self.document.string_array(path)
    }

    fn mutate_setting(
        &mut self,
        path: &[&str],
        mutation: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>,
    ) {
        if !self.schema.allows_write_path(path) {
            self.reject(format!("undeclared settings path {}", path.join(".")));
            return;
        }
        match mutation(&mut self.document) {
            Ok(()) => {
                self.edited.insert(path.join("."));
                self.dirty = true;
                self.submission_pending = true;
                self.last_error = None;
            }
            Err(error) => self.reject(error.to_string()),
        }
    }
}
