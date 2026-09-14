use bootty_config::{
    KeymapModifierSide, KeymapModifiers, ModifierRemapParseError, ModifierRemapSet,
};
use bootty_terminal::terminal_input_model::KeyMods;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("invalid modifier-remap {entry:?}: {source}")]
pub struct ModifierRemapConfigError {
    entry: String,
    r#source: ModifierRemapParseError,
}

/// Parse and finalize the configured modifier remaps.
///
/// # Errors
/// Returns the offending entry and parse error for an invalid remap.
pub fn resolve_modifier_remaps(
    entries: &[String],
) -> Result<ModifierRemapSet, ModifierRemapConfigError> {
    let mut set = ModifierRemapSet::default();
    for entry in entries {
        set.parse(entry)
            .map_err(|source| ModifierRemapConfigError {
                entry: entry.clone(),
                source,
            })?;
    }
    set.finalize();
    Ok(set)
}

pub fn apply_modifier_remap(remaps: &ModifierRemapSet, mods: KeyMods) -> KeyMods {
    let remapped = remaps.apply(KeymapModifiers {
        shift: mods.shift,
        ctrl: mods.ctrl,
        alt: mods.alt,
        command: mods.command,
        shift_side: modifier_side(mods.shift, mods.right_shift),
        ctrl_side: modifier_side(mods.ctrl, mods.right_ctrl),
        alt_side: modifier_side(mods.alt, mods.right_alt),
        command_side: modifier_side(mods.command, mods.right_command),
    });
    KeyMods {
        shift: remapped.shift,
        ctrl: remapped.ctrl,
        alt: remapped.alt,
        command: remapped.command,
        right_shift: remapped.shift_side == Some(KeymapModifierSide::Right),
        right_ctrl: remapped.ctrl_side == Some(KeymapModifierSide::Right),
        right_alt: remapped.alt_side == Some(KeymapModifierSide::Right),
        right_command: remapped.command_side == Some(KeymapModifierSide::Right),
        caps_lock: mods.caps_lock,
        num_lock: mods.num_lock,
    }
}

fn modifier_side(pressed: bool, right: bool) -> Option<KeymapModifierSide> {
    pressed.then_some(if right {
        KeymapModifierSide::Right
    } else {
        KeymapModifierSide::Left
    })
}
