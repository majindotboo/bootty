#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::{cell::RefCell, rc::Rc};

use bootty_config::config::{AppearanceMode, InputConfig};
use bootty_control::Caller;
use bootty_terminal::terminal_input::ModifierSideState;
use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};
use bootty_ui::gpui::{DialogId, DialogIntent, Modifiers};
use bootty_ui::{
    action_catalog::Command,
    app_actions::AppKeyBindings,
    gpui_actions::{
        InvokeCommand, WORKSPACE_KEY_CONTEXT, binding_specs, key_bindings, workspace_key_context,
    },
    presentation::dialogs::{
        COMMAND_PALETTE_ID, CommandPaletteDialog, CommandPaletteEvent, CommandPaletteState,
    },
};
use gpui_kit::{
    AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement, Render,
    Styled as _, TestAppContext, Window, div,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn new_session_worktree_step_does_not_inherit_the_directory_filter() {
    use bootty_ui::presentation::dialogs::{NewSessionDialog, NewSessionPickerEvent};

    let directory = assert_fs::TempDir::new().expect("temporary session directory");
    let path = directory
        .path()
        .canonicalize()
        .expect("canonical session directory")
        .to_string_lossy()
        .into_owned();
    let occupied = vec![path.clone()];
    let mut dialog = NewSessionDialog::open();
    let id = dialog.spec().id;
    dialog.apply(
        &DialogIntent::TextChanged {
            dialog: id,
            value: path.clone(),
        },
        &occupied,
    );
    let activate = |spec: bootty_gpui::DialogSpec| {
        let row = spec
            .rows
            .iter()
            .find(|row| row.action.is_some())
            .expect("selectable choice");
        let action = row.action.as_ref().expect("choice action");
        DialogIntent::Activate {
            dialog: spec.id,
            row: row.id.clone(),
            action: action.id.clone(),
            payload: action.payload.clone(),
        }
    };
    assert!(dialog.apply(&activate(dialog.spec()), &occupied).is_none());
    let worktrees = dialog.spec();
    assert_eq!(worktrees.text.as_deref(), Some(""));
    assert!(matches!(
        dialog.apply(&activate(worktrees), &occupied),
        Some(NewSessionPickerEvent::CreateSession { cwd }) if cwd == path
    ));
}

fn input(keybinds: &[&str]) -> InputConfig {
    InputConfig {
        keybind: keybinds
            .iter()
            .map(|binding| (*binding).to_owned())
            .collect(),
        ..InputConfig::default()
    }
}

#[rstest]
#[case("workspace:window:1", "BoottyWorkspace_workspace_3Awindow_3A1")]
#[case("workspace-window-2", "BoottyWorkspace_workspace-window-2")]
fn workspace_key_context_is_stable_and_gpui_safe(#[case] key: &str, #[case] expected: &str) {
    assert_eq!(workspace_key_context(key), expected);
    assert!(gpui_kit::KeyContext::parse(&workspace_key_context(key)).is_ok());
    assert_ne!(
        workspace_key_context("window:1"),
        workspace_key_context("window:2")
    );
    assert_ne!(
        workspace_key_context("main_window_1"),
        workspace_key_context("main:window:1")
    );
    assert_eq!(
        workspace_key_context("window:1").split('_').next(),
        Some(WORKSPACE_KEY_CONTEXT)
    );
}

struct BindingProbe {
    key_context: String,
    focus: FocusHandle,
    received: Rc<RefCell<Vec<String>>>,
}

impl Render for BindingProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .key_context(self.key_context.as_str())
            .on_action(cx.listener(|this, action: &InvokeCommand, _, _| {
                this.received
                    .borrow_mut()
                    .push(action.invocation().action_name());
            }))
    }
}

#[rstest]
#[case("cmd+t=new_tab", "cmd-t", "new_tab", Vec::<String>::new())]
#[case("cmd+shift+]=next_tab", "shift-cmd-]", "next_tab", vec![])]
#[case("cmd+shift+[=previous_tab", "shift-cmd-[", "previous_tab", vec![])]
#[case(
    "cmd+shift+Enter=toggle_pane_zoom",
    "shift-cmd-enter",
    "toggle_pane_zoom",
    vec![]
)]
#[case(
    "cmd+shift+,=move_session:-1",
    "shift-cmd-,",
    "move_session",
    vec!["-1".to_owned()]
)]
fn direct_binding_becomes_a_typed_gpui_action(
    #[case] binding: &str,
    #[case] keystrokes: &str,
    #[case] command: &str,
    #[case] arguments: Vec<String>,
) {
    let specs = binding_specs(&input(&[binding])).unwrap();

    assert_eq!(specs.len(), 1);
    let expected = match keystrokes {
        "shift-cmd-]" => "cmd-}",
        "shift-cmd-[" => "cmd-{",
        "shift-cmd-," => "cmd-<",
        keystrokes => keystrokes,
    };
    assert_eq!(specs[0].keystrokes, expected);
    assert_eq!(specs[0].invocation.command, command);
    assert_eq!(specs[0].invocation.arguments, arguments);
    assert_eq!(specs[0].invocation.caller, Caller::Keybinding);
}

#[rstest]
fn leader_binding_becomes_a_gpui_chord() {
    let specs = binding_specs(&input(&["ctrl+a>c=new_tab"])).unwrap();

    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].keystrokes, "ctrl-a c");
    assert_eq!(specs[0].invocation.action_name(), "new_tab");
}

#[rstest]
#[case("left_cmd+t=new_tab")]
#[case("scroll_up=scroll_page_up")]
#[case("global:cmd+t=new_tab")]
#[case("all:cmd+t=new_tab")]
#[case("unconsumed:cmd+t=new_tab")]
#[case("performable:cmd+t=new_tab")]
fn bindings_without_exact_gpui_semantics_stay_in_the_legacy_resolver(#[case] binding: &str) {
    assert_eq!(
        binding_specs(&input(&[binding])).unwrap(),
        Vec::<bootty_ui::gpui_actions::GpuiBindingSpec>::new()
    );
}

#[rstest]
fn sided_modifier_binding_remains_runnable_through_the_legacy_resolver() {
    let mut bindings = AppKeyBindings::from_config(&input(&["left_cmd+t=new_tab"])).unwrap();
    let invocation = bindings
        .invocation_for_input(KeyInput {
            key: TerminalKey::T,
            mods: KeyMods {
                command: true,
                ..KeyMods::default()
            },
            repeat: false,
            utf8: None,
            unshifted: Some('t'),
        })
        .expect("left-command binding remains in legacy resolution");

    assert_eq!(invocation.action_name(), "new_tab");
}

#[rstest]
fn scroll_binding_remains_runnable_through_the_legacy_resolver() {
    let mut bindings = AppKeyBindings::from_config(&input(&["scroll_up=scroll_page_up"])).unwrap();
    let invocation = bindings
        .invocation_for_scroll_with_modifier_sides(
            true,
            Modifiers::default(),
            ModifierSideState::default(),
        )
        .expect("scroll binding remains in legacy resolution");

    assert_eq!(invocation.action_name(), "scroll_page_up");
}

#[rstest]
fn flagged_binding_remains_runnable_through_the_legacy_resolver() {
    let mut bindings = AppKeyBindings::from_config(&input(&["global:cmd+t=new_tab"])).unwrap();
    let invocation = bindings
        .invocation_for_input(KeyInput {
            key: TerminalKey::T,
            mods: KeyMods {
                command: true,
                ..KeyMods::default()
            },
            repeat: false,
            utf8: None,
            unshifted: Some('t'),
        })
        .expect("flagged binding remains in legacy resolution");

    assert_eq!(invocation.action_name(), "new_tab");
}

#[gpui_kit::test]
fn built_key_binding_keeps_the_typed_command_action(cx: &TestAppContext) {
    let input = input(&["cmd+t=new_tab"]);
    let expected = InvokeCommand::new(binding_specs(&input).unwrap()[0].invocation.clone());
    let bindings = cx.update(|cx| key_bindings(&input, cx).unwrap());

    assert!(bindings[0].action().partial_eq(&expected));
    assert_eq!(expected.invocation().action_name(), "new_tab");
    assert_eq!(expected.invocation().caller, Caller::Keybinding);
}

#[gpui_kit::test]
fn shifted_brackets_dispatch_tab_navigation(cx: &mut TestAppContext) {
    let input = input(&["cmd+shift+]=next_tab", "cmd+shift+[=previous_tab"]);
    cx.update(|cx| cx.bind_keys(key_bindings(&input, cx).unwrap()));
    let received = Rc::new(RefCell::new(Vec::new()));
    let window = cx.update(|cx| {
        let received = Rc::clone(&received);
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            cx.new(|_| BindingProbe {
                focus,
                received,
                key_context: WORKSPACE_KEY_CONTEXT.to_owned(),
            })
        })
        .expect("open binding probe")
    });

    cx.simulate_keystrokes(*window, "cmd-} cmd-{");

    assert_eq!(received.borrow().as_slice(), ["next_tab", "previous_tab"]);
}

#[rstest]
fn palette_filter_selects_and_runs_its_first_visible_command() {
    let mut palette = CommandPaletteDialog::open(&[], CommandPaletteState::default()).unwrap();

    assert_eq!(
        palette.apply(&DialogIntent::TextChanged {
            dialog: DialogId::new(COMMAND_PALETTE_ID),
            value: "split".to_owned(),
        }),
        None
    );

    let spec = palette.spec();
    let first = spec
        .rows
        .iter()
        .find(|row| row.enabled)
        .expect("matching palette result");
    assert_eq!(first.label, "Split Right");
    assert!(
        spec.rows
            .iter()
            .any(|row| !row.enabled && row.label == "Layout")
    );
    assert_eq!(palette.current_action(), Some("split_right"));
    let row = first.id.clone();
    let action = first.action.clone().expect("command action");

    assert_eq!(
        palette.apply(&DialogIntent::Activate {
            dialog: DialogId::new(COMMAND_PALETTE_ID),
            row,
            action: action.id,
            payload: action.payload,
        }),
        Some(CommandPaletteEvent::Run(Command::SplitRight))
    );
}

#[rstest]
fn palette_uses_the_first_configured_binding_for_a_command() {
    let palette = CommandPaletteDialog::open(
        &["cmd+t=new_tab".to_owned(), "ctrl+t=new_tab".to_owned()],
        CommandPaletteState::default(),
    )
    .unwrap();
    let row = palette
        .spec()
        .rows
        .into_iter()
        .find(|row| row.label == "New Tab")
        .expect("new tab palette row");
    assert_eq!(row.keybinding.as_deref(), Some("cmd+t"));
}

#[rstest]
fn palette_marks_the_current_appearance() {
    let palette = CommandPaletteDialog::open(
        &[],
        CommandPaletteState {
            appearance_mode: AppearanceMode::Dark,
            sidebar_visible: true,
        },
    )
    .unwrap();
    let rows = palette.spec().rows;

    let current = |label: &str| {
        rows.iter()
            .find(|row| row.label == label)
            .map(|row| row.current)
            .expect("palette command row")
    };
    assert!(current("Use Dark Appearance"));
    assert!(!current("Use System Appearance"));
    assert!(!current("Use Light Appearance"));
}

#[gpui_kit::test]
fn right_dock_defaults_dispatch_and_resolve_a_tooltip_hint(cx: &mut TestAppContext) {
    use bootty_config::config::load_config_from_path;
    use bootty_control::CommandInvocation;
    let action = InvokeCommand::new(CommandInvocation::from_action(
        "toggle_right_dock",
        Caller::Keybinding,
    ));
    for preset in ["bootty", "tmux", "ghostty"] {
        let file = assert_fs::NamedTempFile::new("config.toml").unwrap();
        std::fs::write(file.path(), format!("[input]\npreset = {preset:?}\n")).unwrap();
        let config = load_config_from_path(file.path()).unwrap();
        let context = workspace_key_context(preset);
        cx.update(|cx| {
            bootty_ui::gpui_actions::replace_workspace_key_bindings_for_context(
                &context,
                bootty_ui::gpui_actions::workspace_key_bindings(&config.input).unwrap(),
                cx,
            )
            .unwrap();
        });
        let received = Rc::new(RefCell::new(Vec::new()));
        let window = cx.update(|cx| {
            let received = received.clone();
            cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                cx.new(|_| BindingProbe {
                    focus,
                    received,
                    key_context: context.clone(),
                })
            })
            .unwrap()
        });
        let stroke = if cfg!(target_os = "macos") {
            "cmd-alt-b"
        } else {
            "ctrl-alt-b"
        };
        cx.simulate_keystrokes(*window, stroke);
        assert_eq!(
            received.borrow().as_slice(),
            ["toggle_right_dock"],
            "{preset}"
        );
        // Tooltip overlays do not inherit the workspace dispatch context.
        let overlay = cx.update(|cx| {
            cx.open_window(gpui_kit::WindowOptions::default(), |_, cx| {
                cx.new(|_| gpui_kit::Empty)
            })
            .unwrap()
        });
        overlay
            .update(cx, |_, window, _| {
                assert!(
                    gpui_kit::component::kbd::Kbd::binding_for_action(&action, None, window)
                        .is_none()
                );
                assert!(
                    gpui_kit::component::kbd::Kbd::binding_for_action(
                        &action,
                        Some(&context),
                        window
                    )
                    .is_some(),
                    "{preset} tooltip"
                );
            })
            .unwrap();
    }
}
