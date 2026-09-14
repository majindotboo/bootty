use assert_fs::{TempDir, prelude::*};
use bootty_config::keymap_file::{
    KeymapAction, KeymapBindingKind, KeymapBindingSource, KeymapBindingTarget, KeymapContext,
    KeymapEdit, KeymapFile, update_keymap_jsonc, write_keymap_edit,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::json;

#[rstest]
fn jsonc_sections_preserve_order_and_action_shapes() {
    let source = r#"
// Bootty user keymap
[
  {
    "context": "Terminal",
    "use_key_equivalents": true,
    "unbind": { "ctrl-k": "open_settings", },
    "bindings": {
      "ctrl-k": null,
      "ctrl-j": "new_tab",
      "ctrl-l": ["select_tab", 3],
    },
  },
]
"#;

    let keymap = KeymapFile::parse(source).expect("parse JSONC keymap");
    let section = keymap.sections().next().expect("one section");

    assert_eq!(section.context, KeymapContext::Terminal);
    assert!(section.use_key_equivalents);
    assert_eq!(section.unbind[0].keystrokes, "ctrl-k");
    assert_eq!(section.bindings[0].action, KeymapAction::None);
    assert_eq!(section.bindings[1].action, KeymapAction::command("new_tab"));
    assert_eq!(
        section.bindings[2].action,
        KeymapAction::command_with_input("select_tab", json!(3))
    );
    assert_eq!(keymap.diagnostics(), []);
}

#[rstest]
fn builtin_default_policy_is_optional_context_local_and_last_explicit_wins() {
    let source = r#"[
      { "context": "Terminal", "use_builtin_defaults": false },
      { "context": "Sidebar", "use_builtin_defaults": false },
      { "context": "Terminal", "bindings": { "ctrl-a": "new_tab" } },
      { "context": "Terminal", "use_builtin_defaults": true }
    ]"#;

    let keymap = KeymapFile::parse(source).expect("parse built-in policy");

    assert!(keymap.use_builtin_defaults(&KeymapContext::Terminal));
    assert!(!keymap.use_builtin_defaults(&KeymapContext::Sidebar));
    assert!(keymap.use_builtin_defaults(&KeymapContext::Global));
}

#[rstest]
fn malformed_builtin_default_policy_inherits_true_without_hiding_bindings() {
    let source = r#"[
      {
        "context": "Terminal",
        "use_builtin_defaults": "no",
        "bindings": { "ctrl-a": "new_tab" }
      }
    ]"#;

    let keymap = KeymapFile::parse(source).expect("parse section around malformed policy");

    assert!(keymap.use_builtin_defaults(&KeymapContext::Terminal));
    assert_eq!(
        keymap
            .sections()
            .next()
            .expect("terminal section")
            .bindings
            .len(),
        1
    );
    assert_eq!(keymap.diagnostics().len(), 1);
}

#[rstest]
fn malformed_sections_and_entries_do_not_hide_valid_neighbors() {
    let source = r#"[
      { "context": "Somewhere", "bindings": { "ctrl-a": "new_tab" } },
      {
        "context": "Terminal",
        "use_key_equivalents": "yes",
        "bindings": {
          "ctrl-b": ["new_tab"],
          "ctrl-c": "new_tab"
        }
      },
      42
    ]"#;

    let keymap = KeymapFile::parse(source).expect("valid root still parses");

    assert_eq!(keymap.sections().count(), 2);
    assert_eq!(
        keymap
            .sections()
            .next()
            .expect("expression section")
            .context,
        KeymapContext::Expression("Somewhere".to_owned())
    );
    assert_eq!(
        keymap.sections().nth(1).expect("valid section").bindings,
        vec![bootty_config::keymap_file::KeymapEntry {
            keystrokes: "ctrl-c".to_owned(),
            action: KeymapAction::command("new_tab"),
        }]
    );
    assert_eq!(keymap.diagnostics().len(), 3);
}

#[rstest]
#[case("keep this file comment")]
#[case("保留 🦀 — quoted \"text\" and \\slashes")]
fn add_and_replace_preserve_unedited_jsonc(#[case] comment: &str) {
    let source = r#"// keep this file comment
[
  // keep this section comment
  {
    "context": "Terminal",
    "bindings": { "ctrl-a": "new_tab" }
  }
]
"#;
    let source = source.replace("keep this file comment", comment);
    let target = KeymapBindingTarget::binding(
        KeymapContext::Terminal,
        "ctrl-a",
        KeymapAction::command("new_tab"),
    );
    let replacement = KeymapBindingTarget::binding(
        KeymapContext::Terminal,
        "ctrl-b",
        KeymapAction::command("previous_tab"),
    );

    let replaced = update_keymap_jsonc(
        &source,
        &KeymapEdit::replace(target, replacement, KeymapBindingSource::User),
    )
    .expect("replace user binding");
    let added = update_keymap_jsonc(
        &replaced,
        &KeymapEdit::add(KeymapBindingTarget::binding(
            KeymapContext::Tmux,
            "ctrl-c",
            KeymapAction::command_with_input("move_tab", json!(-1)),
        )),
    )
    .expect("add user binding");

    assert!(added.contains(&format!("// {comment}")));
    assert!(added.contains("// keep this section comment"));
    let keymap = KeymapFile::parse(&added).expect("updated JSONC remains valid");
    assert_eq!(keymap.sections().count(), 2);
    assert_eq!(
        keymap.sections().next().expect("replacement").bindings[0].keystrokes,
        "ctrl-b"
    );
}

#[rstest]
fn edits_preserve_legacy_trigger_flags_and_multi_step_triggers() {
    let source = r#"[
      { "bindings": { "performable:unconsumed:left_ctrl+k scroll_up": "new_tab" } }
    ]"#;
    let target = KeymapBindingTarget::binding(
        KeymapContext::Global,
        "performable:unconsumed:left_ctrl+k scroll_up",
        KeymapAction::command("new_tab"),
    );
    let replacement = KeymapBindingTarget::binding(
        KeymapContext::Global,
        "performable:unconsumed:right_ctrl+j scroll_down",
        KeymapAction::command("previous_tab"),
    );

    let updated = update_keymap_jsonc(
        source,
        &KeymapEdit::replace(target, replacement, KeymapBindingSource::User),
    )
    .expect("replace capable trigger");
    let keymap = KeymapFile::parse(&updated).expect("updated keymap parses");
    let binding = &keymap.sections().next().expect("section").bindings[0];

    assert_eq!(
        binding.keystrokes,
        "performable:unconsumed:right_ctrl+j scroll_down"
    );
}

#[rstest]
fn removing_a_builtin_binding_appends_a_targeted_unbind() {
    let target = KeymapBindingTarget::binding(
        KeymapContext::Global,
        "ctrl-k",
        KeymapAction::command("open_settings"),
    );

    let updated = update_keymap_jsonc(
        "[\n]\n",
        &KeymapEdit::remove(target, KeymapBindingSource::BuiltIn),
    )
    .expect("append suppression");
    let keymap = KeymapFile::parse(&updated).expect("parse updated keymap");
    let section = keymap.sections().next().expect("suppression section");

    assert_eq!(section.context, KeymapContext::Global);
    assert_eq!(section.unbind.len(), 1);
    assert_eq!(
        section.unbind[0].action,
        KeymapAction::command("open_settings")
    );
}

#[rstest]
fn removing_one_grouped_user_binding_keeps_the_other_entry() {
    let source = r#"[
      { "bindings": { "ctrl-a": "new_tab", "ctrl-b": "previous_tab" } }
    ]"#;
    let target = KeymapBindingTarget {
        context: KeymapContext::Global,
        keystrokes: "ctrl-a".to_owned(),
        action: KeymapAction::command("new_tab"),
        kind: KeymapBindingKind::Binding,
    };

    let updated = update_keymap_jsonc(
        source,
        &KeymapEdit::remove(target, KeymapBindingSource::User),
    )
    .expect("remove grouped binding");
    let section = KeymapFile::parse(&updated)
        .expect("parse updated keymap")
        .sections()
        .next()
        .expect("remaining section")
        .clone();

    assert_eq!(section.bindings.len(), 1);
    assert_eq!(section.bindings[0].keystrokes, "ctrl-b");
}

#[rstest]
fn moving_a_builtin_binding_to_another_context_suppresses_the_original() {
    let target = KeymapBindingTarget::binding(
        KeymapContext::Global,
        "ctrl-k",
        KeymapAction::command("new_tab"),
    );
    let replacement = KeymapBindingTarget::binding(
        KeymapContext::Terminal,
        "ctrl-k",
        KeymapAction::command("new_tab"),
    );
    let updated = update_keymap_jsonc(
        "[]",
        &KeymapEdit::replace(target, replacement, KeymapBindingSource::BuiltIn),
    )
    .expect("move built-in binding");
    let keymap = KeymapFile::parse(&updated).expect("parse edited keymap");

    let original = keymap
        .sections()
        .find(|section| section.context == KeymapContext::Global)
        .expect("original context is suppressed");
    assert_eq!(original.unbind[0].action, KeymapAction::command("new_tab"));
    assert_eq!(original.unbind[0].keystrokes, "ctrl-k");
    let moved = keymap
        .sections()
        .find(|section| section.context == KeymapContext::Terminal)
        .expect("new terminal binding");
    assert_eq!(moved.bindings[0].action, KeymapAction::command("new_tab"));
    assert_eq!(moved.bindings[0].keystrokes, "ctrl-k");

    let builtin = bootty_config::KeymapBindingSnapshot {
        context: KeymapContext::Global,
        keystrokes: "ctrl-k".to_owned(),
        action: KeymapAction::command("new_tab"),
        kind: KeymapBindingKind::Binding,
        source: KeymapBindingSource::BuiltIn,
    };
    let (mut program, diagnostics) = bootty_config::KeymapProgram::compile(
        bootty_config::keymap::effective_bindings(&[builtin], &keymap),
        |action| Ok(action.name().map(str::to_owned)),
        |context| Ok(context.clone()),
    );
    assert_eq!(
        diagnostics,
        Vec::<bootty_config::keymap_file::KeymapDiagnostic>::new()
    );
    let sequence = bootty_config::parse_keymap_sequence("ctrl-k").expect("key sequence");
    assert_eq!(
        program.next_candidates(&sequence.triggers, |context| *context
            == KeymapContext::Global),
        bootty_config::KeymapMatch::NoMatch,
    );
    assert_eq!(
        program.next_candidates(&sequence.triggers, |context| matches!(
            context,
            KeymapContext::Global | KeymapContext::Terminal
        )),
        bootty_config::KeymapMatch::Matched {
            action: "new_tab".to_owned(),
            consumed: true
        },
    );
}

#[rstest]
fn resetting_a_context_removes_its_user_entries_only() {
    let source = r#"[
      { "bindings": { "ctrl-a": "new_tab" } },
      { "context": "Terminal", "bindings": { "ctrl-b": "previous_tab" } },
      { "context": "Sidebar", "unbind": { "ctrl-c": "open_settings" } }
    ]"#;

    let updated = update_keymap_jsonc(source, &KeymapEdit::reset_context(KeymapContext::Terminal))
        .expect("reset terminal context");
    let keymap = KeymapFile::parse(&updated).expect("parse reset keymap");
    let sections = keymap.sections().collect::<Vec<_>>();

    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].context, KeymapContext::Global);
    assert_eq!(sections[1].context, KeymapContext::Sidebar);
    assert_eq!(sections[0].binding_count(), 1);
    assert_eq!(sections[1].binding_count(), 1);
}

#[rstest]
fn resetting_a_context_preserves_its_non_binding_options() {
    let source = r#"[
      {
        "context": "Terminal",
        "use_key_equivalents": true,
        "unbind": { "ctrl-a": "new_tab" },
        "bindings": { "ctrl-b": "previous_tab" }
      }
    ]"#;

    let updated = update_keymap_jsonc(source, &KeymapEdit::reset_context(KeymapContext::Terminal))
        .expect("reset terminal context");
    let keymap = KeymapFile::parse(&updated).expect("parse reset keymap");
    let section = keymap.sections().next().expect("preserved context section");

    assert_eq!(section.context, KeymapContext::Terminal);
    assert!(section.use_key_equivalents);
    assert_eq!(section.binding_count(), 0);
    assert!(updated.contains("use_key_equivalents"));
}

#[rstest]
#[case::remove(false)]
#[case::reset(true)]
fn removing_bindings_preserves_disabled_builtin_defaults(#[case] reset: bool) {
    let source = r#"[
      {
        "context": "Terminal",
        "use_builtin_defaults": false,
        "bindings": { "ctrl-b": "previous_tab" }
      }
    ]"#;
    let edit = if reset {
        KeymapEdit::reset_context(KeymapContext::Terminal)
    } else {
        KeymapEdit::remove(
            KeymapBindingTarget::binding(
                KeymapContext::Terminal,
                "ctrl-b",
                KeymapAction::command("previous_tab"),
            ),
            KeymapBindingSource::User,
        )
    };

    let updated = update_keymap_jsonc(source, &edit).expect("remove terminal bindings");
    let keymap = KeymapFile::parse(&updated).expect("parse updated keymap");

    assert!(!keymap.use_builtin_defaults(&KeymapContext::Terminal));
    assert_eq!(
        keymap
            .sections()
            .map(bootty_config::keymap_file::KeymapSection::binding_count)
            .sum::<usize>(),
        0
    );
}

#[rstest]
fn setting_builtin_defaults_preserves_bindings_unbinds_and_other_options() {
    let source = r#"// keep the file comment
[
  {
    "context": "Terminal",
    "use_key_equivalents": true,
    "unbind": { "ctrl-a": "new_tab" },
    "bindings": { "ctrl-b": "previous_tab" }
  }
]
"#;

    let updated = update_keymap_jsonc(
        source,
        &KeymapEdit::set_builtin_defaults(KeymapContext::Terminal, false),
    )
    .expect("disable terminal defaults");
    let keymap = KeymapFile::parse(&updated).expect("parse updated policy");
    let section = keymap.sections().next().expect("terminal section");

    assert!(updated.contains("// keep the file comment"));
    assert!(!keymap.use_builtin_defaults(&KeymapContext::Terminal));
    assert!(section.use_key_equivalents);
    assert_eq!(section.unbind.len(), 1);
    assert_eq!(section.bindings.len(), 1);
}

#[rstest]
fn setting_builtin_defaults_appends_an_option_only_section_for_a_new_context() {
    let source = r#"[
      { "bindings": { "ctrl-a": "new_tab" } }
    ]"#;

    let updated = update_keymap_jsonc(
        source,
        &KeymapEdit::set_builtin_defaults(KeymapContext::Sidebar, false),
    )
    .expect("disable sidebar defaults");
    let keymap = KeymapFile::parse(&updated).expect("parse updated policy");
    let sections = keymap.sections().collect::<Vec<_>>();

    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].binding_count(), 1);
    assert_eq!(sections[1].context, KeymapContext::Sidebar);
    assert_eq!(sections[1].binding_count(), 0);
    assert!(!keymap.use_builtin_defaults(&KeymapContext::Sidebar));
}

#[rstest]
fn keymap_edits_create_the_identity_sibling_file_atomically() {
    let directory = TempDir::new().expect("temporary config directory");
    let keymap = directory.child("nested/keymap.json");
    let edit = KeymapEdit::add(KeymapBindingTarget::binding(
        KeymapContext::Sidebar,
        "j",
        KeymapAction::command("ui.sidebar.next_session"),
    ));

    let outcome = write_keymap_edit(keymap.path(), &edit).expect("write keymap");

    assert_eq!(outcome.durability_warning(), None);
    assert!(keymap.path().is_file());
    let parsed = KeymapFile::parse(&std::fs::read_to_string(keymap.path()).expect("read keymap"))
        .expect("written keymap parses");
    assert_eq!(
        parsed.sections().next().expect("section").context,
        KeymapContext::Sidebar
    );
}
