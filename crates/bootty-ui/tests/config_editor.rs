#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::{cell::RefCell, path::Path, rc::Rc};

use bootty_ui::gpui::{FileEditor, FileEditorEvent, UiPalette, init_theme};
use gpui_kit::component::highlighter::LanguageRegistry;
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Styled as _, Subscription, TestAppContext, Window,
};

struct ConfigEditorProbe {
    editor: Entity<FileEditor>,
    saves: Rc<RefCell<Vec<String>>>,
    _subscription: Subscription,
    host_saves: usize,
}

gpui_kit::actions!(
    editor_host,
    [
        #[derive(Eq)]
        SaveAtHost
    ]
);

#[gpui_kit::test]
fn clicking_the_editor_acquires_keyboard_focus(cx: &mut TestAppContext) {
    init(cx);
    let mut probe = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| ConfigEditorProbe::new("original text", window, cx));
        probe = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let probe = probe.expect("editor probe");
    cx.simulate_click(
        gpui_kit::point(gpui_kit::px(60.0), gpui_kit::px(40.0)),
        gpui_kit::Modifiers::none(),
    );
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input("replacement");
    probe.update(cx, |probe, cx| {
        assert_eq!(probe.editor.read(cx).contents(cx), "replacement");
    });
}

struct InputProbe {
    first: Entity<gpui_kit::component::input::InputState>,
    second: Entity<gpui_kit::component::input::InputState>,
}
impl Render for InputProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::input::Input;
        gpui_kit::div()
            .w(gpui_kit::px(300.0))
            .flex()
            .flex_col()
            .child(bootty_gpui::focus_input(
                &self.first,
                Input::new(&self.first),
            ))
            .child(bootty_gpui::focus_input(
                &self.second,
                Input::new(&self.second),
            ))
    }
}

#[gpui_kit::test]
fn pointer_focus_moves_between_form_fields(cx: &mut TestAppContext) {
    use gpui_kit::component::input::InputState;
    init(cx);
    let mut probe = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| InputProbe {
            first: cx.new(|cx| InputState::new(window, cx).default_value("first")),
            second: cx.new(|cx| InputState::new(window, cx).default_value("second")),
        });
        probe = Some(view.clone());
        gpui_kit::component::Root::new(view, window, cx)
    });
    let probe = probe.expect("form probe");
    cx.simulate_click(
        gpui_kit::point(gpui_kit::px(50.0), gpui_kit::px(15.0)),
        gpui_kit::Modifiers::none(),
    );
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input("one");
    cx.simulate_click(
        gpui_kit::point(gpui_kit::px(50.0), gpui_kit::px(47.0)),
        gpui_kit::Modifiers::none(),
    );
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    });
    cx.simulate_input("two");
    probe.update(cx, |probe, cx| {
        pretty_assertions::assert_eq!(probe.first.read(cx).value().as_str(), "one");
        pretty_assertions::assert_eq!(probe.second.read(cx).value().as_str(), "two");
    });
}

impl ConfigEditorProbe {
    fn new(contents: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::for_path(contents, Path::new("config.toml"), window, cx)
    }

    fn for_path(contents: &str, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| FileEditor::new_for_path(contents.to_owned(), path, window, cx));
        let saves = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::clone(&saves);
        let subscription = cx.subscribe(&editor, move |_, _, event, _| match event {
            FileEditorEvent::Save { contents } => {
                received.borrow_mut().push(contents.clone());
            }
        });
        Self {
            editor,
            saves,
            _subscription: subscription,
            host_saves: 0,
        }
    }
}

#[gpui_kit::test]
fn keymap_json_uses_the_same_component_editor_and_save_contract(cx: &mut TestAppContext) {
    init(cx);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            cx.new(|cx| {
                ConfigEditorProbe::for_path(
                    "[{\n  \"bindings\": {}\n}]\n",
                    Path::new("keymap.json"),
                    window,
                    cx,
                )
            })
        })
        .expect("open keymap file editor")
    });
    window
        .update(cx, |probe, window, cx| {
            probe
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        })
        .expect("focus keymap file editor");

    cx.simulate_input(*window, "// user keymap\n");
    cx.simulate_keystrokes(*window, save_shortcut());

    window
        .update(cx, |probe, _, cx| {
            let contents = probe.editor.read(cx).contents(cx);
            assert!(contents.contains("// user keymap"));
            assert_eq!(probe.saves.borrow().as_slice(), [contents]);
        })
        .expect("read keymap file editor");
}

#[gpui_kit::test]
fn focused_file_editor_routes_backspace_and_delete_to_the_document(cx: &mut TestAppContext) {
    init(cx);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            cx.new(|cx| ConfigEditorProbe::new("abc", window, cx))
        })
        .expect("open file editor")
    });
    window
        .update(cx, |probe, window, cx| {
            probe
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        })
        .expect("focus file editor");

    // EditorState starts at the beginning of the document, so Delete removes the leading byte.
    cx.simulate_keystrokes(*window, "delete");
    assert_eq!(
        window
            .update(cx, |probe, _, cx| probe.editor.read(cx).contents(cx))
            .expect("read after Delete"),
        "bc"
    );

    cx.simulate_input(*window, "x");
    cx.simulate_keystrokes(*window, "backspace");
    assert_eq!(
        window
            .update(cx, |probe, _, cx| probe.editor.read(cx).contents(cx))
            .expect("read after Backspace"),
        "bc"
    );
}

impl Render for ConfigEditorProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui_kit::div()
            .size_full()
            .key_context("FileEditorHost")
            .on_action(cx.listener(|this, _: &SaveAtHost, _, _| {
                this.host_saves = this.host_saves.checked_add(1).expect("save count fits");
            }))
            .child(self.editor.clone())
    }
}

#[gpui_kit::test]
fn document_save_takes_precedence_over_the_host_after_keymap_reload(cx: &mut TestAppContext) {
    init(cx);
    let shortcut = save_shortcut();
    cx.update(|cx| {
        cx.bind_keys([gpui_kit::KeyBinding::new(
            shortcut,
            SaveAtHost,
            Some("FileEditorHost"),
        )]);
        bootty_ui::gpui_actions::replace_workspace_key_bindings(
            bootty_ui::gpui_actions::workspace_key_bindings(
                &bootty_config::config::InputConfig::default(),
            )
            .unwrap(),
            cx,
        )
        .unwrap();
    });
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            cx.new(|cx| ConfigEditorProbe::new("before", window, cx))
        })
        .unwrap()
    });
    window
        .update(cx, |probe, window, cx| {
            probe
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        })
        .unwrap();
    cx.simulate_input(*window, "edit");
    cx.simulate_keystrokes(*window, shortcut);
    window
        .update(cx, |probe, _, cx| {
            pretty_assertions::assert_eq!(probe.host_saves, 0);
            pretty_assertions::assert_eq!(
                probe.saves.borrow().as_slice(),
                [probe.editor.read(cx).contents(cx)]
            );
        })
        .unwrap();
}

const fn save_shortcut() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    }
}

fn init(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
}

#[gpui_kit::test]
fn component_editor_has_real_json_and_toml_grammars(cx: &TestAppContext) {
    init(cx);
    for language in ["json", "toml"] {
        let config = LanguageRegistry::singleton()
            .language(language)
            .unwrap_or_else(|| panic!("{language} must be registered"));
        assert!(config.has_grammar(), "{language} must have a grammar");
        assert!(
            !config.highlights.is_empty(),
            "{language} must have a syntax highlight query"
        );
    }
}

#[gpui_kit::test]
fn file_editor_edits_and_emits_the_exact_revision_to_save(cx: &mut TestAppContext) {
    init(cx);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            cx.new(|cx| ConfigEditorProbe::new("theme = \"dark\"\n", window, cx))
        })
        .expect("open config editor")
    });
    window
        .update(cx, |probe, window, cx| {
            probe
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        })
        .expect("focus config editor");

    cx.simulate_input(*window, "# Bootty\n");
    cx.simulate_keystrokes(*window, save_shortcut());

    window
        .update(cx, |probe, _, cx| {
            let contents = probe.editor.read(cx).contents(cx);
            assert!(probe.editor.read(cx).is_dirty(cx));
            assert_eq!(probe.saves.borrow().as_slice(), [contents]);
            assert!(probe.editor.read(cx).save_in_flight());
        })
        .expect("read config editor");
}

#[gpui_kit::test]
fn save_is_available_only_for_an_unsaved_revision(cx: &mut TestAppContext) {
    init(cx);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            cx.new(|cx| ConfigEditorProbe::new("theme = \"dark\"\n", window, cx))
        })
        .expect("open config editor")
    });
    window
        .update(cx, |probe, window, cx| {
            assert!(!probe.editor.read(cx).can_save(cx));
            probe
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        })
        .expect("focus config editor");

    cx.simulate_input(*window, "# edited\n");
    let persisted = window
        .update(cx, |probe, _, cx| {
            assert!(probe.editor.read(cx).can_save(cx));
            probe
                .editor
                .update(cx, bootty_gpui::FileEditor::request_save);
            assert!(!probe.editor.read(cx).can_save(cx));
            probe.editor.read(cx).contents(cx)
        })
        .expect("request save");

    window
        .update(cx, |probe, _, cx| {
            probe.editor.update(cx, |editor, cx| {
                editor.mark_saved(persisted, None, cx);
            });
            assert!(!probe.editor.read(cx).can_save(cx));
            assert!(!probe.editor.read(cx).is_dirty(cx));
        })
        .expect("finish save");
}

#[gpui_kit::test]
fn saving_one_revision_does_not_clear_newer_edits(cx: &mut TestAppContext) {
    init(cx);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            cx.new(|cx| ConfigEditorProbe::new("shell = \"zsh\"\n", window, cx))
        })
        .expect("open config editor")
    });
    window
        .update(cx, |probe, window, cx| {
            probe
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx));
        })
        .expect("focus config editor");
    cx.simulate_input(*window, "first");
    let persisted = window
        .update(cx, |probe, _, cx| {
            probe
                .editor
                .update(cx, bootty_gpui::FileEditor::request_save);
            probe.editor.read(cx).contents(cx)
        })
        .expect("request save");
    cx.simulate_input(*window, "second");
    window
        .update(cx, |probe, _, cx| {
            probe.editor.update(cx, |editor, cx| {
                editor.mark_saved(persisted, None, cx);
            });
            assert!(probe.editor.read(cx).is_dirty(cx));
            assert!(!probe.editor.read(cx).save_in_flight());
        })
        .expect("finish older save");
}
