use bootty_config::{
    KeymapModifierSide, KeymapModifiers, ModifierRemapParseError, ModifierRemapSet,
};
use pretty_assertions::assert_eq;

fn left_ctrl() -> KeymapModifiers {
    KeymapModifiers {
        ctrl: true,
        ctrl_side: Some(KeymapModifierSide::Left),
        ..KeymapModifiers::default()
    }
}

fn right_ctrl() -> KeymapModifiers {
    KeymapModifiers {
        ctrl: true,
        ctrl_side: Some(KeymapModifierSide::Right),
        ..KeymapModifiers::default()
    }
}

fn left_alt() -> KeymapModifiers {
    KeymapModifiers {
        alt: true,
        alt_side: Some(KeymapModifierSide::Left),
        ..KeymapModifiers::default()
    }
}

fn right_alt() -> KeymapModifiers {
    KeymapModifiers {
        alt: true,
        alt_side: Some(KeymapModifierSide::Right),
        ..KeymapModifiers::default()
    }
}

fn left_command() -> KeymapModifiers {
    KeymapModifiers {
        command: true,
        command_side: Some(KeymapModifierSide::Left),
        ..KeymapModifiers::default()
    }
}

#[test]
fn unsided_source_maps_both_sides_to_the_left_target() {
    let mut remaps = ModifierRemapSet::default();
    remaps.parse("ctrl=super").expect("remap parses");
    remaps.finalize();

    assert_eq!(remaps.apply(left_ctrl()), left_command());
    assert_eq!(remaps.apply(right_ctrl()), left_command());
    assert_eq!(
        remaps.formatted_entries(),
        vec![
            "right_ctrl=left_super".to_owned(),
            "left_ctrl=left_super".to_owned(),
        ]
    );
}

#[test]
fn sided_source_and_target_preserve_other_modifier_sides() {
    let mut remaps = ModifierRemapSet::default();
    remaps
        .parse("left_alt=right_ctrl")
        .expect("sided remap parses");
    remaps.finalize();

    assert_eq!(remaps.apply(left_alt()), right_ctrl());
    assert_eq!(remaps.apply(right_alt()), right_alt());
}

#[test]
fn aliases_and_errors_keep_the_public_grammar() {
    let mut remaps = ModifierRemapSet::default();
    remaps.parse("cmd=control").expect("aliases parse");
    remaps.parse("opt=shift").expect("aliases parse");

    assert_eq!(
        remaps.parse("ctrl"),
        Err(ModifierRemapParseError::MissingAssignment)
    );
    assert_eq!(
        remaps.parse("middle_ctrl=super"),
        Err(ModifierRemapParseError::InvalidModifier(
            "middle_ctrl".to_owned()
        ))
    );
}

#[test]
fn an_empty_set_formats_as_the_clear_entry() {
    assert_eq!(
        ModifierRemapSet::default().formatted_entries(),
        vec![String::new()]
    );
}
