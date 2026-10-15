#![allow(clippy::unwrap_used, clippy::expect_used)]
use bootty_agents::{AgentKind, NativeModelOption};
use bootty_ui::gpui::{ModelPickerEvent, ModelPickerView, UiPalette, init_theme};
use bootty_ui::gpui_actions::{
    InvokeCommand, WORKSPACE_KEY_CONTEXT, replace_workspace_key_bindings, workspace_key_bindings,
};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, Modifiers,
    ParentElement as _, Render, TestAppContext, Window, div, px, size,
};

struct WorkspacePicker {
    picker: Entity<ModelPickerView>,
    commands: Rc<RefCell<Vec<String>>>,
}

impl Render for WorkspacePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(WORKSPACE_KEY_CONTEXT)
            .on_action(cx.listener(|this, command: &InvokeCommand, _, cx| {
                this.commands
                    .borrow_mut()
                    .push(command.invocation().command.clone());
                cx.stop_propagation();
            }))
            .child(self.picker.clone())
    }
}
use pretty_assertions::assert_eq;
use std::{cell::RefCell, rc::Rc};

fn model(id: &str, legacy: bool, favorite: bool) -> NativeModelOption {
    NativeModelOption {
        id: id.into(),
        display_name: if id.ends_with("current") {
            "Current model".into()
        } else {
            id.rsplit('/').next().unwrap().into()
        },
        reasoning_efforts: vec!["medium".into()],
        default_reasoning_effort: Some("medium".into()),
        is_default: id.ends_with("current"),
        is_legacy: legacy,
        is_favorite: favorite,
        supports_fast_mode: true,
    }
}

#[gpui_kit::test]
fn catalog_picker_filters_legacy_and_uses_numbered_selection(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
        let input = bootty_config::config::InputConfig {
            keybind: vec![format!("{}=select_tab:1", secondary_key("1"))],
            ..Default::default()
        };
        replace_workspace_key_bindings(workspace_key_bindings(&input).unwrap(), cx).unwrap();
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let collected = events.clone();
    let commands = Rc::new(RefCell::new(Vec::new()));
    let received = commands.clone();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            ModelPickerView::new(
                AgentKind::Pi,
                vec![
                    model("openai/current", false, true),
                    model("openai/legacy", true, false),
                    model("anthropic/other", false, true),
                ],
                Some("openai/current"),
                window,
                cx,
            )
        });
        cx.subscribe(&view, move |_, _, event: &ModelPickerEvent, _| {
            collected.borrow_mut().push(event.clone());
        })
        .detach();
        let host = cx.new(|_| WorkspacePicker {
            picker: view,
            commands: received,
        });
        gpui_kit::component::Root::new(host, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(600.)));
    draw(visual);
    click(visual, "model-picker-trigger");
    assert!(visual.debug_bounds("model-option:openai/current").is_some());
    assert!(visual.debug_bounds("model-fast-mode").is_none());
    assert!(visual.debug_bounds("model-option:openai/legacy").is_none());
    click(visual, "model-favorite:openai/current");
    assert_eq!(
        *events.borrow(),
        vec![ModelPickerEvent::Favorite("openai/current".into())]
    );
    assert!(visual.debug_bounds("model-picker-content").is_some());
    events.borrow_mut().clear();
    click(visual, "model-favorites");
    assert!(visual.debug_bounds("model-option:openai/current").is_some());
    assert!(
        visual
            .debug_bounds("model-option:anthropic/other")
            .is_some()
    );
    assert!(visual.debug_bounds("model-option:openai/legacy").is_none());
    click(visual, "model-provider:anthropic");
    assert!(visual.debug_bounds("model-option:openai/current").is_none());
    assert!(
        visual
            .debug_bounds("model-option:anthropic/other")
            .is_some()
    );
    click(visual, "model-provider:openai");
    visual.simulate_input("Current");
    visual.simulate_keystrokes("space");
    visual.simulate_input("model");
    draw(visual);
    assert!(visual.debug_bounds("model-picker-content").is_some());
    assert!(visual.debug_bounds("model-option:openai/current").is_some());
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-a backspace"
    } else {
        "ctrl-a backspace"
    });
    draw(visual);
    click(visual, "model-legacy-disclosure");
    assert!(visual.debug_bounds("model-option:openai/legacy").is_some());
    // A search reaches legacy models without needing to expand them.
    click(visual, "model-legacy-disclosure");
    visual.simulate_input("legacy");
    visual.run_until_parked();
    draw(visual);
    assert!(visual.debug_bounds("model-option:openai/legacy").is_some());
    assert!(visual.debug_bounds("model-option:openai/current").is_none());
    visual.simulate_keystrokes(&secondary_key("1"));
    draw(visual);
    assert_eq!(
        *events.borrow(),
        vec![ModelPickerEvent::Select("openai/legacy".into())]
    );
    assert!(visual.debug_bounds("model-picker-content").is_none());
    assert!(commands.borrow().is_empty());
}

fn draw(visual: &mut gpui_kit::VisualTestContext) {
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
}

fn secondary_key(key: &str) -> String {
    format!(
        "{}-{key}",
        if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        }
    )
}

fn click(visual: &mut gpui_kit::VisualTestContext, selector: &'static str) {
    let bounds = visual.debug_bounds(selector).expect(selector);
    visual.simulate_click(bounds.center(), Modifiers::none());
    draw(visual);
}

#[gpui_kit::test]
fn fast_mode_is_a_shared_picker_control_and_pi_has_no_speed_control(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let collected = events.clone();
    let retained = Rc::new(RefCell::new(None));
    let captured = retained.clone();
    let (_, visual) = cx.add_window_view(|window, cx| {
        let picker = cx.new(|cx| {
            ModelPickerView::new_with_providers(
                AgentKind::Codex,
                vec![model("current", false, false)],
                Some("current"),
                vec!["Codex".into(), "Pi".into()],
                window,
                cx,
            )
        });
        *captured.borrow_mut() = Some(picker.clone());
        cx.subscribe(&picker, move |_, _, event: &ModelPickerEvent, _| {
            collected.borrow_mut().push(event.clone());
        })
        .detach();
        let host = cx.new(|_| WorkspacePicker {
            picker,
            commands: Rc::default(),
        });
        gpui_kit::component::Root::new(host, window, cx)
    });
    visual.simulate_resize(size(px(800.), px(600.)));
    draw(visual);
    click(visual, "model-picker-trigger");
    click(visual, "model-fast-mode");
    assert_eq!(*events.borrow(), vec![ModelPickerEvent::FastMode(true)]);
    retained
        .borrow()
        .as_ref()
        .unwrap()
        .update(visual, |picker, cx| picker.set_fast_mode(true, cx));
    draw(visual);
    click(visual, "model-fast-mode");
    assert_eq!(
        *events.borrow(),
        vec![
            ModelPickerEvent::FastMode(true),
            ModelPickerEvent::FastMode(false)
        ]
    );
}

#[gpui_kit::test]
fn unavailable_explicit_models_are_kept_and_speed_requires_a_capability(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        init_theme(UiPalette::default(), cx);
    });
    for current in [None, Some("removed-model")] {
        let retained = Rc::new(RefCell::new(None));
        let captured = retained.clone();
        let (_, visual) = cx.add_window_view(|window, cx| {
            let picker = cx.new(|cx| {
                let mut option = model("current", false, false);
                option.supports_fast_mode = false;
                ModelPickerView::new(AgentKind::Codex, vec![option], current, window, cx)
            });
            *captured.borrow_mut() = Some(picker.clone());
            gpui_kit::component::Root::new(picker, window, cx)
        });
        visual.simulate_resize(size(px(800.), px(600.)));
        draw(visual);
        let selector = if current.is_some() {
            "model-label:removed-model"
        } else {
            "model-label:Current model"
        };
        assert!(visual.debug_bounds(selector).is_some());
        click(visual, "model-picker-trigger");
        assert!(visual.debug_bounds("model-fast-mode").is_none());
        visual.simulate_keystrokes("escape");
        let mut option = model("current", false, false);
        option.supports_fast_mode = false;
        retained
            .borrow()
            .as_ref()
            .unwrap()
            .update_in(visual, |picker, window, cx| {
                picker.set_models(&[option], Some("removed-after-refresh"), true, window, cx);
            });
        draw(visual);
        assert!(
            visual
                .debug_bounds("model-label:removed-after-refresh")
                .is_some()
        );
    }
}
