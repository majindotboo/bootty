//! What a setting *is*, as data.
//!
//! A [`SettingSpec`] names one config key once: its TOML path, the value it holds, how it is
//! labelled, and what it falls back to. The settings UI renders specs instead of hand-writing a
//! read/widget/write block per key. The native registry contains built-in settings; raw extension
//! tables remain a compatibility path and are not interpreted as executable declarations.
//!
//! This lives in `bootty-config`, not `bootty-ui`: a spec names [`BoottyConfig`] and config paths,
//! both product types. `bootty-ui` stays a widget library.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::OnceLock;

use crate::config::BoottyConfig;

mod builtin;

/// One editable config key.
#[derive(Clone, Debug)]
pub struct SettingSpec {
    /// TOML path, e.g. `["window", "width"]`. Also the spec's identity, joined with `.`.
    pub path: Vec<Cow<'static, str>>,
    pub label: Cow<'static, str>,
    pub help: Cow<'static, str>,
    /// Page the setting appears on, matching the settings surface's page ids.
    pub page: Cow<'static, str>,
    /// Section header within the page. Specs render in declaration order within a section.
    pub section: Cow<'static, str>,
    pub kind: SettingKind,
    /// Legacy paths removed whenever this key is written, so an old spelling cannot win the next
    /// load. Empty for almost every setting.
    pub supersedes: Vec<Vec<Cow<'static, str>>>,
    pub default: SettingDefault,
}

/// The path/page projection of one [`SettingSpec`]. The settings registry derives these values;
/// callers do not author a second declaration list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingDeclaration {
    /// TOML path. A `*` segment matches one dynamic table key; a trailing `*` matches the rest of
    /// a path, which is used for compatibility tables such as raw extension settings.
    pub path: Vec<Cow<'static, str>>,
    /// Settings page that owns the editor for this value.
    pub page: Cow<'static, str>,
}

impl SettingDeclaration {
    #[must_use]
    pub fn path_parts(&self) -> Vec<&str> {
        self.path.iter().map(Cow::as_ref).collect()
    }

    #[must_use]
    pub fn matches_path(&self, path: &[&str]) -> bool {
        let pattern = self.path_parts();
        let trailing_wildcard = pattern.last() == Some(&"*");
        if !trailing_wildcard && pattern.len() != path.len() {
            return false;
        }
        if trailing_wildcard && path.len() < pattern.len().saturating_sub(1) {
            return false;
        }
        pattern
            .iter()
            .zip(path)
            .all(|(expected, actual)| *expected == "*" || *expected == *actual)
    }

    #[must_use]
    fn matches_prefix(&self, path: &[&str]) -> bool {
        let pattern = self.path_parts();
        path.len() <= pattern.len()
            && pattern
                .iter()
                .zip(path)
                .all(|(expected, actual)| *expected == "*" || *expected == *actual)
    }
}

impl SettingSpec {
    /// Stable identity: the TOML path joined with `.`.
    #[must_use]
    pub fn id(&self) -> String {
        self.path.join(".")
    }

    /// The path as the borrowed slice the document readers and writers take.
    #[must_use]
    pub fn path_parts(&self) -> Vec<&str> {
        self.path.iter().map(Cow::as_ref).collect()
    }

    /// The scalar value shown when the document omits this key. Custom editors
    /// own their defaults and return `None` here.
    #[must_use]
    pub fn default_value(&self, defaults: &BoottyConfig) -> Option<SettingValue> {
        match &self.default {
            SettingDefault::Field(read) => Some(read(defaults)),
            SettingDefault::UiFontWeight(role) => Some(SettingValue::from(
                defaults
                    .font
                    .ui_weights
                    .get(role)
                    .unwrap_or(&crate::FontStyleAssignment::Automatic),
            )),
            SettingDefault::Unused => None,
        }
    }

    /// Whether `needle` matches this spec, for the settings search box.
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_ascii_lowercase();
        needle.is_empty()
            || self.label.to_ascii_lowercase().contains(&needle)
            || self.help.to_ascii_lowercase().contains(&needle)
            || self.id().to_ascii_lowercase().contains(&needle)
    }
}

/// Where a spec's fallback value comes from.
#[derive(Clone, Debug)]
pub enum SettingDefault {
    /// Built-in: read the field off the default config, so the fallback is never a literal copied
    /// out of `defaults.rs`, and a renamed field is a compile error at the spec.
    Field(fn(&BoottyConfig) -> SettingValue),
    UiFontWeight(crate::FontWeightRole),
    /// A hand-written editor owns the value and reads its typed config field directly.
    Unused,
}

/// What the setting holds, and how to edit it.
#[derive(Clone, Debug)]
pub enum SettingKind {
    Bool,
    /// An advertised font style name, `auto`, or false to use the base style.
    FontStyle,
    Text {
        placeholder: Cow<'static, str>,
        /// An empty value removes the key instead of writing an empty string.
        optional: bool,
    },
    Number {
        range: RangeInclusive<f32>,
        control: NumberControl,
        /// Decimal places shown. A count reads wrong as `3.0`.
        precision: usize,
        suffix: Cow<'static, str>,
        /// Multiplier applied for display only: a 0.0-1.0 fraction shown as a percentage uses 100.
        display_scale: f32,
    },
    Choice {
        options: Vec<SettingOption>,
    },
    /// A non-scalar setting whose editor is owned by a settings-surface module. It is still in
    /// this registry so the path cannot be accepted by the loader without an editor owner.
    Custom(SettingEditor),
}

/// The hand-written editor responsible for a non-scalar setting.
///
/// This is deliberately closed. Adding a new editor family requires adding a named owner here
/// and handling it in the settings surface, rather than silently creating another unregistered
/// path list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingEditor {
    Appearance,
    Colors,
    Text,
    General,
    Status,
    Sidebar,
    Remotes,
    Keys,
    Shell,
    Window,
    Extensions,
}

impl SettingEditor {
    /// Stable owner name used in diagnostics and exhaustive editor dispatch.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Colors => "colors",
            Self::Text => "text",
            Self::General => "general",
            Self::Status => "status",
            Self::Sidebar => "sidebar",
            Self::Remotes => "remotes",
            Self::Keys => "keys",
            Self::Shell => "shell",
            Self::Window => "window",
            Self::Extensions => "extensions",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberControl {
    Edit,
    Slider,
}

/// One choice in a [`SettingKind::Choice`].
#[derive(Clone, Debug)]
pub struct SettingOption {
    /// The token written to `config.toml`.
    pub token: Cow<'static, str>,
    pub label: Cow<'static, str>,
    /// One line saying what picking this option does. When any option on a setting carries one, the
    /// setting renders as a described list rather than a row of bare labels.
    pub description: Option<Cow<'static, str>>,
}

impl SettingOption {
    /// Build an option from an enum with an infallible static token conversion.
    #[must_use]
    pub fn of<T: Copy + Into<&'static str>>(value: &T, label: &'static str) -> Self {
        Self {
            token: (*value).into().into(),
            label: label.into(),
            description: None,
        }
    }

    /// Build an enum option with an explanation of its behavior.
    #[must_use]
    pub fn described<T: Copy + Into<&'static str>>(
        value: &T,
        label: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            description: Some(description.into()),
            ..Self::of(value, label)
        }
    }
}

/// A setting's value, in the shape the document holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Text(String),
    Number(f32),
    Token(String),
}

impl From<&crate::FontStyleAssignment> for SettingValue {
    fn from(style: &crate::FontStyleAssignment) -> Self {
        match style {
            crate::FontStyleAssignment::Automatic => Self::Token("auto".to_owned()),
            crate::FontStyleAssignment::Disabled => Self::Bool(false),
            crate::FontStyleAssignment::Named(name) => Self::Text(name.clone()),
        }
    }
}

impl SettingValue {
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_number(&self) -> Option<f32> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(value) | Self::Token(value) => Some(value),
            _ => None,
        }
    }
}

/// The settings the UI can render from the built-in registry.
#[derive(Debug, Default)]
pub struct SettingsSchema {
    specs: Vec<SettingSpec>,
    declarations: Vec<SettingDeclaration>,
    by_id: BTreeMap<String, usize>,
}

impl SettingsSchema {
    #[must_use]
    pub fn new(specs: Vec<SettingSpec>) -> Self {
        let by_id = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| (spec.id(), index))
            .collect();
        let declarations = specs
            .iter()
            .map(|spec| SettingDeclaration {
                path: spec.path.clone(),
                page: spec.page.clone(),
            })
            .collect();
        Self {
            specs,
            declarations,
            by_id,
        }
    }

    /// The built-in settings. Shared, because they never change within a run.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static SCHEMA: OnceLock<SettingsSchema> = OnceLock::new();
        SCHEMA.get_or_init(|| Self::new(builtin::specs()))
    }

    #[must_use]
    pub fn specs(&self) -> &[SettingSpec] {
        &self.specs
    }

    #[must_use]
    pub fn declarations(&self) -> &[SettingDeclaration] {
        &self.declarations
    }

    /// Whether a TOML path is a declared user setting.
    #[must_use]
    pub fn allows_path(&self, path: &[&str]) -> bool {
        (path == ["version"])
            || self
                .declarations
                .iter()
                .any(|declaration| declaration.matches_path(path))
            || builtin::compatibility_paths()
                .iter()
                .any(|pattern| path_matches(pattern, path))
    }

    /// Whether a write may target this path or a declared table below it.
    ///
    /// Hand-written editors sometimes serialize a complete table (for example one SSH profile)
    /// instead of setting each leaf independently. The table is allowed only when the registry
    /// declares at least one supported leaf below it.
    #[must_use]
    pub fn allows_write_path(&self, path: &[&str]) -> bool {
        self.allows_path(path)
            || self
                .declarations
                .iter()
                .any(|declaration| declaration.matches_prefix(path))
            || builtin::compatibility_paths()
                .iter()
                .any(|pattern| path_matches_prefix(pattern, path))
    }

    /// The settings on one page, in declaration order.
    pub fn page<'a>(&'a self, page: &'a str) -> impl Iterator<Item = &'a SettingSpec> + 'a {
        self.specs.iter().filter(move |spec| spec.page == page)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&SettingSpec> {
        self.by_id.get(id).and_then(|index| self.specs.get(*index))
    }
}

fn path_matches(pattern: &[&str], path: &[&str]) -> bool {
    let trailing_wildcard = pattern.last() == Some(&"*");
    if !trailing_wildcard && pattern.len() != path.len() {
        return false;
    }
    if trailing_wildcard && path.len() < pattern.len().saturating_sub(1) {
        return false;
    }
    pattern
        .iter()
        .zip(path)
        .all(|(expected, actual)| *expected == "*" || *expected == *actual)
}

fn path_matches_prefix(pattern: &[&str], path: &[&str]) -> bool {
    path.len() <= pattern.len()
        && pattern
            .iter()
            .zip(path)
            .all(|(expected, actual)| *expected == "*" || *expected == *actual)
}
