use std::{
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Instant, SystemTime},
};

use thiserror::Error;

use crate::{
    config::{BackendKeybindConfig, BoottyConfig, MultiplexerBackendConfig, split_keybind_entry},
    config_reload::CONFIG_HOT_RELOAD_INTERVAL,
    keymap_file::{
        KeymapAction, KeymapBindingKind, KeymapContext, KeymapDiagnostic, KeymapEdit, KeymapFile,
        KeymapWriteOutcome, write_keymap_edit,
    },
};

pub use crate::keymap_file::KeymapBindingSource;

/// The side of a modifier key used by a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeymapModifierSide {
    Left,
    Right,
}

/// Modifier state used by keymap matching. Side constraints are optional so a binding can match
/// either a particular physical modifier or the aggregate modifier.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeymapModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub command: bool,
    pub shift_side: Option<KeymapModifierSide>,
    pub ctrl_side: Option<KeymapModifierSide>,
    pub alt_side: Option<KeymapModifierSide>,
    pub command_side: Option<KeymapModifierSide>,
}

impl KeymapModifiers {
    /// Remove all left/right constraints while retaining aggregate modifier state.
    #[must_use]
    pub const fn without_side_constraints(mut self) -> Self {
        self.shift_side = None;
        self.ctrl_side = None;
        self.alt_side = None;
        self.command_side = None;
        self
    }

    fn input_candidates(self) -> Vec<Self> {
        let mut candidates = vec![self];
        if self.shift_side.is_some() {
            push_without_side(&mut candidates, |mods| mods.shift_side = None);
        }
        if self.ctrl_side.is_some() {
            push_without_side(&mut candidates, |mods| mods.ctrl_side = None);
        }
        if self.alt_side.is_some() {
            push_without_side(&mut candidates, |mods| mods.alt_side = None);
        }
        if self.command_side.is_some() {
            push_without_side(&mut candidates, |mods| mods.command_side = None);
        }
        candidates
    }
}

fn push_without_side(
    candidates: &mut Vec<KeymapModifiers>,
    clear_side: impl Fn(&mut KeymapModifiers),
) {
    let existing_count = candidates.len();
    for index in 0..existing_count {
        let Some(mut candidate) = candidates.get(index).copied() else {
            break;
        };
        clear_side(&mut candidate);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
}

/// Physical keys that the keymap grammar accepts. Native keys outside this set are converted by
/// their adapter to text or a catch-all trigger when appropriate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeymapPhysicalKey {
    Backquote,
    Backslash,
    BracketLeft,
    BracketRight,
    Comma,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Minus,
    Period,
    Quote,
    Semicolon,
    Slash,
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
    Insert,
    Enter,
    Tab,
    Backspace,
    Escape,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

impl KeymapPhysicalKey {
    fn parse(input: &str) -> Result<Option<Self>, KeymapTriggerError> {
        macro_rules! keys {
            ($($canonical:literal | $alias:literal => $key:ident,)+) => {
                match input {
                    $($canonical | $alias => Ok(Some(Self::$key)),)+
                    _ if input.starts_with("Key") || input.starts_with("Digit") => {
                        Err(KeymapTriggerError::invalid_format())
                    }
                    _ => Ok(None),
                }
            };
        }
        keys! {
            "KeyA" | "key_a" => A,
            "KeyB" | "key_b" => B,
            "KeyC" | "key_c" => C,
            "KeyD" | "key_d" => D,
            "KeyE" | "key_e" => E,
            "KeyF" | "key_f" => F,
            "KeyG" | "key_g" => G,
            "KeyH" | "key_h" => H,
            "KeyI" | "key_i" => I,
            "KeyJ" | "key_j" => J,
            "KeyK" | "key_k" => K,
            "KeyL" | "key_l" => L,
            "KeyM" | "key_m" => M,
            "KeyN" | "key_n" => N,
            "KeyO" | "key_o" => O,
            "KeyP" | "key_p" => P,
            "KeyQ" | "key_q" => Q,
            "KeyR" | "key_r" => R,
            "KeyS" | "key_s" => S,
            "KeyT" | "key_t" => T,
            "KeyU" | "key_u" => U,
            "KeyV" | "key_v" => V,
            "KeyW" | "key_w" => W,
            "KeyX" | "key_x" => X,
            "KeyY" | "key_y" => Y,
            "KeyZ" | "key_z" => Z,
            "Digit0" | "digit_0" => Digit0,
            "Digit1" | "digit_1" => Digit1,
            "Digit2" | "digit_2" => Digit2,
            "Digit3" | "digit_3" => Digit3,
            "Digit4" | "digit_4" => Digit4,
            "Digit5" | "digit_5" => Digit5,
            "Digit6" | "digit_6" => Digit6,
            "Digit7" | "digit_7" => Digit7,
            "Digit8" | "digit_8" => Digit8,
            "Digit9" | "digit_9" => Digit9,
            "Backquote" | "backquote" => Backquote,
            "Backslash" | "backslash" => Backslash,
            "BracketLeft" | "bracket_left" => BracketLeft,
            "BracketRight" | "bracket_right" => BracketRight,
            "Comma" | "comma" => Comma,
            "Equal" | "equal" => Equal,
            "Minus" | "minus" => Minus,
            "Period" | "period" => Period,
            "Quote" | "quote" => Quote,
            "Semicolon" | "semicolon" => Semicolon,
            "Slash" | "slash" => Slash,
            "ArrowUp" | "arrow_up" => ArrowUp,
            "ArrowDown" | "arrow_down" => ArrowDown,
            "ArrowRight" | "arrow_right" => ArrowRight,
            "ArrowLeft" | "arrow_left" => ArrowLeft,
            "Delete" | "delete" => Delete,
            "Home" | "home" => Home,
            "End" | "end" => End,
            "PageUp" | "page_up" => PageUp,
            "PageDown" | "page_down" => PageDown,
            "Space" | "space" => Space,
            "Insert" | "insert" => Insert,
            "Enter" | "enter" => Enter,
            "Tab" | "tab" => Tab,
            "Backspace" | "backspace" => Backspace,
            "Escape" | "escape" => Escape,
            "F1" | "f1" => F1,
            "F2" | "f2" => F2,
            "F3" | "f3" => F3,
            "F4" | "f4" => F4,
            "F5" | "f5" => F5,
            "F6" | "f6" => F6,
            "F7" | "f7" => F7,
            "F8" | "f8" => F8,
            "F9" | "f9" => F9,
            "F10" | "f10" => F10,
            "F11" | "f11" => F11,
            "F12" | "f12" => F12,
        }
    }

    const fn canonical_name(self) -> &'static str {
        macro_rules! names {
            ($($key:ident => $name:literal,)+) => {
                match self { $(Self::$key => $name,)+ }
            };
        }
        names! {
            A => "KeyA", B => "KeyB", C => "KeyC", D => "KeyD", E => "KeyE", F => "KeyF",
            G => "KeyG", H => "KeyH", I => "KeyI", J => "KeyJ", K => "KeyK", L => "KeyL",
            M => "KeyM", N => "KeyN", O => "KeyO", P => "KeyP", Q => "KeyQ", R => "KeyR",
            S => "KeyS", T => "KeyT", U => "KeyU", V => "KeyV", W => "KeyW", X => "KeyX",
            Y => "KeyY", Z => "KeyZ", Digit0 => "Digit0", Digit1 => "Digit1", Digit2 => "Digit2",
            Digit3 => "Digit3", Digit4 => "Digit4", Digit5 => "Digit5", Digit6 => "Digit6",
            Digit7 => "Digit7", Digit8 => "Digit8", Digit9 => "Digit9", Backquote => "Backquote",
            Backslash => "Backslash", BracketLeft => "BracketLeft", BracketRight => "BracketRight",
            Comma => "Comma", Equal => "Equal", Minus => "Minus", Period => "Period", Quote => "Quote",
            Semicolon => "Semicolon", Slash => "Slash", ArrowUp => "ArrowUp", ArrowDown => "ArrowDown",
            ArrowRight => "ArrowRight", ArrowLeft => "ArrowLeft", Delete => "Delete", Home => "Home",
            End => "End", PageUp => "PageUp", PageDown => "PageDown", Space => "Space",
            Insert => "Insert", Enter => "Enter", Tab => "Tab", Backspace => "Backspace",
            Escape => "Escape", F1 => "F1", F2 => "F2", F3 => "F3", F4 => "F4", F5 => "F5",
            F6 => "F6", F7 => "F7", F8 => "F8", F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12",
        }
    }
}

/// One keymap trigger, independent of any windowing or terminal framework.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeymapTrigger {
    pub modifiers: KeymapModifiers,
    pub key: KeymapTriggerKey,
}

/// The non-physical trigger keys accepted by the keymap grammar.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeymapTriggerKey {
    Unicode(char),
    Physical(KeymapPhysicalKey),
    ScrollUp,
    ScrollDown,
    CatchAll,
}

impl KeymapTrigger {
    /// Return the canonical trigger spelling used by legacy defaults and UI recording.
    #[must_use]
    pub fn format_entry(&self) -> String {
        let mut output = String::new();
        if self.modifiers.command {
            push_modifier(
                &mut output,
                modifier_name("cmd", self.modifiers.command_side),
            );
        }
        if self.modifiers.ctrl {
            push_modifier(&mut output, modifier_name("ctrl", self.modifiers.ctrl_side));
        }
        if self.modifiers.alt {
            push_modifier(&mut output, modifier_name("alt", self.modifiers.alt_side));
        }
        if self.modifiers.shift {
            push_modifier(
                &mut output,
                modifier_name("shift", self.modifiers.shift_side),
            );
        }
        if !output.is_empty() {
            output.push('+');
        }
        match &self.key {
            KeymapTriggerKey::Unicode(ch) => output.push(*ch),
            KeymapTriggerKey::Physical(key) => output.push_str(key.canonical_name()),
            KeymapTriggerKey::ScrollUp => output.push_str("scroll_up"),
            KeymapTriggerKey::ScrollDown => output.push_str("scroll_down"),
            KeymapTriggerKey::CatchAll => output.push_str("catch_all"),
        }
        output
    }

    /// Generate modifier candidates in specificity order for a native input.
    #[must_use]
    pub fn input_mod_candidates(modifiers: KeymapModifiers) -> Vec<KeymapModifiers> {
        modifiers.input_candidates()
    }
}

fn modifier_name(base: &'static str, side: Option<KeymapModifierSide>) -> &'static str {
    match (base, side) {
        ("cmd", Some(KeymapModifierSide::Left)) => "left_cmd",
        ("cmd", Some(KeymapModifierSide::Right)) => "right_cmd",
        ("ctrl", Some(KeymapModifierSide::Left)) => "left_ctrl",
        ("ctrl", Some(KeymapModifierSide::Right)) => "right_ctrl",
        ("alt", Some(KeymapModifierSide::Left)) => "left_alt",
        ("alt", Some(KeymapModifierSide::Right)) => "right_alt",
        ("shift", Some(KeymapModifierSide::Left)) => "left_shift",
        ("shift", Some(KeymapModifierSide::Right)) => "right_shift",
        _ => base,
    }
}

fn push_modifier(output: &mut String, modifier: &str) {
    if !output.is_empty() {
        output.push('+');
    }
    output.push_str(modifier);
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct KeymapTriggerError {
    message: &'static str,
}

impl KeymapTriggerError {
    const fn invalid_format() -> Self {
        Self {
            message: "invalid keymap trigger",
        }
    }
}

impl FromStr for KeymapTrigger {
    type Err = KeymapTriggerError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(KeymapTriggerError::invalid_format());
        }
        let mut modifiers = KeymapModifiers::default();
        let mut key = None;
        for part in split_trigger_parts(input)? {
            match part {
                "shift" => set_modifier(&mut modifiers.shift)?,
                "ctrl" | "control" => set_modifier(&mut modifiers.ctrl)?,
                "alt" | "opt" | "option" => set_modifier(&mut modifiers.alt)?,
                "cmd" | "command" | "super" => set_modifier(&mut modifiers.command)?,
                "left_shift" => set_sided_modifier(
                    &mut modifiers.shift,
                    &mut modifiers.shift_side,
                    KeymapModifierSide::Left,
                )?,
                "right_shift" => set_sided_modifier(
                    &mut modifiers.shift,
                    &mut modifiers.shift_side,
                    KeymapModifierSide::Right,
                )?,
                "left_ctrl" | "left_control" => set_sided_modifier(
                    &mut modifiers.ctrl,
                    &mut modifiers.ctrl_side,
                    KeymapModifierSide::Left,
                )?,
                "right_ctrl" | "right_control" => set_sided_modifier(
                    &mut modifiers.ctrl,
                    &mut modifiers.ctrl_side,
                    KeymapModifierSide::Right,
                )?,
                "left_alt" | "left_opt" | "left_option" => set_sided_modifier(
                    &mut modifiers.alt,
                    &mut modifiers.alt_side,
                    KeymapModifierSide::Left,
                )?,
                "right_alt" | "right_opt" | "right_option" => set_sided_modifier(
                    &mut modifiers.alt,
                    &mut modifiers.alt_side,
                    KeymapModifierSide::Right,
                )?,
                "left_cmd" | "left_command" | "left_super" => set_sided_modifier(
                    &mut modifiers.command,
                    &mut modifiers.command_side,
                    KeymapModifierSide::Left,
                )?,
                "right_cmd" | "right_command" | "right_super" => set_sided_modifier(
                    &mut modifiers.command,
                    &mut modifiers.command_side,
                    KeymapModifierSide::Right,
                )?,
                _ => {
                    let parsed = if part == "physical:zero" {
                        Some(KeymapTriggerKey::Physical(KeymapPhysicalKey::Digit0))
                    } else {
                        KeymapPhysicalKey::parse(part)?.map(KeymapTriggerKey::Physical)
                    };
                    let parsed = parsed.or_else(|| {
                        if part.eq_ignore_ascii_case("scroll_up") {
                            Some(KeymapTriggerKey::ScrollUp)
                        } else if part.eq_ignore_ascii_case("scroll_down") {
                            Some(KeymapTriggerKey::ScrollDown)
                        } else if part.eq_ignore_ascii_case("catch_all") {
                            Some(KeymapTriggerKey::CatchAll)
                        } else if part.eq_ignore_ascii_case("space") {
                            Some(KeymapTriggerKey::Unicode(' '))
                        } else {
                            None
                        }
                    });
                    let parsed = parsed.or_else(|| {
                        let mut chars = part.chars();
                        let ch = chars.next()?;
                        chars
                            .next()
                            .is_none()
                            .then_some(KeymapTriggerKey::Unicode(ch))
                    });
                    if key
                        .replace(parsed.ok_or_else(KeymapTriggerError::invalid_format)?)
                        .is_some()
                    {
                        return Err(KeymapTriggerError::invalid_format());
                    }
                }
            }
        }
        Ok(Self {
            modifiers,
            key: key.ok_or_else(KeymapTriggerError::invalid_format)?,
        })
    }
}

fn split_trigger_parts(input: &str) -> Result<Vec<&str>, KeymapTriggerError> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, character) in input.char_indices() {
        if character == '+' && index != start {
            parts.push(
                input
                    .get(start..index)
                    .ok_or_else(KeymapTriggerError::invalid_format)?,
            );
            start = index.saturating_add(1);
        }
    }
    parts.push(
        input
            .get(start..)
            .ok_or_else(KeymapTriggerError::invalid_format)?,
    );
    if parts.iter().any(|part| part.is_empty()) {
        return Err(KeymapTriggerError::invalid_format());
    }
    Ok(parts)
}

const fn set_modifier(field: &mut bool) -> Result<(), KeymapTriggerError> {
    if *field {
        return Err(KeymapTriggerError::invalid_format());
    }
    *field = true;
    Ok(())
}

fn set_sided_modifier(
    field: &mut bool,
    side_field: &mut Option<KeymapModifierSide>,
    side: KeymapModifierSide,
) -> Result<(), KeymapTriggerError> {
    set_modifier(field)?;
    if side_field.replace(side).is_some() {
        return Err(KeymapTriggerError::invalid_format());
    }
    Ok(())
}

/// Flags attached to a keymap sequence.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeymapBindingFlags {
    pub consumed: bool,
    pub all: bool,
    pub global: bool,
    pub performable: bool,
}

impl KeymapBindingFlags {
    #[must_use]
    pub const fn default_consumed() -> Self {
        Self {
            consumed: true,
            all: false,
            global: false,
            performable: false,
        }
    }
}

impl Default for KeymapBindingFlags {
    fn default() -> Self {
        Self::default_consumed()
    }
}

/// A parsed keymap sequence and its trigger flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapSequence {
    pub triggers: Vec<KeymapTrigger>,
    pub flags: KeymapBindingFlags,
}

impl KeymapSequence {
    #[must_use]
    pub fn format_entry(&self) -> String {
        let mut output = String::new();
        for (enabled, name) in [
            (self.flags.performable, "performable"),
            (self.flags.global, "global"),
            (self.flags.all, "all"),
            (!self.flags.consumed, "unconsumed"),
        ] {
            if enabled {
                output.push_str(name);
                output.push(':');
            }
        }
        output.push_str(
            &self
                .triggers
                .iter()
                .map(KeymapTrigger::format_entry)
                .collect::<Vec<_>>()
                .join(" "),
        );
        output
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct KeymapSequenceError {
    message: String,
}

impl KeymapSequenceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Parse a JSONC or legacy keymap sequence without parsing its action.
///
/// # Errors
/// Rejects empty or malformed trigger sequences and invalid trigger flags.
pub fn parse_keymap_sequence(source: &str) -> Result<KeymapSequence, KeymapSequenceError> {
    let (flags, source) = parse_sequence_flags(source)?;
    let triggers = source
        .split_ascii_whitespace()
        .map(parse_legacy_trigger)
        .collect::<Result<Vec<_>, _>>()?;
    if triggers.is_empty() {
        return Err(KeymapSequenceError::new(
            "keystroke sequence cannot be empty",
        ));
    }
    if triggers.len() > 1 && (flags.global || flags.all) {
        return Err(KeymapSequenceError::new(
            "global/all trigger flags cannot be used with a chord",
        ));
    }
    Ok(KeymapSequence { triggers, flags })
}

fn parse_sequence_flags(
    mut source: &str,
) -> Result<(KeymapBindingFlags, &str), KeymapSequenceError> {
    let mut flags = KeymapBindingFlags::default();
    loop {
        let Some((prefix, tail)) = source.split_once(':') else {
            return Ok((flags, source));
        };
        let field = match prefix {
            "performable" => &mut flags.performable,
            "global" => &mut flags.global,
            "all" => &mut flags.all,
            "unconsumed" => {
                if !flags.consumed {
                    return Err(KeymapSequenceError::new(format!(
                        "duplicate trigger flag {prefix:?}"
                    )));
                }
                flags.consumed = false;
                source = tail;
                continue;
            }
            _ => return Ok((flags, source)),
        };
        if *field {
            return Err(KeymapSequenceError::new(format!(
                "duplicate trigger flag {prefix:?}"
            )));
        }
        *field = true;
        source = tail;
    }
}

fn parse_legacy_trigger(source: &str) -> Result<KeymapTrigger, KeymapSequenceError> {
    if let Ok(trigger) = source.parse() {
        return Ok(trigger);
    }
    let mut rest = source;
    let mut modifiers = Vec::new();
    while let Some((candidate, tail)) = rest.split_once('-') {
        let Some(modifier) = normalize_modifier(candidate) else {
            break;
        };
        modifiers.push(modifier);
        rest = tail;
    }
    if modifiers.is_empty() || rest.is_empty() {
        return Err(KeymapSequenceError::new(format!(
            "invalid keystroke {source:?}"
        )));
    }
    let normalized = format!("{}+{rest}", modifiers.join("+"));
    normalized
        .parse()
        .map_err(|_| KeymapSequenceError::new(format!("invalid keystroke {source:?}")))
}

fn normalize_modifier(value: &str) -> Option<&'static str> {
    match value {
        "shift" => Some("shift"),
        "ctrl" | "control" => Some("ctrl"),
        "alt" | "opt" | "option" => Some("alt"),
        "cmd" | "command" | "super" | "win" => Some("cmd"),
        _ => None,
    }
}

/// A framework-free event passed to the effective keymap matcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapInput {
    Key {
        key: KeymapPhysicalKey,
        modifiers: KeymapModifiers,
        text: Option<char>,
    },
    Scroll {
        up: bool,
        modifiers: KeymapModifiers,
    },
}

impl KeymapInput {
    /// Return candidates from most specific to least specific, preserving legacy lookup order.
    #[must_use]
    pub fn candidates(self) -> Vec<KeymapTrigger> {
        let (modifiers, key) = match self {
            Self::Key {
                key,
                modifiers,
                text,
            } => {
                let mut candidates = Vec::new();
                for mods in modifiers.input_candidates() {
                    candidates.push(KeymapTrigger {
                        modifiers: mods,
                        key: KeymapTriggerKey::Physical(key),
                    });
                    if let Some(text) = text {
                        candidates.push(KeymapTrigger {
                            modifiers: mods,
                            key: KeymapTriggerKey::Unicode(text),
                        });
                    }
                }
                candidates.extend(catch_all_candidates(modifiers));
                return candidates;
            }
            Self::Scroll { up, modifiers } => (
                modifiers,
                if up {
                    KeymapTriggerKey::ScrollUp
                } else {
                    KeymapTriggerKey::ScrollDown
                },
            ),
        };
        let mut candidates = modifiers
            .input_candidates()
            .into_iter()
            .map(|modifiers| KeymapTrigger {
                modifiers,
                key: key.clone(),
            })
            .collect::<Vec<_>>();
        candidates.extend(catch_all_candidates(modifiers));
        candidates
    }
}

fn catch_all_candidates(modifiers: KeymapModifiers) -> Vec<KeymapTrigger> {
    let mut candidates = modifiers
        .input_candidates()
        .into_iter()
        .map(|modifiers| KeymapTrigger {
            modifiers,
            key: KeymapTriggerKey::CatchAll,
        })
        .collect::<Vec<_>>();
    if modifiers != KeymapModifiers::default() {
        let global = KeymapTrigger {
            modifiers: KeymapModifiers::default(),
            key: KeymapTriggerKey::CatchAll,
        };
        if !candidates.contains(&global) {
            candidates.push(global);
        }
    }
    candidates
}

/// A raw binding with its source metadata, ready for action/context injection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapBindingSnapshot {
    pub context: KeymapContext,
    pub keystrokes: String,
    pub action: KeymapAction,
    pub kind: KeymapBindingKind,
    pub source: KeymapBindingSource,
}

/// The accepted keymap file and effective raw binding layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapSnapshot {
    pub path: PathBuf,
    pub keymap: KeymapFile,
    pub effective_bindings: Vec<KeymapBindingSnapshot>,
    pub diagnostics: Vec<KeymapDiagnostic>,
    pub revision: u64,
}

impl KeymapSnapshot {
    #[must_use]
    pub fn diagnostic_summary(&self) -> Option<String> {
        (!self.diagnostics.is_empty()).then(|| {
            self.diagnostics
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}

/// Match result from a compiled effective keymap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapMatch<A> {
    NoMatch,
    Pending,
    Matched { action: A, consumed: bool },
    Consumed,
}

#[derive(Clone, Debug)]
enum CompiledAction<A> {
    Invoke(A),
    Unbind(A),
    Consume,
}

#[derive(Clone, Debug)]
struct CompiledBinding<A, C> {
    context: C,
    sequence: Vec<KeymapTrigger>,
    action: CompiledAction<A>,
    flags: KeymapBindingFlags,
}

/// A compiled keymap program. The action and context types are injected by the composition layer.
#[derive(Clone, Debug)]
pub struct KeymapProgram<A, C> {
    bindings: Vec<CompiledBinding<A, C>>,
    active_sequence: Vec<KeymapTrigger>,
}

impl<A, C> KeymapProgram<A, C>
where
    A: Clone + Eq,
    C: Clone,
{
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            bindings: Vec::new(),
            active_sequence: Vec::new(),
        }
    }

    /// Compile raw bindings while leaving action and GPUI context interpretation to callbacks.
    pub fn compile(
        bindings: impl IntoIterator<Item = KeymapBindingSnapshot>,
        resolve_action: impl Fn(&KeymapAction) -> Result<Option<A>, String>,
        compile_context: impl Fn(&KeymapContext) -> Result<C, String>,
    ) -> (Self, Vec<KeymapDiagnostic>) {
        let mut compiled = Vec::new();
        let mut diagnostics = Vec::new();
        for binding in bindings {
            let context = match compile_context(&binding.context) {
                Ok(context) => context,
                Err(message) => {
                    diagnostics.push(KeymapDiagnostic {
                        section: None,
                        field: Some("context".to_owned()),
                        message,
                    });
                    continue;
                }
            };
            let (sequence, flags) = match parse_keymap_sequence(&binding.keystrokes) {
                Ok(sequence) => (sequence.triggers, sequence.flags),
                Err(error) => {
                    diagnostics.push(KeymapDiagnostic {
                        section: None,
                        field: Some(format!(
                            "{}.{}",
                            binding_kind_field(binding.kind),
                            binding.keystrokes
                        )),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let action = match binding.kind {
                KeymapBindingKind::Unbind => match resolve_action(&binding.action) {
                    Ok(Some(action)) => CompiledAction::Unbind(action),
                    Ok(None) => {
                        diagnostics.push(KeymapDiagnostic {
                            section: None,
                            field: Some(format!("unbind.{}", binding.keystrokes)),
                            message: "an unbind target must name an action".to_owned(),
                        });
                        continue;
                    }
                    Err(message) => {
                        diagnostics.push(binding_diagnostic(
                            format!("unbind.{}", binding.keystrokes),
                            message,
                        ));
                        continue;
                    }
                },
                KeymapBindingKind::Binding => match &binding.action {
                    KeymapAction::None => CompiledAction::Consume,
                    action @ KeymapAction::Command { .. } => match resolve_action(action) {
                        Ok(Some(action)) => CompiledAction::Invoke(action),
                        Ok(None) => CompiledAction::Consume,
                        Err(message) => {
                            diagnostics.push(binding_diagnostic(
                                format!("bindings.{}", binding.keystrokes),
                                message,
                            ));
                            continue;
                        }
                    },
                },
            };
            compiled.push(CompiledBinding {
                context,
                sequence,
                action,
                flags,
            });
        }
        (
            Self {
                bindings: compiled,
                active_sequence: Vec::new(),
            },
            diagnostics,
        )
    }

    /// Match one input against this program. `context_active` is supplied by the UI adapter.
    pub fn next(
        &mut self,
        input: KeymapInput,
        context_active: impl Fn(&C) -> bool,
    ) -> KeymapMatch<A> {
        self.next_candidates(&input.candidates(), context_active)
    }

    /// Match already converted candidates from a native/UI adapter.
    pub fn next_candidates(
        &mut self,
        candidates: &[KeymapTrigger],
        context_active: impl Fn(&C) -> bool,
    ) -> KeymapMatch<A> {
        if candidates.is_empty() {
            return KeymapMatch::NoMatch;
        }
        let next_index = self.active_sequence.len();
        let mut unbound_bindings = Vec::new();
        let selected = self.bindings.iter().rev().find_map(|binding| {
            if !context_active(&binding.context) {
                return None;
            }
            let (prefix, remaining) = binding.sequence.split_at_checked(next_index)?;
            let (trigger, pending) = remaining.split_first()?;
            if !(prefix == self.active_sequence
                && candidates
                    .iter()
                    .any(|candidate| trigger_matches(candidate, trigger)))
            {
                return None;
            }
            let action = match &binding.action {
                CompiledAction::Unbind(action) => {
                    unbound_bindings.push((binding.sequence.clone(), action.clone()));
                    return None;
                }
                CompiledAction::Invoke(action)
                    if unbound_bindings.iter().any(|(sequence, target)| {
                        sequence == &binding.sequence && target == action
                    }) =>
                {
                    return None;
                }
                CompiledAction::Invoke(action) => Some(action.clone()),
                CompiledAction::Consume => None,
            };
            Some((!pending.is_empty(), action, binding.flags, trigger.clone()))
        });
        let Some((pending, action, flags, trigger)) = selected else {
            if self.active_sequence.is_empty() {
                return KeymapMatch::NoMatch;
            }
            self.active_sequence.clear();
            return KeymapMatch::Consumed;
        };
        if pending {
            self.active_sequence.push(trigger);
            return KeymapMatch::Pending;
        }
        self.active_sequence.clear();
        action.map_or(KeymapMatch::Consumed, |action| KeymapMatch::Matched {
            action,
            consumed: flags.consumed,
        })
    }
}

fn trigger_matches(candidate: &KeymapTrigger, binding: &KeymapTrigger) -> bool {
    if candidate.modifiers != binding.modifiers {
        return false;
    }
    match (&candidate.key, &binding.key) {
        (KeymapTriggerKey::Unicode(candidate), KeymapTriggerKey::Unicode(binding)) => {
            candidate == binding || candidate.to_lowercase().eq(binding.to_lowercase())
        }
        (candidate, binding) => candidate == binding,
    }
}

const fn binding_kind_field(kind: KeymapBindingKind) -> &'static str {
    match kind {
        KeymapBindingKind::Unbind => "unbind",
        KeymapBindingKind::Binding => "bindings",
    }
}

const fn binding_diagnostic(field: String, message: String) -> KeymapDiagnostic {
    KeymapDiagnostic {
        section: None,
        field: Some(field),
        message,
    }
}

/// Effective defaults and user entries in source order. A user section can disable built-ins for
/// exactly its context while retaining all user entries in that section.
#[must_use]
pub fn effective_bindings(
    builtins: &[KeymapBindingSnapshot],
    keymap: &KeymapFile,
) -> Vec<KeymapBindingSnapshot> {
    let enabled_builtins = builtins
        .iter()
        .filter(|binding| keymap.use_builtin_defaults(&binding.context))
        .cloned()
        .collect::<Vec<_>>();
    let mut effective = enabled_builtins;
    for section in keymap.sections() {
        for kind in [KeymapBindingKind::Unbind, KeymapBindingKind::Binding] {
            effective.extend(section.entries(kind).map(|entry| KeymapBindingSnapshot {
                context: section.context.clone(),
                keystrokes: entry.keystrokes.clone(),
                action: entry.action.clone(),
                kind,
                source: KeymapBindingSource::User,
            }));
        }
    }
    effective
}

/// Owns the accepted keymap file, effective defaults, watch stamp, and compiled matcher.
pub struct KeymapRuntime<A, C>
where
    A: Clone + Eq,
    C: Clone,
{
    path: PathBuf,
    last_check: Instant,
    stamp: FileStamp,
    snapshot: KeymapSnapshot,
    built_in_bindings: Vec<KeymapBindingSnapshot>,
    program: KeymapProgram<A, C>,
}

impl<A, C> KeymapRuntime<A, C>
where
    A: Clone + Eq,
    C: Clone,
{
    pub fn new(
        config: &BoottyConfig,
        built_in_bindings: Vec<KeymapBindingSnapshot>,
        resolve_action: impl Fn(&KeymapAction) -> Result<Option<A>, String>,
        compile_context: impl Fn(&KeymapContext) -> Result<C, String>,
    ) -> Self {
        let path = crate::keymap_path_for_config(&config.config_path);
        let effective_bindings = built_in_bindings.clone();
        let (program, diagnostics) = KeymapProgram::compile(
            effective_bindings.clone(),
            &resolve_action,
            &compile_context,
        );
        let mut runtime = Self {
            stamp: FileStamp::read(&path),
            snapshot: KeymapSnapshot {
                path: path.clone(),
                keymap: KeymapFile::default(),
                effective_bindings,
                diagnostics,
                revision: 0,
            },
            path,
            last_check: Instant::now(),
            built_in_bindings,
            program,
        };
        runtime.reload(resolve_action, compile_context).ok();
        runtime
    }

    #[must_use]
    pub const fn snapshot(&self) -> &KeymapSnapshot {
        &self.snapshot
    }

    #[must_use]
    pub fn reload_due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_check) < CONFIG_HOT_RELOAD_INTERVAL {
            return false;
        }
        self.last_check = now;
        FileStamp::read(&self.path) != self.stamp
    }

    pub fn sync_config(
        &mut self,
        config: &BoottyConfig,
        built_in_bindings: Vec<KeymapBindingSnapshot>,
        resolve_action: impl Fn(&KeymapAction) -> Result<Option<A>, String>,
        compile_context: impl Fn(&KeymapContext) -> Result<C, String>,
    ) {
        self.built_in_bindings = built_in_bindings;
        self.snapshot.path = crate::keymap_path_for_config(&config.config_path);
        self.rebuild_effective_bindings(resolve_action, compile_context);
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
    }

    ///
    /// # Errors
    /// Returns a read error for an inaccessible keymap file. Parse diagnostics retain
    /// the previous keymap and are returned in the successful outcome.
    pub fn reload(
        &mut self,
        resolve_action: impl Fn(&KeymapAction) -> Result<Option<A>, String>,
        compile_context: impl Fn(&KeymapContext) -> Result<C, String>,
    ) -> Result<Option<String>, KeymapRuntimeError> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(KeymapRuntimeError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        self.stamp = FileStamp::read(&self.path);
        let keymap = match KeymapFile::parse(&contents) {
            Ok(keymap) => keymap,
            Err(error) => {
                self.snapshot.diagnostics = vec![KeymapDiagnostic {
                    section: None,
                    field: None,
                    message: error.to_string(),
                }];
                self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
                return Ok(self.snapshot.diagnostic_summary());
            }
        };
        self.snapshot.keymap = keymap;
        self.rebuild_effective_bindings(resolve_action, compile_context);
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
        Ok(self.snapshot.diagnostic_summary())
    }

    ///
    /// # Errors
    /// Returns a write or reload error when the edited keymap cannot be committed
    /// or reloaded.
    pub fn edit(
        &mut self,
        edit: &KeymapEdit,
        resolve_action: impl Fn(&KeymapAction) -> Result<Option<A>, String>,
        compile_context: impl Fn(&KeymapContext) -> Result<C, String>,
    ) -> Result<KeymapWriteOutcome, KeymapRuntimeError> {
        let outcome = write_keymap_edit(&self.path, edit)?;
        self.reload(resolve_action, compile_context)?;
        Ok(outcome)
    }

    pub fn next(
        &mut self,
        input: KeymapInput,
        context_active: impl Fn(&C) -> bool,
    ) -> KeymapMatch<A> {
        self.program.next(input, context_active)
    }

    pub fn next_candidates(
        &mut self,
        candidates: &[KeymapTrigger],
        context_active: impl Fn(&C) -> bool,
    ) -> KeymapMatch<A> {
        self.program.next_candidates(candidates, context_active)
    }

    fn rebuild_effective_bindings(
        &mut self,
        resolve_action: impl Fn(&KeymapAction) -> Result<Option<A>, String>,
        compile_context: impl Fn(&KeymapContext) -> Result<C, String>,
    ) {
        self.snapshot.effective_bindings =
            effective_bindings(&self.built_in_bindings, &self.snapshot.keymap);
        let (program, mut diagnostics) = KeymapProgram::compile(
            self.snapshot.effective_bindings.clone(),
            resolve_action,
            compile_context,
        );
        diagnostics.splice(0..0, self.snapshot.keymap.diagnostics().iter().cloned());
        self.snapshot.diagnostics = diagnostics;
        self.program = program;
    }
}

#[derive(Debug, Error)]
pub enum KeymapRuntimeError {
    #[error("read keymap file {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Write(#[from] crate::keymap_file::KeymapWriteError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: Option<u64>,
}

impl FileStamp {
    fn read(path: &Path) -> Self {
        let metadata = fs::metadata(path).ok();
        Self {
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            len: metadata.map(|metadata| metadata.len()),
        }
    }
}

/// Convert the existing TOML keybind defaults into raw config keymap bindings.
#[must_use]
pub fn legacy_keymap_bindings(config: &BoottyConfig) -> Vec<KeymapBindingSnapshot> {
    let input = &config.input;
    let mut bindings = Vec::new();
    let mut global_input = input.clone();
    global_input.backend_keybinds = BackendKeybindConfig::default();
    extend_legacy_bindings(
        &mut bindings,
        &KeymapContext::Global,
        &global_input.keybinds_for_backend(MultiplexerBackendConfig::Native),
    );
    for entry in &input.sidebar_keybind {
        let Some((keystrokes, action)) = split_keybind_entry(entry) else {
            continue;
        };
        let action = if action.starts_with("ui.sidebar.") {
            action.to_owned()
        } else {
            format!("ui.sidebar.{action}")
        };
        bindings.push(KeymapBindingSnapshot {
            context: KeymapContext::Sidebar,
            keystrokes: keystrokes.to_owned(),
            action: KeymapAction::command(action),
            kind: KeymapBindingKind::Binding,
            source: KeymapBindingSource::BuiltIn,
        });
    }
    let mut backend_input = input.clone();
    backend_input.keybind.clear();
    for (context, backend) in [
        (KeymapContext::Herdr, MultiplexerBackendConfig::Herdr),
        (KeymapContext::Native, MultiplexerBackendConfig::Native),
        (KeymapContext::Rmux, MultiplexerBackendConfig::Rmux),
        (KeymapContext::Tmux, MultiplexerBackendConfig::Tmux),
    ] {
        extend_legacy_bindings(
            &mut bindings,
            &context,
            &backend_input.keybinds_for_backend(backend),
        );
    }
    bindings
}

fn extend_legacy_bindings(
    snapshots: &mut Vec<KeymapBindingSnapshot>,
    context: &KeymapContext,
    entries: &[String],
) {
    for entry in entries {
        let Some((trigger, action)) = split_keybind_entry(entry) else {
            continue;
        };
        let normalized = trigger.split('>').collect::<Vec<_>>().join(" ");
        let Ok(sequence) = parse_keymap_sequence(&normalized) else {
            continue;
        };
        snapshots.push(KeymapBindingSnapshot {
            context: context.clone(),
            keystrokes: sequence.format_entry(),
            action: KeymapAction::command(action),
            kind: KeymapBindingKind::Binding,
            source: KeymapBindingSource::BuiltIn,
        });
    }
}
