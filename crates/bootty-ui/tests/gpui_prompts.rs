#![cfg(test)]

use bootty_ui::gpui::{UiPalette, init_theme, prompt};
use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::{
    Context, FocusHandle, IntoElement, Modifiers, Render, TestAppContext, Window, div, prelude::*,
};
use pretty_assertions::assert_eq;

struct PromptProbe {
    focus: FocusHandle,
}

impl Render for PromptProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layer = Root::render_dialog_layer(window, cx);
        div().size_full().track_focus(&self.focus).children(layer)
    }
}

#[gpui_kit::test]
fn confirmations_preserve_answers_and_cancel_without_losing_focus(cx: &mut TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let focus = cx.update(|cx| cx.focus_handle());
    let (_, cx) = cx.add_window_view(|window, cx| {
        let probe = cx.new(|_| PromptProbe {
            focus: focus.clone(),
        });
        Root::new(probe, window, cx).bordered(false)
    });

    for (selector, expected) in [
        ("prompt-answer-Save", 0),
        ("prompt-answer-Discard", 1),
        ("prompt-answer-Cancel", 2),
    ] {
        let mut answer = cx.update(|window, cx| {
            focus.focus(window, cx);
            prompt(
                "Save changes to config.toml?",
                Some("/tmp/config.toml"),
                &["Save".into(), "Discard".into(), "Cancel".into()],
                window,
                cx,
            )
        });
        let bounds = cx.debug_bounds(selector).expect("themed prompt action");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(answer.try_recv(), Ok(Some(expected)));
        cx.update(|window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert!(focus.is_focused(window));
        });
    }

    for (keys, expected) in [("enter", Some(0)), ("tab enter", Some(1)), ("escape", None)] {
        let mut answer = cx.update(|window, cx| {
            prompt(
                "Discard changes?",
                None,
                &["Discard".into(), "Cancel".into()],
                window,
                cx,
            )
        });
        cx.refresh().expect("render confirmation keyboard context");
        cx.simulate_keystrokes(keys);
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        match expected {
            Some(expected) => assert_eq!(answer.try_recv(), Ok(Some(expected)), "{keys}"),
            None => assert!(answer.try_recv().is_err(), "{keys}"),
        }
        cx.update(|window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert!(focus.is_focused(window));
        });
    }
}
