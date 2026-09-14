use bootty_terminal::{
    terminal_input::{DirectKeyInput, ModifierSideState, TerminalInputCommand},
    terminal_input_model::{KeyInput, KeyMods, TerminalKey},
};
use pretty_assertions::assert_eq;

const fn key_input(key: TerminalKey, mods: KeyMods) -> KeyInput {
    KeyInput {
        key,
        mods,
        repeat: false,
        utf8: None,
        unshifted: None,
    }
}

#[test]
fn modifier_side_state_applies_physical_sides_to_terminal_input() {
    let sides = ModifierSideState {
        right_shift: true,
        right_alt: true,
        left_ctrl: true,
        ..ModifierSideState::default()
    };
    let mut input = key_input(
        TerminalKey::Tab,
        KeyMods {
            shift: true,
            alt: true,
            ..KeyMods::default()
        },
    );

    sides.apply_to_key_input(&mut input);

    assert_eq!(
        input.mods,
        KeyMods {
            shift: true,
            alt: true,
            ctrl: true,
            right_shift: true,
            right_alt: true,
            ..KeyMods::default()
        }
    );
}

#[test]
fn direct_key_input_and_terminal_commands_are_framework_free_values() {
    let direct = DirectKeyInput {
        input: key_input(
            TerminalKey::C,
            KeyMods {
                ctrl: true,
                ..KeyMods::default()
            },
        ),
    };
    let command = TerminalInputCommand::Key(direct.input());

    assert_eq!(command, TerminalInputCommand::Key(direct.input()));
}
