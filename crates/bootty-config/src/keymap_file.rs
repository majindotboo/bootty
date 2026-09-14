use std::{
    fmt, fs, io,
    ops::Range,
    path::{Path, PathBuf},
    str::FromStr,
};

use bootty_write::{CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};
use serde_json::{Map, Value};
use thiserror::Error;

pub const INITIAL_KEYMAP_CONTENT: &str = "[\n]\n";

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeymapContext {
    #[default]
    Global,
    Sidebar,
    Command,
    Terminal,
    Herdr,
    Native,
    Rmux,
    Tmux,
    /// A Zed-compatible GPUI key-context predicate supplied by the user.
    Expression(String),
}

impl KeymapContext {
    pub const ALL: [Self; 8] = [
        Self::Global,
        Self::Sidebar,
        Self::Command,
        Self::Terminal,
        Self::Herdr,
        Self::Native,
        Self::Rmux,
        Self::Tmux,
    ];

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Global => "Global",
            Self::Sidebar => "Sidebar",
            Self::Command => "Command",
            Self::Terminal => "Terminal",
            Self::Herdr => "Herdr",
            Self::Native => "Native",
            Self::Rmux => "rmux",
            Self::Tmux => "tmux",
            Self::Expression(expression) => expression,
        }
    }
}

impl fmt::Display for KeymapContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for KeymapContext {
    type Err = KeymapContextParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "" | "Global" | "global" | "Workspace" | "workspace" => Ok(Self::Global),
            "Sidebar" | "sidebar" => Ok(Self::Sidebar),
            "Command" | "command" => Ok(Self::Command),
            "Terminal" | "terminal" => Ok(Self::Terminal),
            "Herdr" | "herdr" | "Terminal && backend == herdr" => Ok(Self::Herdr),
            "Native" | "native" | "Terminal && backend == native" => Ok(Self::Native),
            "rmux" | "Rmux" | "Terminal && backend == rmux" => Ok(Self::Rmux),
            "tmux" | "Tmux" | "Terminal && backend == tmux" => Ok(Self::Tmux),
            value => Ok(Self::Expression(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("unknown keymap context {0:?}")]
pub struct KeymapContextParseError(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapAction {
    None,
    Command { name: String, input: Option<Value> },
}

impl KeymapAction {
    pub fn command(name: impl Into<String>) -> Self {
        Self::Command {
            name: name.into(),
            input: None,
        }
    }

    pub fn command_with_input(name: impl Into<String>, input: Value) -> Self {
        Self::Command {
            name: name.into(),
            input: Some(input),
        }
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Command { name, .. } => Some(name),
        }
    }

    #[must_use]
    pub const fn input(&self) -> Option<&Value> {
        match self {
            Self::None | Self::Command { input: None, .. } => None,
            Self::Command {
                input: Some(input), ..
            } => Some(input),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::None => Value::Null,
            Self::Command { name, input: None } => Value::String(name.clone()),
            Self::Command {
                name,
                input: Some(input),
            } => Value::Array(vec![Value::String(name.clone()), input.clone()]),
        }
    }

    fn parse(value: &Value) -> Result<Self, String> {
        match value {
            Value::Null => Ok(Self::None),
            Value::String(name) => Ok(Self::command(name)),
            Value::Array(items) => match items.as_slice() {
                [Value::String(name), input] => Ok(Self::command_with_input(name, input.clone())),
                _ => Err("expected null, an action name, or [name, input]".to_owned()),
            },
            _ => Err("expected null, an action name, or [name, input]".to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapBindingKind {
    Unbind,
    #[default]
    Binding,
}

impl KeymapBindingKind {
    const fn field(self) -> &'static str {
        match self {
            Self::Unbind => "unbind",
            Self::Binding => "bindings",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapEntry {
    pub keystrokes: String,
    pub action: KeymapAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapSection {
    pub context: KeymapContext,
    pub use_key_equivalents: bool,
    pub use_builtin_defaults: Option<bool>,
    pub unbind: Vec<KeymapEntry>,
    pub bindings: Vec<KeymapEntry>,
    source_index: usize,
}

impl KeymapSection {
    pub fn entries(
        &self,
        kind: KeymapBindingKind,
    ) -> impl DoubleEndedIterator<Item = &KeymapEntry> {
        match kind {
            KeymapBindingKind::Unbind => self.unbind.iter(),
            KeymapBindingKind::Binding => self.bindings.iter(),
        }
    }

    #[must_use]
    pub const fn binding_count(&self) -> usize {
        self.unbind.len().saturating_add(self.bindings.len())
    }

    #[must_use]
    pub const fn source_index(&self) -> usize {
        self.source_index
    }

    const fn entries_mut(&mut self, kind: KeymapBindingKind) -> &mut Vec<KeymapEntry> {
        match kind {
            KeymapBindingKind::Unbind => &mut self.unbind,
            KeymapBindingKind::Binding => &mut self.bindings,
        }
    }

    fn to_json(&self) -> Value {
        let mut section = Map::new();
        if self.context != KeymapContext::Global {
            section.insert(
                "context".to_owned(),
                Value::String(self.context.as_str().to_owned()),
            );
        }
        if self.use_key_equivalents {
            section.insert("use_key_equivalents".to_owned(), Value::Bool(true));
        }
        if let Some(use_builtin_defaults) = self.use_builtin_defaults {
            section.insert(
                "use_builtin_defaults".to_owned(),
                Value::Bool(use_builtin_defaults),
            );
        }
        insert_entries(&mut section, KeymapBindingKind::Unbind, &self.unbind);
        insert_entries(&mut section, KeymapBindingKind::Binding, &self.bindings);
        Value::Object(section)
    }
}

fn insert_entries(
    section: &mut Map<String, Value>,
    kind: KeymapBindingKind,
    entries: &[KeymapEntry],
) {
    if entries.is_empty() {
        return;
    }
    let mut values = Map::new();
    for entry in entries {
        values.insert(entry.keystrokes.clone(), entry.action.to_json());
    }
    section.insert(kind.field().to_owned(), Value::Object(values));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapDiagnostic {
    pub section: Option<usize>,
    pub field: Option<String>,
    pub message: String,
}

impl fmt::Display for KeymapDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.section, self.field.as_deref()) {
            (Some(section), Some(field)) => {
                write!(
                    formatter,
                    "section {} {field}: {}",
                    section.saturating_add(1),
                    self.message
                )
            }
            (Some(section), None) => write!(
                formatter,
                "section {}: {}",
                section.saturating_add(1),
                self.message
            ),
            (None, Some(field)) => write!(formatter, "{field}: {}", self.message),
            (None, None) => formatter.write_str(&self.message),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeymapFile {
    sections: Vec<KeymapSection>,
    diagnostics: Vec<KeymapDiagnostic>,
}

impl KeymapFile {
    ///
    /// # Errors
    /// Rejects malformed JSON, unterminated block comments, and non-array roots.
    /// Invalid entries within an array are retained as diagnostics.
    pub fn parse(contents: &str) -> Result<Self, KeymapParseError> {
        if contents.trim().is_empty() {
            return Ok(Self::default());
        }
        let sanitized = sanitize_jsonc(contents)?;
        let root: Value = serde_json::from_str(&sanitized)?;
        let Value::Array(values) = root else {
            return Err(KeymapParseError::RootMustBeArray);
        };

        let mut file = Self::default();
        for (source_index, value) in values.iter().enumerate() {
            file.parse_section(source_index, value);
        }
        Ok(file)
    }

    pub fn sections(&self) -> impl DoubleEndedIterator<Item = &KeymapSection> {
        self.sections.iter()
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[KeymapDiagnostic] {
        &self.diagnostics
    }

    /// Whether built-in bindings for exactly `context` participate in the effective keymap.
    ///
    /// Sections layer in source order, so the last explicit declaration wins. Omitting the field
    /// preserves Bootty's built-in defaults.
    #[must_use]
    pub fn use_builtin_defaults(&self, context: &KeymapContext) -> bool {
        self.sections
            .iter()
            .rev()
            .filter(|section| &section.context == context)
            .find_map(|section| section.use_builtin_defaults)
            .unwrap_or(true)
    }

    #[must_use]
    pub fn into_parts(self) -> (Vec<KeymapSection>, Vec<KeymapDiagnostic>) {
        (self.sections, self.diagnostics)
    }

    fn parse_section(&mut self, source_index: usize, value: &Value) {
        let Value::Object(section) = value else {
            self.diagnostic(
                source_index,
                None::<String>,
                "expected a keymap section object",
            );
            return;
        };

        let context = match section.get("context") {
            None => KeymapContext::Global,
            Some(Value::String(context)) => match context.parse::<KeymapContext>() {
                Ok(context) => context,
                Err(error) => {
                    self.diagnostic(source_index, Some("context"), error.to_string());
                    return;
                }
            },
            Some(_) => {
                self.diagnostic(source_index, Some("context"), "expected a string");
                return;
            }
        };
        let use_key_equivalents = match section.get("use_key_equivalents") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                self.diagnostic(
                    source_index,
                    Some("use_key_equivalents"),
                    "expected a boolean; using false",
                );
                false
            }
        };
        let use_builtin_defaults = match section.get("use_builtin_defaults") {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => {
                self.diagnostic(
                    source_index,
                    Some("use_builtin_defaults"),
                    "expected a boolean; using the inherited default",
                );
                None
            }
        };

        for field in section.keys().filter(|field| {
            !matches!(
                field.as_str(),
                "context" | "use_key_equivalents" | "use_builtin_defaults" | "unbind" | "bindings"
            )
        }) {
            self.diagnostic(
                source_index,
                Some(field),
                "unrecognized field; the rest of the section was loaded",
            );
        }

        let unbind = self.parse_entries(source_index, section.get("unbind"), true);
        let bindings = self.parse_entries(source_index, section.get("bindings"), false);
        self.sections.push(KeymapSection {
            context,
            use_key_equivalents,
            use_builtin_defaults,
            unbind,
            bindings,
            source_index,
        });
    }

    fn parse_entries(
        &mut self,
        source_index: usize,
        value: Option<&Value>,
        unbind: bool,
    ) -> Vec<KeymapEntry> {
        let field = if unbind { "unbind" } else { "bindings" };
        let Some(value) = value else {
            return Vec::new();
        };
        let Value::Object(entries) = value else {
            self.diagnostic(source_index, Some(field), "expected an object");
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|(keystrokes, value)| match KeymapAction::parse(value) {
                Ok(KeymapAction::None) if unbind => {
                    self.diagnostic(
                        source_index,
                        Some(format!("{field}.{keystrokes}")),
                        "an unbind target must name an action",
                    );
                    None
                }
                Ok(action) => Some(KeymapEntry {
                    keystrokes: keystrokes.clone(),
                    action,
                }),
                Err(message) => {
                    self.diagnostic(source_index, Some(format!("{field}.{keystrokes}")), message);
                    None
                }
            })
            .collect()
    }

    fn diagnostic(
        &mut self,
        section: usize,
        field: Option<impl Into<String>>,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(KeymapDiagnostic {
            section: Some(section),
            field: field.map(Into::into),
            message: message.into(),
        });
    }
}

#[derive(Debug, Error)]
pub enum KeymapParseError {
    #[error("unterminated block comment in keymap.json")]
    UnterminatedBlockComment,
    #[error("keymap.json must contain a top-level array of sections")]
    RootMustBeArray,
    #[error("failed to parse keymap.json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapBindingTarget {
    pub context: KeymapContext,
    pub keystrokes: String,
    pub action: KeymapAction,
    pub kind: KeymapBindingKind,
}

impl KeymapBindingTarget {
    pub fn binding(
        context: KeymapContext,
        keystrokes: impl Into<String>,
        action: KeymapAction,
    ) -> Self {
        Self {
            context,
            keystrokes: keystrokes.into(),
            action,
            kind: KeymapBindingKind::Binding,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapBindingSource {
    User,
    #[default]
    BuiltIn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapEdit {
    Add {
        binding: KeymapBindingTarget,
    },
    Replace {
        target: KeymapBindingTarget,
        replacement: KeymapBindingTarget,
        target_source: KeymapBindingSource,
    },
    Remove {
        target: KeymapBindingTarget,
        target_source: KeymapBindingSource,
    },
    ResetContext {
        context: KeymapContext,
    },
    SetBuiltInDefaults {
        context: KeymapContext,
        enabled: bool,
    },
}

impl KeymapEdit {
    #[must_use]
    pub const fn add(binding: KeymapBindingTarget) -> Self {
        Self::Add { binding }
    }

    #[must_use]
    pub const fn replace(
        target: KeymapBindingTarget,
        replacement: KeymapBindingTarget,
        target_source: KeymapBindingSource,
    ) -> Self {
        Self::Replace {
            target,
            replacement,
            target_source,
        }
    }

    #[must_use]
    pub const fn remove(target: KeymapBindingTarget, target_source: KeymapBindingSource) -> Self {
        Self::Remove {
            target,
            target_source,
        }
    }

    #[must_use]
    pub const fn reset_context(context: KeymapContext) -> Self {
        Self::ResetContext { context }
    }

    #[must_use]
    pub const fn set_builtin_defaults(context: KeymapContext, enabled: bool) -> Self {
        Self::SetBuiltInDefaults { context, enabled }
    }
}

#[derive(Debug, Error)]
pub enum KeymapEditError {
    #[error(transparent)]
    Parse(#[from] KeymapParseError),
    #[error("the user keybinding was not found in keymap.json")]
    BindingNotFound,
    #[error("an unbind target must name an action")]
    MissingUnbindAction,
    #[error("could not locate section {0} in keymap.json")]
    MissingSection(usize),
    #[error("failed to format keymap.json: {0}")]
    Format(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeymapWriteOutcome {
    durability_warning: Option<String>,
}

impl KeymapWriteOutcome {
    #[must_use]
    pub fn durability_warning(&self) -> Option<&str> {
        self.durability_warning.as_deref()
    }
}

#[derive(Debug, Error)]
#[error("failed to write keymap file {path}: {message}")]
pub struct KeymapWriteError {
    path: PathBuf,
    message: String,
}

///
/// # Errors
/// Returns an error for invalid JSONC, a missing section, or an invalid edit.
pub fn update_keymap_jsonc(contents: &str, edit: &KeymapEdit) -> Result<String, KeymapEditError> {
    let contents = if contents.trim().is_empty() {
        INITIAL_KEYMAP_CONTENT
    } else {
        contents
    };
    let file = KeymapFile::parse(contents)?;
    match edit {
        KeymapEdit::Add { binding } => append_binding(contents, binding),
        KeymapEdit::Replace {
            target,
            replacement,
            target_source: KeymapBindingSource::BuiltIn,
        } => {
            let updated = if target.keystrokes != replacement.keystrokes
                || target.context != replacement.context
            {
                append_binding(contents, &as_unbind(target)?)?
            } else {
                contents.to_owned()
            };
            append_binding(&updated, replacement)
        }
        KeymapEdit::Replace {
            target,
            replacement,
            target_source: KeymapBindingSource::User,
        } => {
            let Some((section, entry_index)) = find_binding(&file, target) else {
                return append_binding(contents, replacement);
            };
            if target.context == replacement.context && target.kind == replacement.kind {
                let mut section = section.clone();
                let source_index = section.source_index;
                *section
                    .entries_mut(target.kind)
                    .get_mut(entry_index)
                    .ok_or(KeymapEditError::MissingSection(source_index))? = KeymapEntry {
                    keystrokes: replacement.keystrokes.clone(),
                    action: replacement.action.clone(),
                };
                replace_section(contents, section.source_index, &section)
            } else {
                let updated = remove_user_binding(contents, section, entry_index, target.kind)?;
                append_binding(&updated, replacement)
            }
        }
        KeymapEdit::Remove {
            target,
            target_source: KeymapBindingSource::BuiltIn,
        } => append_binding(contents, &as_unbind(target)?),
        KeymapEdit::Remove {
            target,
            target_source: KeymapBindingSource::User,
        } => {
            let Some((section, entry_index)) = find_binding(&file, target) else {
                return Err(KeymapEditError::BindingNotFound);
            };
            remove_user_binding(contents, section, entry_index, target.kind)
        }
        KeymapEdit::ResetContext { context } => reset_context(contents, &file, context),
        KeymapEdit::SetBuiltInDefaults { context, enabled } => {
            set_builtin_defaults(contents, &file, context.clone(), *enabled)
        }
    }
}

///
/// # Errors
/// Returns an error if the keymap cannot be read, edited, locked, or replaced.
pub fn write_keymap_edit(
    path: impl AsRef<Path>,
    edit: &KeymapEdit,
) -> Result<KeymapWriteOutcome, KeymapWriteError> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .ok_or_else(|| write_error(path, "target has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|error| write_error(path, format!("create config directory: {error}")))?;
    let target = WriteTarget::resolve(path).map_err(|error| {
        let message = match error {
            ResolveTargetError::SymlinkCycle => "symlink cycle detected".to_owned(),
            ResolveTargetError::Io(error) => error.to_string(),
        };
        write_error(path, format!("resolve target: {message}"))
    })?;
    let target = target
        .lock()
        .map_err(|error| write_error(path, format!("claim writer lease: {error}")))?;
    // Read under the same lease as replacement so edits from other windows are retained.
    let contents = match fs::read_to_string(target.path()) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => INITIAL_KEYMAP_CONTENT.to_owned(),
        Err(error) => return Err(write_error(path, format!("read: {error}"))),
    };
    let updated = update_keymap_jsonc(&contents, edit)
        .map_err(|error| write_error(path, error.to_string()))?;
    let outcome = target
        .replace(updated.as_bytes(), NewFileMode::Private)
        .map_err(|error| {
            let phase = error.phase();
            let error = error.into_io();
            write_error(path, format!("{phase}: {error}"))
        })?;
    drop(target);
    Ok(KeymapWriteOutcome {
        durability_warning: match outcome {
            CommitOutcome::Confirmed => None,
            CommitOutcome::CommittedWithDurabilityWarning(error) => Some(error.to_string()),
        },
    })
}

fn write_error(path: &Path, message: impl Into<String>) -> KeymapWriteError {
    KeymapWriteError {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn as_unbind(target: &KeymapBindingTarget) -> Result<KeymapBindingTarget, KeymapEditError> {
    if target.action == KeymapAction::None {
        return Err(KeymapEditError::MissingUnbindAction);
    }
    Ok(KeymapBindingTarget {
        context: target.context.clone(),
        keystrokes: target.keystrokes.clone(),
        action: target.action.clone(),
        kind: KeymapBindingKind::Unbind,
    })
}

fn find_binding<'a>(
    file: &'a KeymapFile,
    target: &KeymapBindingTarget,
) -> Option<(&'a KeymapSection, usize)> {
    file.sections().find_map(|section| {
        (section.context == target.context).then(|| {
            section
                .entries(target.kind)
                .position(|entry| {
                    entry.keystrokes == target.keystrokes && entry.action == target.action
                })
                .map(|index| (section, index))
        })?
    })
}

fn remove_user_binding(
    contents: &str,
    section: &KeymapSection,
    entry_index: usize,
    kind: KeymapBindingKind,
) -> Result<String, KeymapEditError> {
    if section.binding_count() == 1
        && !section.use_key_equivalents
        && section.use_builtin_defaults.is_none()
    {
        return remove_section(contents, section.source_index);
    }
    let mut replacement = section.clone();
    replacement.entries_mut(kind).remove(entry_index);
    replace_section(contents, section.source_index, &replacement)
}

fn reset_context(
    contents: &str,
    file: &KeymapFile,
    context: &KeymapContext,
) -> Result<String, KeymapEditError> {
    let sections = file
        .sections()
        .filter(|section| &section.context == context && section.binding_count() > 0)
        .collect::<Vec<_>>();
    let mut updated = contents.to_owned();
    for section in sections.into_iter().rev() {
        if section.use_key_equivalents || section.use_builtin_defaults.is_some() {
            let mut replacement = section.clone();
            replacement.unbind.clear();
            replacement.bindings.clear();
            updated = replace_section(&updated, section.source_index, &replacement)?;
        } else {
            updated = remove_section(&updated, section.source_index)?;
        }
    }
    Ok(updated)
}

fn set_builtin_defaults(
    contents: &str,
    file: &KeymapFile,
    context: KeymapContext,
    enabled: bool,
) -> Result<String, KeymapEditError> {
    if let Some(section) = file
        .sections()
        .rev()
        .find(|section| section.context == context)
    {
        let mut replacement = section.clone();
        replacement.use_builtin_defaults = Some(enabled);
        return replace_section(contents, section.source_index, &replacement);
    }

    append_section(
        contents,
        &KeymapSection {
            context,
            use_key_equivalents: false,
            use_builtin_defaults: Some(enabled),
            unbind: Vec::new(),
            bindings: Vec::new(),
            source_index: usize::MAX,
        },
    )
}

fn append_binding(
    contents: &str,
    binding: &KeymapBindingTarget,
) -> Result<String, KeymapEditError> {
    if binding.kind == KeymapBindingKind::Unbind && binding.action == KeymapAction::None {
        return Err(KeymapEditError::MissingUnbindAction);
    }
    let mut section = KeymapSection {
        context: binding.context.clone(),
        use_key_equivalents: false,
        use_builtin_defaults: None,
        unbind: Vec::new(),
        bindings: Vec::new(),
        source_index: usize::MAX,
    };
    section.entries_mut(binding.kind).push(KeymapEntry {
        keystrokes: binding.keystrokes.clone(),
        action: binding.action.clone(),
    });
    append_section(contents, &section)
}

fn append_section(contents: &str, section: &KeymapSection) -> Result<String, KeymapEditError> {
    let comments_removed = strip_jsonc_comments(contents)?;
    let sanitized = remove_trailing_commas(&comments_removed);
    let (open, close) = root_array_bounds(&sanitized)?;
    let section = pretty_json(&section.to_json())?;
    let indented = indent_lines(&section, 2);
    let has_elements = sanitized
        .get(open.saturating_add(1)..close)
        .ok_or(KeymapParseError::RootMustBeArray)?
        .bytes()
        .any(|byte| !byte.is_ascii_whitespace());
    let has_trailing_comma = comments_removed
        .get(..close)
        .ok_or(KeymapParseError::RootMustBeArray)?
        .bytes()
        .rev()
        .find(|byte| !byte.is_ascii_whitespace())
        == Some(b',');
    let insertion = if has_elements {
        format!("{}\n{indented}", if has_trailing_comma { "" } else { "," })
    } else {
        format!("\n{indented}\n")
    };
    let mut updated = contents.to_owned();
    updated.insert_str(close, &insertion);
    Ok(updated)
}

fn replace_section(
    contents: &str,
    source_index: usize,
    section: &KeymapSection,
) -> Result<String, KeymapEditError> {
    let sanitized = sanitize_jsonc(contents)?;
    let elements = root_array_elements(&sanitized)?;
    let range = elements
        .get(source_index)
        .cloned()
        .ok_or(KeymapEditError::MissingSection(source_index))?;
    let column = line_column(contents, range.start);
    let replacement = indent_lines(&pretty_json(&section.to_json())?, column);
    let mut updated = contents.to_owned();
    updated.replace_range(range, &replacement);
    Ok(updated)
}

fn remove_section(contents: &str, source_index: usize) -> Result<String, KeymapEditError> {
    let sanitized = sanitize_jsonc(contents)?;
    let elements = root_array_elements(&sanitized)?;
    let Some(element) = elements.get(source_index) else {
        return Err(KeymapEditError::MissingSection(source_index));
    };
    let range = if elements.len() == 1 {
        element.clone()
    } else if let Some(next) = elements.get(source_index.saturating_add(1)) {
        element.start..next.start
    } else {
        elements
            .get(source_index.saturating_sub(1))
            .ok_or(KeymapEditError::MissingSection(source_index))?
            .end..element.end
    };
    let mut updated = contents.to_owned();
    updated.replace_range(range, "");
    Ok(updated)
}

fn pretty_json(value: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value)
}

fn indent_lines(value: &str, width: usize) -> String {
    let indent = " ".repeat(width);
    value
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                line.to_owned()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_column(contents: &str, offset: usize) -> usize {
    contents
        .get(..offset)
        .unwrap_or_default()
        .rsplit_once('\n')
        .map_or(offset, |(_, line)| line.len())
}

fn sanitize_jsonc(contents: &str) -> Result<String, KeymapParseError> {
    strip_jsonc_comments(contents).map(|contents| remove_trailing_commas(&contents))
}

fn strip_jsonc_comments(contents: &str) -> Result<String, KeymapParseError> {
    let mut output = String::with_capacity(contents.len());
    let mut characters = contents.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if in_string {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if character == '"' {
            in_string = true;
            output.push(character);
            continue;
        }
        if character != '/' {
            output.push(character);
            continue;
        }
        match characters.peek().copied() {
            Some('/') => {
                characters.next();
                output.push_str("  ");
                for character in characters.by_ref() {
                    if character == '\n' {
                        output.push('\n');
                        break;
                    }
                    output.extend(std::iter::repeat_n(' ', character.len_utf8()));
                }
            }
            Some('*') => {
                characters.next();
                output.push_str("  ");
                let mut closed = false;
                while let Some(character) = characters.next() {
                    if character == '*' && characters.peek() == Some(&'/') {
                        characters.next();
                        output.push_str("  ");
                        closed = true;
                        break;
                    }
                    if character == '\n' {
                        output.push('\n');
                    } else {
                        output.extend(std::iter::repeat_n(' ', character.len_utf8()));
                    }
                }
                if !closed {
                    return Err(KeymapParseError::UnterminatedBlockComment);
                }
            }
            _ => output.push(character),
        }
    }
    Ok(output)
}

fn remove_trailing_commas(contents: &str) -> String {
    let mut output = String::with_capacity(contents.len());
    let mut characters = contents.chars();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if in_string {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if character == '"' {
            in_string = true;
        } else if character == ','
            && matches!(
                characters
                    .clone()
                    .find(|character| !character.is_ascii_whitespace()),
                Some(']' | '}')
            )
        {
            output.push(' ');
            continue;
        }
        output.push(character);
    }
    output
}

fn root_array_bounds(contents: &str) -> Result<(usize, usize), KeymapEditError> {
    let open = contents
        .bytes()
        .position(|byte| !byte.is_ascii_whitespace())
        .filter(|index| contents.as_bytes().get(*index) == Some(&b'['))
        .ok_or(KeymapParseError::RootMustBeArray)?;
    let close = matching_delimiter(contents.as_bytes(), open, b'[', b']')
        .ok_or(KeymapParseError::RootMustBeArray)?;
    Ok((open, close))
}

fn root_array_elements(contents: &str) -> Result<Vec<Range<usize>>, KeymapEditError> {
    let (open, close) = root_array_bounds(contents)?;
    let bytes = contents.as_bytes();
    let mut elements = Vec::new();
    let mut start = open.saturating_add(1);
    let mut object_depth = 0_usize;
    let mut array_depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate().take(close).skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' => object_depth = object_depth.saturating_add(1),
                b'}' => object_depth = object_depth.saturating_sub(1),
                b'[' => array_depth = array_depth.saturating_add(1),
                b']' => array_depth = array_depth.saturating_sub(1),
                b',' if object_depth == 0 && array_depth == 0 => {
                    if let Some(range) = trimmed_range(bytes, start..index) {
                        elements.push(range);
                    }
                    start = index.saturating_add(1);
                }
                _ => {}
            }
        }
    }
    if let Some(range) = trimmed_range(bytes, start..close) {
        elements.push(range);
    }
    Ok(elements)
}

fn trimmed_range(bytes: &[u8], range: Range<usize>) -> Option<Range<usize>> {
    let bytes = bytes.get(range.clone())?;
    let first = bytes.iter().position(|byte| !byte.is_ascii_whitespace())?;
    let last = bytes.iter().rposition(|byte| !byte.is_ascii_whitespace())?;
    Some(range.start.checked_add(first)?..range.start.checked_add(last)?.checked_add(1)?)
}

fn matching_delimiter(bytes: &[u8], open: usize, left: u8, right: u8) -> Option<usize> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate().skip(open) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            byte if byte == left => depth = depth.checked_add(1)?,
            byte if byte == right => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}
