//! Typed native input targets one owned page; coordinates are CSS pixels inside its viewport.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMouseButton {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserModifier {
    Shift,
    Control,
    Alt,
    Super,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserInput {
    Click {
        x: u32,
        y: u32,
        button: BrowserMouseButton,
    },
    Scroll {
        x: u32,
        y: u32,
        delta_x: i32,
        delta_y: i32,
    },
    Type {
        text: String,
    },
    Key {
        key: String,
        modifiers: Vec<BrowserModifier>,
    },
}

impl BrowserInput {
    /// # Errors
    /// Rejects unbounded text, unknown keys and duplicate modifiers before platform dispatch.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Type { text } if text.is_empty() || text.len() > 4096 || text.contains('\0') => {
                Err("Browser text must contain 1–4096 bytes without NUL".into())
            }
            Self::Scroll {
                delta_x, delta_y, ..
            } if delta_x.unsigned_abs() > 4096 || delta_y.unsigned_abs() > 4096 => {
                Err("Browser scroll deltas must be within 4096 pixels".into())
            }
            Self::Key { key, modifiers }
                if key_code(key).is_none()
                    || modifiers.len() > 4
                    || modifiers.iter().enumerate().any(|(index, modifier)| {
                        modifiers
                            .get(..index)
                            .is_some_and(|previous| previous.contains(modifier))
                    }) =>
            {
                Err("Browser key or modifiers are invalid".into())
            }
            _ => Ok(()),
        }
    }
}

pub fn key_code(key: &str) -> Option<(u16, &str)> {
    Some(match key {
        "enter" => (36, "\r"),
        "tab" => (48, "\t"),
        "escape" => (53, "\u{1b}"),
        "backspace" => (51, "\u{7f}"),
        "delete" => (117, "\u{f728}"),
        "left" => (123, "\u{f702}"),
        "right" => (124, "\u{f703}"),
        "up" => (126, "\u{f700}"),
        "down" => (125, "\u{f701}"),
        "home" => (115, "\u{f729}"),
        "end" => (119, "\u{f72b}"),
        "pageup" => (116, "\u{f72c}"),
        "pagedown" => (121, "\u{f72d}"),
        "space" => (49, " "),
        _ => {
            if key.chars().any(char::is_whitespace) {
                return None;
            }
            // AppKit's ANSI virtual-key positions, including the unused ISO slot at 10.
            let index = "asdfhgzxcv bqweryt123465=97-80]ou[ip\rlj'k;\\,/nm."
                .chars()
                .position(|character| key.len() == 1 && key.starts_with(character))?;
            (u16::try_from(index).ok()?, key)
        }
    })
}
