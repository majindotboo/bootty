use bootty_config::{
    KeymapBindingFlags, KeymapBindingSnapshot, KeymapBindingSource, KeymapInput, KeymapMatch,
    KeymapModifierSide, KeymapModifiers, KeymapPhysicalKey, KeymapProgram, KeymapTrigger,
    KeymapTriggerKey,
    keymap_file::{KeymapAction, KeymapBindingKind, KeymapContext, KeymapFile},
    parse_keymap_sequence,
};
use pretty_assertions::assert_eq;

fn binding(
    context: KeymapContext,
    keystrokes: &str,
    action: KeymapAction,
) -> KeymapBindingSnapshot {
    KeymapBindingSnapshot {
        context,
        keystrokes: keystrokes.to_owned(),
        action,
        kind: KeymapBindingKind::Binding,
        source: KeymapBindingSource::User,
    }
}

#[allow(clippy::unnecessary_wraps)] // Matches the fallible action resolver contract.
fn action_name(action: &KeymapAction) -> Result<Option<String>, String> {
    Ok(action.name().map(str::to_owned))
}

#[allow(clippy::unnecessary_wraps)] // Matches the fallible context compiler contract.
fn context_name(context: &KeymapContext) -> Result<String, String> {
    Ok(context.to_string())
}

#[test]
fn sequence_parser_preserves_flags_sides_aliases_and_legacy_hyphens() {
    let sequence = parse_keymap_sequence("performable:unconsumed:left_ctrl+right_alt+a ctrl-b")
        .expect("sequence parses");
    assert_eq!(
        sequence.flags,
        KeymapBindingFlags {
            consumed: false,
            performable: true,
            ..KeymapBindingFlags::default()
        }
    );
    assert_eq!(
        sequence.triggers[0].modifiers.ctrl_side,
        Some(KeymapModifierSide::Left)
    );
    assert_eq!(
        sequence.triggers[0].modifiers.alt_side,
        Some(KeymapModifierSide::Right)
    );
    assert_eq!(sequence.triggers[1].format_entry(), "ctrl+b");
    assert_eq!(
        sequence.format_entry(),
        "performable:unconsumed:left_ctrl+right_alt+a ctrl+b"
    );
}

#[test]
fn input_candidates_prefer_physical_then_text_then_catch_all() {
    let candidates = KeymapInput::Key {
        key: KeymapPhysicalKey::A,
        modifiers: KeymapModifiers {
            ctrl: true,
            ctrl_side: Some(KeymapModifierSide::Right),
            ..KeymapModifiers::default()
        },
        text: Some('a'),
    }
    .candidates();
    assert_eq!(
        &candidates[..4],
        &[
            KeymapTrigger {
                modifiers: KeymapModifiers {
                    ctrl: true,
                    ctrl_side: Some(KeymapModifierSide::Right),
                    ..KeymapModifiers::default()
                },
                key: KeymapTriggerKey::Physical(KeymapPhysicalKey::A),
            },
            KeymapTrigger {
                modifiers: KeymapModifiers {
                    ctrl: true,
                    ctrl_side: Some(KeymapModifierSide::Right),
                    ..KeymapModifiers::default()
                },
                key: KeymapTriggerKey::Unicode('a'),
            },
            KeymapTrigger {
                modifiers: KeymapModifiers {
                    ctrl: true,
                    ..KeymapModifiers::default()
                },
                key: KeymapTriggerKey::Physical(KeymapPhysicalKey::A),
            },
            KeymapTrigger {
                modifiers: KeymapModifiers {
                    ctrl: true,
                    ..KeymapModifiers::default()
                },
                key: KeymapTriggerKey::Unicode('a'),
            },
        ]
    );
    let last = candidates.last().expect("catch-all candidate");
    assert_eq!(last.modifiers, KeymapModifiers::default());
    assert_eq!(last.key, KeymapTriggerKey::CatchAll);
}

#[test]
fn program_preserves_precedence_named_unbind_case_fold_and_consume() {
    let mut program = KeymapProgram::<String, String>::compile(
        [
            binding(KeymapContext::Global, "a", KeymapAction::command("old")),
            binding(KeymapContext::Global, "a", KeymapAction::command("new")),
            KeymapBindingSnapshot {
                context: KeymapContext::Global,
                keystrokes: "a".to_owned(),
                action: KeymapAction::command("old"),
                kind: KeymapBindingKind::Unbind,
                source: KeymapBindingSource::User,
            },
            binding(KeymapContext::Global, "ctrl+catch_all", KeymapAction::None),
        ],
        action_name,
        context_name,
    )
    .0;

    let matched = program.next(
        KeymapInput::Key {
            key: KeymapPhysicalKey::A,
            modifiers: KeymapModifiers::default(),
            text: Some('A'),
        },
        |context| context == "Global",
    );
    assert_eq!(
        matched,
        KeymapMatch::Matched {
            action: "new".to_owned(),
            consumed: true
        }
    );

    let consumed = program.next(
        KeymapInput::Key {
            key: KeymapPhysicalKey::B,
            modifiers: KeymapModifiers {
                ctrl: true,
                ..KeymapModifiers::default()
            },
            text: Some('b'),
        },
        |context| context == "Global",
    );
    assert_eq!(consumed, KeymapMatch::Consumed);
}

#[test]
fn effective_file_layers_disable_exact_context_defaults_and_keep_user_entries() {
    let keymap = KeymapFile::parse(
        r#"[
            { "context": "Native", "use_builtin_defaults": false,
              "bindings": { "ctrl-j": "new_window" } }
        ]"#,
    )
    .expect("keymap parses");
    let builtins = vec![
        KeymapBindingSnapshot {
            context: KeymapContext::Global,
            keystrokes: "ctrl-k".to_owned(),
            action: KeymapAction::command("global"),
            kind: KeymapBindingKind::Binding,
            source: KeymapBindingSource::BuiltIn,
        },
        KeymapBindingSnapshot {
            context: KeymapContext::Native,
            keystrokes: "ctrl-k".to_owned(),
            action: KeymapAction::command("native"),
            kind: KeymapBindingKind::Binding,
            source: KeymapBindingSource::BuiltIn,
        },
    ];
    let effective = bootty_config::keymap::effective_bindings(&builtins, &keymap);
    assert!(effective.iter().any(|entry| {
        entry.context == KeymapContext::Global
            && entry.source == KeymapBindingSource::BuiltIn
            && entry.action.name() == Some("global")
    }));
    assert!(!effective.iter().any(|entry| {
        entry.context == KeymapContext::Native
            && entry.source == KeymapBindingSource::User
            && entry.action.name() == Some("native")
    }));
    assert!(effective.iter().any(|entry| {
        entry.context == KeymapContext::Native && entry.action.name() == Some("new_window")
    }));
}
