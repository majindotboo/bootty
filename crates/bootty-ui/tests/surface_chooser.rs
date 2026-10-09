use std::{cell::RefCell, rc::Rc};

use bootty_control::{CommandInvocation, CommandTarget, ResourceKind};
use bootty_ui::{
    SurfaceCommand, SurfacePanel,
    gpui::{UiPalette, init_theme},
    surface_creation::{PendingNewSurface, SurfaceParent, SurfacePlacement},
};
use gpui_kit::{Focusable as _, Modifiers, TestAppContext};
use pretty_assertions::assert_eq;

fn request() -> PendingNewSurface {
    let binding = CommandTarget {
        kind: ResourceKind::Binding,
        handle: "captured-binding".to_owned(),
        generation: 7,
    };
    PendingNewSurface {
        id: 41,
        binding: binding.clone(),
        task_identity: "saved-task".to_owned(),
        cwd: "/tmp/project".to_owned(),
        parent: SurfaceParent::Binding(binding),
        placement: SurfacePlacement::Tab,
    }
}

struct ChooserProbe {
    panel: gpui_kit::Entity<SurfacePanel>,
    _subscription: gpui_kit::Subscription,
}

impl gpui_kit::Render for ChooserProbe {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _};
        gpui_kit::div()
            .size_full()
            .key_context(bootty_ui::gpui_actions::WORKSPACE_KEY_CONTEXT)
            .child(self.panel.clone())
    }
}

fn chooser_window(
    cx: &mut TestAppContext,
    received: Rc<RefCell<Vec<CommandInvocation>>>,
) -> (
    gpui_kit::Entity<SurfacePanel>,
    &mut gpui_kit::VisualTestContext,
) {
    chooser_window_with_config(
        cx,
        received,
        &bootty_config::config::BoottyConfig::default(),
    )
}

#[expect(
    clippy::expect_used,
    reason = "Abort invalid test fixture setup before exercising the chooser"
)]
fn chooser_window_with_config<'a>(
    cx: &'a mut TestAppContext,
    received: Rc<RefCell<Vec<CommandInvocation>>>,
    config: &bootty_config::config::BoottyConfig,
) -> (
    gpui_kit::Entity<SurfacePanel>,
    &'a mut gpui_kit::VisualTestContext,
) {
    use bootty_ui::{
        commands::{
            CommandCatalog, CommandExecutor, CoreCommandExecutor, SurfaceCommand as Command,
        },
        gpui_actions::{key_bindings_for_snapshot, replace_workspace_key_bindings},
        keymap_runtime::{KeymapFocus, KeymapRuntime},
    };
    use gpui_kit::AppContext as _;
    let catalog = std::sync::Arc::new(CommandCatalog::default());
    let runtime = KeymapRuntime::new(config, catalog.clone());
    assert_eq!(runtime.snapshot().diagnostics, []);
    cx.update(|cx| {
        replace_workspace_key_bindings(
            key_bindings_for_snapshot(
                runtime.snapshot(),
                KeymapFocus::SurfaceChooser,
                bootty_config::config::MultiplexerBackendConfig::Native,
                &catalog,
            ),
            cx,
        )
        .expect("chooser bindings");
    });
    let (host, visual) = cx.add_window_view(|window, cx| {
        let panel = cx.new(|cx| SurfacePanel::chooser(request(), cx));
        panel.read(cx).focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(
            &panel,
            window,
            move |_, panel, event: &SurfaceCommand, window, cx| {
                let command = catalog
                    .resolve(event.0.clone())
                    .expect("catalogued chooser command");
                if let CommandExecutor::Core(CoreCommandExecutor::Surface(Command::Navigate(
                    action,
                ))) = command.executor
                {
                    panel.update(cx, |panel, cx| panel.navigate(action, window, cx));
                } else {
                    received.borrow_mut().push(event.0.clone());
                }
            },
        );
        ChooserProbe {
            panel,
            _subscription: subscription,
        }
    });
    let panel = visual.read(|cx| host.read(cx).panel.clone());
    (panel, visual)
}

#[gpui_kit::test]
fn chooser_buttons_and_keyboard_use_the_same_captured_command(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let options = [
        (
            "surface-choose-agent",
            "surface.choose",
            vec!["41", "agent"],
        ),
        (
            "surface-choose-terminal",
            "surface.choose",
            vec!["41", "terminal"],
        ),
        ("claude", "surface.choose", vec!["41", "profile", "claude"]),
        ("codex", "surface.choose", vec!["41", "profile", "codex"]),
        ("pi", "surface.choose", vec!["41", "profile", "pi"]),
        (
            "surface-edit-profiles",
            "open_setting",
            vec!["agents.codex.enabled"],
        ),
    ];
    for (index, (selector, command, args)) in options.into_iter().enumerate() {
        let received = Rc::new(RefCell::new(Vec::<CommandInvocation>::new()));
        let (view, cx) = chooser_window(cx, received.clone());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let button = cx.debug_bounds(selector).expect("actual chooser button");
        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked();
        let pointer = received.borrow_mut().pop().expect("choice command");
        assert_eq!(pointer.command, command);
        assert_eq!(pointer.arguments, args);
        assert!(received.borrow().is_empty(), "one invocation per click");
        cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
        for key in ["down", "j", "ctrl-n"] {
            cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
            for _ in 0..index {
                cx.simulate_keystrokes(key);
            }
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert_eq!(
                received.borrow_mut().pop().expect("navigation choice"),
                pointer
            );
            assert!(
                received.borrow().is_empty(),
                "one invocation per navigation"
            );
        }
        cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
        cx.simulate_keystrokes(
            ["a", "t", "1", "2", "3", "e"]
                .get(index)
                .copied()
                .expect("choice shortcut"),
        );
        cx.run_until_parked();
        assert_eq!(
            received.borrow_mut().pop().expect("direct shortcut"),
            pointer
        );
        assert!(received.borrow().is_empty(), "one invocation per shortcut");
        cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
        for _ in 0..index {
            cx.simulate_keystrokes("tab");
        }
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            received.borrow_mut().pop().expect("tab activation"),
            pointer
        );
        assert!(received.borrow().is_empty(), "one invocation after Tab");
        cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
        for _ in index..6 {
            cx.simulate_keystrokes("shift-tab");
        }
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            received.borrow_mut().pop().expect("reverse tab activation"),
            pointer
        );
        assert!(
            received.borrow().is_empty(),
            "one invocation after Shift-Tab"
        );
    }
}

#[gpui_kit::test]
fn chooser_keeps_provider_artwork_and_escape_cancels_without_a_choice(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let received = Rc::new(RefCell::new(Vec::new()));
    let (_view, cx) = chooser_window(cx, received.clone());
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for (button, icon) in [
        ("surface-choose-agent", "surface-icon-surface-choose-agent"),
        (
            "surface-choose-terminal",
            "surface-icon-surface-choose-terminal",
        ),
        ("claude", "surface-icon-claude"),
        ("codex", "surface-icon-codex"),
        ("pi", "surface-icon-pi"),
    ] {
        let row = cx.debug_bounds(button).expect("choice row");
        let artwork = cx.debug_bounds(icon).expect("actual artwork in choice row");
        assert!(row.contains(&artwork.center()));
        assert!(artwork.size.width > gpui_kit::px(0.0));
    }
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(received.borrow().len(), 1);
    assert_eq!(received.borrow()[0].command, "surface.cancel");
    assert_eq!(received.borrow()[0].arguments, ["41"]);
}

#[gpui_kit::test]
fn chooser_rebinding_disables_defaults_without_hidden_fallbacks(cx: &mut TestAppContext) {
    use assert_fs::prelude::*;
    use bootty_config::config::load_config_from_path;
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let directory = assert_fs::TempDir::new().expect("private keymap");
    directory.child("config.toml").write_str("").unwrap();
    directory.child("keymap.json").write_str(r#"[{"context":"SurfaceChooser","use_builtin_defaults":false,"bindings":{"x":"ui.surface.next","z":"ui.surface.confirm","q":"ui.surface.cancel"}}]"#).unwrap();
    let config = load_config_from_path(directory.child("config.toml").path()).unwrap();
    let received = Rc::new(RefCell::new(Vec::new()));
    let (view, cx) = chooser_window_with_config(cx, received.clone(), &config);
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    for key in [
        "a", "t", "1", "j", "k", "ctrl-n", "ctrl-p", "down", "up", "enter", "escape", "tab",
    ] {
        cx.simulate_keystrokes(key);
        cx.run_until_parked();
        assert!(received.borrow().is_empty(), "disabled default {key}");
    }
    cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
    cx.simulate_keystrokes("x z");
    cx.run_until_parked();
    let choice = received
        .borrow_mut()
        .pop()
        .expect("remapped terminal choice");
    assert_eq!(choice.command, "surface.choose");
    assert_eq!(choice.arguments, ["41", "terminal"]);
    cx.simulate_keystrokes("q");
    cx.run_until_parked();
    assert_eq!(
        received
            .borrow_mut()
            .pop()
            .expect("remapped cancel")
            .command,
        "surface.cancel"
    );
}
