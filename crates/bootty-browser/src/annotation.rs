use crate::normalize_address;
use serde::Deserialize;

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
