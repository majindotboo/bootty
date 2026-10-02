use crate::normalize_address;
use serde::{Deserialize, Serialize};

/// An explicit editor action; page content never supplies the exported element details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationAction {
    Cancel,
    Copy(String),
    Paste(String),
}

#[derive(Deserialize)]
#[serde(
    tag = "action",
    content = "value",
    rename_all = "lowercase",
    deny_unknown_fields
)]
enum AnnotationRequest {
    Cancel,
    Copy {
        comment: String,
    },
    Paste {
        comment: String,
    },
    Key {
        key: String,
        command: bool,
        shift: bool,
        comment: String,
    },
}

impl AnnotationAction {
    /// Accepts bounded explicit actions, including Escape and the command-Enter shortcut.
    #[must_use]
    pub fn parse(message: &str) -> Option<Self> {
        if message.len() > 32_768 {
            return None;
        }
        let action = match serde_json::from_str::<AnnotationRequest>(message).ok()? {
            AnnotationRequest::Cancel => Self::Cancel,
            AnnotationRequest::Copy { comment } => Self::Copy(comment),
            AnnotationRequest::Paste { comment } => Self::Paste(comment),
            AnnotationRequest::Key {
                key,
                command,
                shift,
                comment,
            } => match key.as_str() {
                "Escape" => Self::Cancel,
                "Enter" if command && !shift => Self::Paste(comment),
                _ => return None,
            },
        };
        if let Self::Copy(comment) | Self::Paste(comment) = &action
            && (comment.trim().is_empty() || comment.len() > 4096)
        {
            return None;
        }
        Some(action)
    }
}

/// Host theme tokens for the native page's floating editor.
#[derive(Serialize)]
pub struct AnnotationTheme {
    pub background: u32,
    pub foreground: u32,
    pub muted: u32,
    pub border: u32,
    pub primary: u32,
    pub primary_foreground: u32,
    pub font_size: f32,
    pub radius: f32,
}

/// Bounded page-controlled details; the user reviews these before sharing feedback.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserElement {
    pub url: String,
    pub selector: String,
    pub text: String,
    pub tag: String,
    pub bounds: [f64; 4],
}

impl BrowserElement {
    /// Parse a native page selection without accepting unbounded or unsafe page data.
    #[must_use]
    pub fn parse(message: &str) -> Option<Self> {
        if message.len() > 8192 {
            return None;
        }
        let element: Self = serde_json::from_str(message).ok()?;
        if normalize_address(&element.url).ok()?.starts_with("about:")
            || element.url.len() > 4096
            || element.selector.len() > 512
            || element.text.len() > 2048
            || element.tag.len() > 48
            || element.selector.is_empty()
            || element.tag.is_empty()
            || element
                .bounds
                .iter()
                .any(|value| !value.is_finite() || value.abs() > 10_000_000.0)
            || element.bounds.iter().skip(2).any(|value| *value < 0.0)
        {
            return None;
        }
        Some(element)
    }

    #[must_use]
    pub fn feedback(&self, comment: &str) -> String {
        // JSON quotes preserve page text as data, including markup and control characters.
        let quoted = |value: &str| serde_json::Value::String(value.to_owned()).to_string();
        format!(
            "Browser feedback\nPage: {}\nElement: {}\nSelector (page data): {}\nText (page data): {}\nBounds: {:?}\n\nRequested change:\n{}",
            quoted(&self.url),
            quoted(&self.tag),
            quoted(&self.selector),
            quoted(&self.text),
            self.bounds,
            comment.trim()
        )
    }
}
