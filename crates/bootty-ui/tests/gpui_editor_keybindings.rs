#![cfg(test)]

use std::path::Path;

use bootty_config::config::InputConfig;
use bootty_ui::gpui::{CommandAction, FileEditor, UiPalette, init_theme};
use bootty_ui::gpui_actions::{
    InvokeCommand, WORKSPACE_KEY_CONTEXT, binding_specs, remove_workspace_key_bindings,
    replace_workspace_key_bindings, replace_workspace_key_bindings_for_context,
    workspace_key_bindings,
};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, KeyContext, Keystroke, Render, TestAppContext,
    Window,
};

struct TerminalTabProbe {
    terminal: gpui_kit::FocusHandle,
    control: gpui_kit::FocusHandle,
    keys: Vec<String>,
    commands: Vec<String>,
}

impl Render for TerminalTabProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _};
        gpui_kit::div()
            .size_full()
            .child(
                gpui_kit::div()
                    .id("terminal-probe")
                    .w_32()
                    .h_8()
                    .key_context(WORKSPACE_KEY_CONTEXT)
                    .track_focus(&self.terminal)
                    .tab_index(0_isize)
                    .on_action(cx.listener(|this, action: &InvokeCommand, _, _| {
                        this.commands.push(action.invocation().action_name());
                    }))
                    .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, _| {
                        this.keys.push(event.keystroke.key.clone());
                    })),
            )
            .child(
                gpui_kit::div()
                    .id("control-probe")
                    .w_32()
                    .h_8()
                    .track_focus(&self.control)
                    .tab_index(0_isize),
            )
    }
}

#[gpui_kit::test]
fn terminal_tab_reaches_key_input_but_controls_keep_root_focus_navigation(cx: &mut TestAppContext) {
    use bootty_ui::{
        commands::CommandCatalog,
        gpui_actions::key_bindings_for_snapshot,
        keymap_runtime::{KeymapFocus, KeymapSnapshot},
    };
    let mut snapshot = KeymapSnapshot {
        path: std::path::PathBuf::default(),
        keymap: bootty_config::keymap_file::KeymapFile::default(),
        effective_bindings: Vec::new(),
        diagnostics: Vec::new(),
        revision: 0,
    };
    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        replace_workspace_key_bindings(
            key_bindings_for_snapshot(
                &snapshot,
                KeymapFocus::Terminal,
                bootty_config::config::MultiplexerBackendConfig::Native,
                &CommandCatalog::default(),
            ),
            cx,
        )
        .unwrap();
    });
    let mut probe = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| TerminalTabProbe {
            terminal: cx.focus_handle().tab_stop(true),
            control: cx.focus_handle().tab_stop(true),
            keys: Vec::new(),
            commands: Vec::new(),
        });
        probe = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let probe = probe.unwrap();
    cx.update(|window, cx| probe.read(cx).terminal.clone().focus(window, cx));
    cx.simulate_keystrokes("tab shift-tab");
    cx.update(|window, cx| {
        let view = probe.read(cx);
        assert_eq!(view.keys, ["tab", "tab"]);
        assert!(view.terminal.is_focused(window));
        replace_workspace_key_bindings(
            key_bindings_for_snapshot(
                &snapshot,
                KeymapFocus::Other,
                bootty_config::config::MultiplexerBackendConfig::Native,
                &CommandCatalog::default(),
            ),
            cx,
        )
        .unwrap();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        assert!(probe.read(cx).control.is_focused(window));
        assert_eq!(probe.read(cx).keys.len(), 2);
    });

    snapshot
        .effective_bindings
        .push(bootty_ui::keymap_runtime::KeymapBindingSnapshot {
            context: bootty_config::keymap_file::KeymapContext::Terminal,
            keystrokes: "tab".to_owned(),
            action: bootty_config::keymap_file::KeymapAction::command("new_tab"),
            kind: bootty_config::keymap_file::KeymapBindingKind::default(),
            source: bootty_config::keymap_file::KeymapBindingSource::User,
        });
    cx.update(|window, cx| {
        replace_workspace_key_bindings(
            key_bindings_for_snapshot(
                &snapshot,
                KeymapFocus::Terminal,
                bootty_config::config::MultiplexerBackendConfig::Native,
                &CommandCatalog::default(),
            ),
            cx,
        )
        .unwrap();
        probe.read(cx).terminal.clone().focus(window, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| {
        assert_eq!(probe.read(cx).commands, ["new_tab"]);
        assert!(probe.read(cx).terminal.is_focused(window));
    });
}

struct EditorProbe {
    editor: Entity<FileEditor>,
}

#[gpui_kit::test]
fn command_context_remaps_and_unbinds_without_activating_workspace_shortcuts(cx: &TestAppContext) {
    use bootty_config::{
        config::MultiplexerBackendConfig,
        keymap_file::{KeymapAction, KeymapBindingKind, KeymapBindingSource, KeymapContext},
    };
    use bootty_ui::{
        commands::CommandCatalog,
        gpui_actions::key_bindings_for_snapshot,
        keymap_runtime::{KeymapBindingSnapshot, KeymapFocus, KeymapSnapshot},
    };
    let binding = |context, key: &str, action: &str, kind| KeymapBindingSnapshot {
        context,
        keystrokes: key.to_owned(),
        action: KeymapAction::command(action),
        kind,
        source: KeymapBindingSource::User,
    };
    let snapshot = KeymapSnapshot {
        path: std::path::PathBuf::default(),
        keymap: bootty_config::keymap_file::KeymapFile::default(),
        diagnostics: Vec::new(),
        revision: 0,
        effective_bindings: vec![
            binding(
                KeymapContext::Global,
                "cmd+n",
                "new_tab",
                KeymapBindingKind::Binding,
            ),
            binding(
                KeymapContext::Command,
                "ctrl+n",
                "ui.command.next",
                KeymapBindingKind::Unbind,
            ),
            binding(
                KeymapContext::Command,
                "ctrl+j",
                "ui.command.next",
                KeymapBindingKind::Binding,
            ),
            binding(
                KeymapContext::Command,
                "ctrl+shift+f",
                "ui.command.toggle_favorite",
                KeymapBindingKind::Binding,
            ),
        ],
    };
    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        let bindings = key_bindings_for_snapshot(
            &snapshot,
            KeymapFocus::Command,
            MultiplexerBackendConfig::Native,
            &CommandCatalog::default(),
        );
        let hints = bindings.command_hints(&CommandCatalog::default());
        assert_eq!(
            hints,
            vec![
                (CommandAction::Next, "ctrl-j".to_owned()),
                (CommandAction::ToggleFavorite, "ctrl-shift-f".to_owned()),
            ]
        );
        replace_workspace_key_bindings(bindings, cx).unwrap();
        let mut workspace = KeyContext::new_with_defaults();
        workspace.add(WORKSPACE_KEY_CONTEXT);
        let mut command = KeyContext::default();
        command.add("Command");
        let contexts = [workspace, command];
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        for (key, expected) in [
            ("ctrl-j", "ui.command.next"),
            ("ctrl-shift-f", "ui.command.toggle_favorite"),
        ] {
            let (bindings, pending) =
                keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &contexts);
            assert!(!pending);
            assert!(bindings.iter().any(|binding| {
                binding
                    .action()
                    .as_any()
                    .downcast_ref::<InvokeCommand>()
                    .is_some_and(|action| action.invocation().command == expected)
            }));
        }
        for key in ["cmd-n", "ctrl-n", "down"] {
            let (bindings, pending) =
                keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &contexts);
            assert!(!pending);
            assert!(bindings.is_empty(), "{key} must stay unbound in Command");
        }
    });
}

impl Render for EditorProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.editor.clone()
    }
}

#[gpui_kit::test]
fn workspace_keymap_reload_preserves_component_editor_deletion(cx: &mut TestAppContext) {
    cx.update(|cx| {
        init_theme(UiPalette::default(), cx);
        let input = InputConfig {
            keybind: vec!["backspace=new_tab".to_owned(), "delete=new_tab".to_owned()],
            ..InputConfig::default()
        };
        replace_workspace_key_bindings(
            workspace_key_bindings(&input).expect("build Bootty workspace bindings"),
            cx,
        )
        .expect("install Bootty workspace bindings");
    });
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            let editor = cx.new(|cx| {
                FileEditor::new_for_path("abc".to_owned(), Path::new("config.toml"), window, cx)
            });
            cx.new(|_| EditorProbe { editor })
        })
        .expect("open component editor")
    });
    window
        .update(cx, |probe, window, cx| {
            probe
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        })
        .expect("focus component editor");

    cx.simulate_keystrokes(*window, "delete");
    cx.simulate_input(*window, "x");
    cx.simulate_keystrokes(*window, "backspace");

    assert_eq!(
        window
            .update(cx, |probe, _, cx| probe.editor.read(cx).contents(cx))
            .expect("read component editor"),
        "bc"
    );
}

#[gpui_kit::test]
fn workspace_keymap_reload_removes_old_bindings(cx: &TestAppContext) {
    cx.update(|cx| {
        let input = InputConfig {
            keybind: vec!["cmd+x=new_tab".to_owned()],
            ..InputConfig::default()
        };
        replace_workspace_key_bindings(
            workspace_key_bindings(&input).expect("build original workspace binding"),
            cx,
        )
        .expect("install original workspace binding");
        replace_workspace_key_bindings(
            workspace_key_bindings(&InputConfig::default()).expect("build empty replacement"),
            cx,
        )
        .expect("install empty replacement");

        let mut context = KeyContext::new_with_defaults();
        context.add(WORKSPACE_KEY_CONTEXT);
        let (bindings, pending) = cx.key_bindings().borrow().bindings_for_input(
            &[Keystroke::parse("cmd-x").expect("parse keystroke")],
            &[context],
        );
        assert!(
            bindings.is_empty(),
            "removed action must not dispatch; resolved {:?}",
            bindings
                .iter()
                .map(|binding| binding.action().name())
                .collect::<Vec<_>>()
        );
        assert!(!pending, "removed action must not leave a chord pending");
    });
}

#[gpui_kit::test]
fn workspace_keymaps_remain_scoped_to_their_window_context(cx: &TestAppContext) {
    cx.update(|cx| {
        let first = InputConfig {
            keybind: vec!["cmd+x=new_tab".to_owned()],
            ..InputConfig::default()
        };
        let second = InputConfig {
            keybind: vec!["cmd+x=close_window".to_owned()],
            ..InputConfig::default()
        };
        replace_workspace_key_bindings_for_context(
            "BoottyWorkspace_window_one",
            workspace_key_bindings(&first).expect("build first workspace bindings"),
            cx,
        )
        .expect("install first workspace bindings");
        replace_workspace_key_bindings_for_context(
            "BoottyWorkspace_window_two",
            workspace_key_bindings(&second).expect("build second workspace bindings"),
            cx,
        )
        .expect("install second workspace bindings");

        for (context_name, expected) in [
            (
                "BoottyWorkspace_window_one",
                InvokeCommand::new(
                    binding_specs(&first).expect("first binding spec")[0]
                        .invocation
                        .clone(),
                ),
            ),
            (
                "BoottyWorkspace_window_two",
                InvokeCommand::new(
                    binding_specs(&second).expect("second binding spec")[0]
                        .invocation
                        .clone(),
                ),
            ),
        ] {
            let context =
                gpui_kit::KeyContext::parse(context_name).expect("parse workspace context");
            let (bindings, pending) = cx.key_bindings().borrow().bindings_for_input(
                &[gpui_kit::Keystroke::parse("cmd-x").expect("parse keystroke")],
                &[context],
            );
            assert!(
                !pending,
                "single-key binding must not leave a chord pending"
            );
            assert_eq!(bindings.len(), 1);
            assert!(bindings[0].action().partial_eq(&expected));
        }
    });
}

#[gpui_kit::test]
fn released_workspace_context_no_longer_resolves_bindings(cx: &TestAppContext) {
    let context = "BoottyWorkspace_released_window";
    let input = InputConfig {
        keybind: vec!["cmd+x=new_tab".to_owned()],
        ..InputConfig::default()
    };

    cx.update(|cx| {
        replace_workspace_key_bindings_for_context(
            context,
            workspace_key_bindings(&input).expect("build workspace binding"),
            cx,
        )
        .expect("install workspace binding");

        let key_context = KeyContext::parse(context).expect("parse workspace context");
        let (bindings, pending) = cx.key_bindings().borrow().bindings_for_input(
            &[Keystroke::parse("cmd-x").expect("parse keystroke")],
            &[key_context],
        );
        assert_eq!(bindings.len(), 1);
        assert!(!pending);

        remove_workspace_key_bindings(context, cx);

        let key_context = KeyContext::parse(context).expect("parse workspace context");
        let (bindings, pending) = cx.key_bindings().borrow().bindings_for_input(
            &[Keystroke::parse("cmd-x").expect("parse keystroke")],
            &[key_context],
        );
        assert!(
            bindings.is_empty(),
            "released context still resolves a binding"
        );
        assert!(!pending, "released context leaves a chord pending");
    });
}
