use std::{fmt, str::FromStr};

use thiserror::Error;

use crate::{KeymapModifierSide, KeymapModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModifierSpec {
    modifier: Modifier,
    side: KeymapModifierSide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemapEntry {
    from: ModifierSpec,
    to: ModifierSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Command,
}

/// The accepted modifier remap policy, independent of terminal or windowing types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModifierRemapSet {
    entries: Vec<RemapEntry>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModifierRemapParseError {
    #[error("missing modifier remap assignment")]
    MissingAssignment,
    #[error("invalid modifier remap modifier {0:?}")]
    InvalidModifier(String),
}

impl ModifierRemapSet {
    ///
    /// # Errors
    /// Rejects missing assignments or invalid modifier names without adding remaps.
    pub fn parse(&mut self, input: &str) -> Result<(), ModifierRemapParseError> {
        let (from, to) = input
            .split_once('=')
            .ok_or(ModifierRemapParseError::MissingAssignment)?;
        let from = ParsedModifier::from_str(from)?;
        let to = ParsedModifier::from_str(to)?.spec_or_default_left();

        match from.side {
            Some(side) => self.entries.push(RemapEntry {
                from: ModifierSpec {
                    modifier: from.modifier,
                    side,
                },
                to,
            }),
            None => {
                for side in [KeymapModifierSide::Left, KeymapModifierSide::Right] {
                    self.entries.push(RemapEntry {
                        from: ModifierSpec {
                            modifier: from.modifier,
                            side,
                        },
                        to,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn finalize(&mut self) {
        self.entries
            .sort_by_key(|entry| entry.from.side != KeymapModifierSide::Right);
    }

    /// Apply the first matching remap while preserving the other modifier state.
    #[must_use]
    pub fn apply(&self, modifiers: KeymapModifiers) -> KeymapModifiers {
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.from.matches(modifiers))
        else {
            return modifiers;
        };
        let mut remapped = modifiers;
        entry.from.unset(&mut remapped);
        entry.to.set(&mut remapped);
        remapped
    }

    #[must_use]
    pub fn formatted_entries(&self) -> Vec<String> {
        if self.entries.is_empty() {
            return vec![String::new()];
        }
        self.entries
            .iter()
            .map(|entry| format!("{}={}", entry.from, entry.to))
            .collect()
    }
}

impl ModifierSpec {
    fn matches(self, modifiers: KeymapModifiers) -> bool {
        let (pressed, side) = match self.modifier {
            Modifier::Shift => (modifiers.shift, modifiers.shift_side),
            Modifier::Ctrl => (modifiers.ctrl, modifiers.ctrl_side),
            Modifier::Alt => (modifiers.alt, modifiers.alt_side),
            Modifier::Command => (modifiers.command, modifiers.command_side),
        };
        pressed
            && ((side != Some(KeymapModifierSide::Right))
                == (self.side == KeymapModifierSide::Left))
    }

    const fn set(self, modifiers: &mut KeymapModifiers) {
        let side = self.side;
        match self.modifier {
            Modifier::Shift => {
                modifiers.shift = true;
                modifiers.shift_side = Some(side);
            }
            Modifier::Ctrl => {
                modifiers.ctrl = true;
                modifiers.ctrl_side = Some(side);
            }
            Modifier::Alt => {
                modifiers.alt = true;
                modifiers.alt_side = Some(side);
            }
            Modifier::Command => {
                modifiers.command = true;
                modifiers.command_side = Some(side);
            }
        }
    }

    const fn unset(self, modifiers: &mut KeymapModifiers) {
        match self.modifier {
            Modifier::Shift => {
                modifiers.shift = false;
                modifiers.shift_side = None;
            }
            Modifier::Ctrl => {
                modifiers.ctrl = false;
                modifiers.ctrl_side = None;
            }
            Modifier::Alt => {
                modifiers.alt = false;
                modifiers.alt_side = None;
            }
            Modifier::Command => {
                modifiers.command = false;
                modifiers.command_side = None;
            }
        }
    }
}

impl fmt::Display for ModifierSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = match self.side {
            KeymapModifierSide::Left => "left",
            KeymapModifierSide::Right => "right",
        };
        let modifier = match self.modifier {
            Modifier::Shift => "shift",
            Modifier::Ctrl => "ctrl",
            Modifier::Alt => "alt",
            Modifier::Command => "super",
        };
        write!(formatter, "{side}_{modifier}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedModifier {
    modifier: Modifier,
    side: Option<KeymapModifierSide>,
}

impl ParsedModifier {
    fn spec_or_default_left(self) -> ModifierSpec {
        ModifierSpec {
            modifier: self.modifier,
            side: self.side.unwrap_or(KeymapModifierSide::Left),
        }
    }
}

impl FromStr for ParsedModifier {
    type Err = ModifierRemapParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (side, modifier) = match input.split_once('_') {
            Some((side, modifier)) => (
                Some(match side {
                    "left" => KeymapModifierSide::Left,
                    "right" => KeymapModifierSide::Right,
                    _ => return Err(ModifierRemapParseError::InvalidModifier(input.to_owned())),
                }),
                modifier,
            ),
            None => (None, input),
        };
        let modifier = match modifier {
            "shift" => Modifier::Shift,
            "ctrl" | "control" => Modifier::Ctrl,
            "alt" | "opt" | "option" => Modifier::Alt,
            "super" | "cmd" | "command" => Modifier::Command,
            _ => return Err(ModifierRemapParseError::InvalidModifier(input.to_owned())),
        };
        Ok(Self { modifier, side })
    }
}
