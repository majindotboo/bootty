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

#[test]
fn unsided_source_maps_both_sides_to_the_left_target() {
    let mut remaps = ModifierRemapSet::default();
    remaps.parse("ctrl=super").expect("remap parses");
    remaps.finalize();

    let expected = KeymapModifiers {
        command: true,
        command_side: Some(KeymapModifierSide::Left),
        ..KeymapModifiers::default()
    };
    assert_eq!(remaps.apply(left_ctrl()), expected);
    assert_eq!(remaps.apply(right_ctrl()), expected);
    assert_eq!(
        remaps.formatted_entries(),
        vec!["right_ctrl=left_super", "left_ctrl=left_super"]
    );
}

#[test]
fn sided_source_and_target_preserve_other_modifier_sides() {
    let mut remaps = ModifierRemapSet::default();
    remaps
        .parse("left_alt=right_ctrl")
        .expect("sided remap parses");
    remaps.finalize();

    let left_alt = KeymapModifiers {
        alt: true,
        alt_side: Some(KeymapModifierSide::Left),
        ..KeymapModifiers::default()
    };
    let right_alt = KeymapModifiers {
        alt: true,
        alt_side: Some(KeymapModifierSide::Right),
        ..KeymapModifiers::default()
    };
    assert_eq!(
        remaps.apply(left_alt),
        KeymapModifiers {
            ctrl: true,
            ctrl_side: Some(KeymapModifierSide::Right),
            ..KeymapModifiers::default()
        }
    );
    assert_eq!(remaps.apply(right_alt), right_alt);
}

#[test]
fn aliases_errors_and_empty_format_keep_the_public_grammar() {
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
    assert_eq!(
        ModifierRemapSet::default().formatted_entries(),
        vec![String::new()]
    );
}
