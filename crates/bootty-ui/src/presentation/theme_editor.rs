use std::sync::mpsc::{Receiver, TryRecvError};

use bootty_config::config::{AppearanceVariant, parse_theme_source, theme_file::ThemeFile};
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use toml_edit::{DocumentMut, value};

use crate::gpui::{
    DialogAction, DialogField, DialogFieldKind, DialogIntent, DialogRow, DialogSpec,
};

const COLORS: [&str; 13] = [
    "background",
    "foreground",
    "cursor",
    "cursor-text",
    "pointer-foreground",
    "pointer-background",
    "tektronix-foreground",
    "tektronix-background",
    "highlight-background",
    "tektronix-cursor",
    "highlight-foreground",
    "selection-background",
    "selection-foreground",
];

pub struct ThemeEditorDialog {
    name: String,
    load_name: String,
    import_path: String,
    document: DocumentMut,
    loaded: Option<(String, String)>,
    appearance: String,
    error: Option<String>,
    notice: Option<String>,
    pending: Option<(String, Receiver<CommandOutcome>, CommandCancellation)>,
}

pub enum ThemeEditorEvent {
    Close,
    Submit(CommandInvocation),
}

impl ThemeEditorDialog {
    #[must_use]
    pub fn new(name: String, appearance: AppearanceVariant) -> Self {
        Self {
            name: format!("{name} Copy"),
            load_name: name,
            import_path: String::new(),
            document: DocumentMut::new(),
            loaded: None,
            appearance: match appearance {
                AppearanceVariant::Light => "light",
                AppearanceVariant::Dark => "dark",
            }
            .to_owned(),
            error: None,
            notice: None,
            pending: None,
        }
    }

    fn command(action: &str, arguments: Vec<String>) -> ThemeEditorEvent {
        let mut invocation = CommandInvocation::from_action(action, Caller::Internal);
        invocation.arguments = arguments;
        ThemeEditorEvent::Submit(invocation)
    }

    #[must_use]
    pub fn load(&self) -> ThemeEditorEvent {
        Self::command("theme.read", vec![self.load_name.clone()])
    }

    pub fn spec(&self) -> DialogSpec {
        let mut spec = DialogSpec::prompt(
            "theme-editor",
            "Edit Theme",
            &self.name,
            "New theme name",
            DialogAction::new("save"),
        );
        spec.text_label = Some("Save as (change the name to duplicate)".to_owned());
        spec.busy = self.pending.is_some();
        if let Some(save) = spec.rows.first_mut() {
            "Save and Apply".clone_into(&mut save.label);
            save.detail.clone_from(&self.error);
            save.enabled = !self.document.is_empty();
        }
        for (id, label) in [
            ("load", "Load"),
            ("import", "Import"),
            ("preview", "Preview"),
        ] {
            spec.rows
                .push(DialogRow::action(id, label, DialogAction::new(id)));
        }
        let mut field = |id: &str, label: &str, text: String, kind| {
            spec.fields.push(DialogField {
                id: id.to_owned(),
                label: label.to_owned(),
                value: text,
                placeholder: String::new(),
                kind,
            });
        };
        field(
            "load-name",
            "Theme to load",
            self.load_name.clone(),
            DialogFieldKind::Text,
        );
        field(
            "import-path",
            "Import TOML or iTerm2 file (absolute local path)",
            self.import_path.clone(),
            DialogFieldKind::Text,
        );
        for key in ["source", "license"] {
            field(
                key,
                key,
                self.document
                    .get("metadata")
                    .and_then(|item| item.get(key))
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                DialogFieldKind::Text,
            );
        }
        for key in COLORS {
            field(
                key,
                key,
                self.document
                    .get("colors")
                    .and_then(|item| item.get(key))
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                DialogFieldKind::Color,
            );
        }
        if let Some(palette) = self
            .document
            .get("colors")
            .and_then(|item| item.get("palette"))
            .and_then(toml_edit::Item::as_array)
        {
            for (index, color) in palette.iter().enumerate() {
                field(
                    &format!("palette-{index}"),
                    &format!("ANSI {index}"),
                    color.as_str().unwrap_or_default().to_owned(),
                    DialogFieldKind::Color,
                );
            }
        }
        spec.footer = Some(self.notice.clone().unwrap_or_else(|| "Preview changes this window until you close the editor. Built-in themes are saved as copies. Source and license are preserved.".to_owned()));
        spec
    }

    fn set_document_field(&mut self, table: &str, key: &str, text: &str) -> bool {
        let item = self
            .document
            .entry(table)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let Some(table) = item.as_table_like_mut() else {
            self.error = Some(format!("Theme {table} must be a table"));
            return false;
        };
        table.insert(key, value(text));
        true
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<ThemeEditorEvent> {
        match intent {
            DialogIntent::Dismiss { dialog } if dialog.0 == "theme-editor" => {
                return Some(ThemeEditorEvent::Close);
            }
            DialogIntent::TextChanged { dialog, value }
                if dialog.0 == "theme-editor" && self.pending.is_none() =>
            {
                self.name.clone_from(value);
            }
            DialogIntent::FieldChanged {
                dialog,
                field,
                value: text,
            } if dialog.0 == "theme-editor" && self.pending.is_none() => match field.as_str() {
                "load-name" => self.load_name.clone_from(text),
                "import-path" => self.import_path.clone_from(text),
                "source" | "license" => {
                    if !self.set_document_field("metadata", field, text) {
                        return None;
                    }
                }
                key if COLORS.contains(&key) => {
                    if text.is_empty() {
                        if let Some(colors) = self
                            .document
                            .get_mut("colors")
                            .and_then(toml_edit::Item::as_table_mut)
                        {
                            colors.remove(key);
                        }
                    } else if !self.set_document_field("colors", key, text) {
                        return None;
                    }
                }
                key => {
                    if let Some(index) = key
                        .strip_prefix("palette-")
                        .and_then(|index| index.parse::<usize>().ok())
                        && let Some(palette) = self
                            .document
                            .get_mut("colors")
                            .and_then(|item| item.get_mut("palette"))
                            .and_then(toml_edit::Item::as_array_mut)
                        && index < palette.len()
                        && !text.is_empty()
                    {
                        palette.replace(index, text);
                    }
                }
            },
            DialogIntent::Activate { dialog, action, .. }
                if dialog.0 == "theme-editor" && self.pending.is_none() =>
            {
                return match action.0.as_str() {
                    "load" => Some(self.load()),
                    "import" => {
                        if std::path::Path::new(&self.import_path).is_absolute() {
                            Some(Self::command(
                                "theme.import",
                                vec![self.import_path.clone()],
                            ))
                        } else {
                            self.error = Some("Choose an absolute local file path".to_owned());
                            None
                        }
                    }
                    "preview" | "save" => {
                        let name = self.name.clone();
                        if !self.set_document_field("metadata", "name", &name) {
                            return None;
                        }
                        let source = self.document.to_string();
                        if let Err(error) = parse_theme_source(&source, &self.name) {
                            self.error = Some(error.to_string());
                            return None;
                        }
                        if action.0 == "preview" {
                            Some(Self::command(
                                "theme.preview",
                                vec![source, self.appearance.clone()],
                            ))
                        } else {
                            let mut args = vec![self.name.clone(), source];
                            if let Some((name, revision)) = &self.loaded
                                && name == &self.name
                            {
                                args.push(revision.clone());
                            }
                            Some(Self::command("theme.save", args))
                        }
                    }
                    _ => None,
                };
            }
            _ => return None,
        }
        self.error = None;
        self.notice = None;
        None
    }

    pub fn started(
        &mut self,
        action: String,
        receiver: Receiver<CommandOutcome>,
        cancellation: CommandCancellation,
    ) {
        self.pending = Some((action, receiver, cancellation));
        self.error = None;
    }
    pub fn failed(&mut self, error: String) {
        self.error = Some(error);
    }
    pub fn poll(&mut self) -> Option<ThemeEditorEvent> {
        let (action, receiver, _) = self.pending.as_ref()?;
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => CommandOutcome::Unavailable {
                message: "Theme command stopped".to_owned(),
            },
        };
        let action = action.clone();
        self.pending = None;
        match outcome {
            CommandOutcome::Success { value, .. } => match action.as_str() {
                "theme.read" | "theme.import" | "theme.save" => {
                    let file = match serde_json::from_value::<ThemeFile>(value) {
                        Ok(file) => file,
                        Err(error) => {
                            self.error = Some(error.to_string());
                            return None;
                        }
                    };
                    self.document = match file.source.parse() {
                        Ok(document) => document,
                        Err(error) => {
                            self.error = Some(format!("{error}"));
                            return None;
                        }
                    };
                    self.loaded = file.revision.map(|revision| (file.name.clone(), revision));
                    self.name = if self.loaded.is_none() && action == "theme.read" {
                        format!("{} Copy", file.name)
                    } else {
                        file.name
                    };
                    if action == "theme.save" {
                        return Some(Self::command(
                            "theme.apply",
                            vec![self.name.clone(), self.appearance.clone()],
                        ));
                    }
                    self.notice = None;
                }
                "theme.apply" => self.notice = Some(format!("Saved and applied {}", self.name)),
                "theme.preview" => {
                    self.notice = Some(
                        "Preview active. Close to restore, or Save and Apply to keep it."
                            .to_owned(),
                    );
                }
                _ => {}
            },
            outcome => self.error = crate::commands::command_outcome_message(&outcome),
        }
        None
    }
}

impl Drop for ThemeEditorDialog {
    fn drop(&mut self) {
        if let Some((_, _, cancellation)) = &self.pending {
            let _ = cancellation.cancel();
        }
    }
}
