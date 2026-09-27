#![cfg(test)]

use bootty_terminal::{terminal_input::ModifierSideState, terminal_input_model::MacosOptionAsAlt};
use bootty_ui::gpui as bootty_gpui;
use bootty_ui::gpui::{InputAccumulator, InputEvent, Key};
use gpui_kit::InputEvent as _;
use gpui_kit::{
    Context, FocusHandle, InputHandler, IntoElement, KeyDownEvent, Render, TestAppContext, Window,
    canvas, div, prelude::*,
};
use pretty_assertions::assert_eq;

struct InputProbe {
    input: InputAccumulator,
    focus: FocusHandle,
    received: Vec<InputEvent>,
    dropped_paths: Vec<std::path::PathBuf>,
    option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
}

impl Render for InputProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.input.drain_frame();
        self.received.extend(frame.events);
        self.dropped_paths.extend(frame.dropped_file_paths);
        let handler = self.input.ime_handler();
        let ime_focus = self.focus.clone();
        div()
            .id("input-probe")
            .size_full()
            .on_drop(
                cx.listener(|this, paths: &gpui_kit::ExternalPaths, window, cx| {
                    this.input.file_drop(paths, window.mouse_position());
                    cx.notify();
                }),
            )
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.input.key_down(event);
                if bootty_gpui::direct_input::terminal_owns_key_down(
                    event,
                    this.option_as_alt,
                    this.modifier_sides,
                ) {
                    cx.stop_propagation();
                }
                cx.notify();
            }))
            .child(canvas(
                |_, _, _| (),
                move |_, (), window, cx| {
                    window.handle_input(&ime_focus, handler.clone(), cx);
                },
            ))
    }
}

fn input_probe(cx: &mut TestAppContext) -> gpui_kit::WindowHandle<InputProbe> {
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), |_, cx| {
            cx.new(|cx| InputProbe {
                input: InputAccumulator::default(),
                focus: cx.focus_handle(),
                received: Vec::new(),
                dropped_paths: Vec::new(),
                option_as_alt: MacosOptionAsAlt::None,
                modifier_sides: ModifierSideState::default(),
            })
        })
        .expect("open input probe")
    });
    window
        .update(cx, |probe, window, cx| probe.focus.focus(window, cx))
        .expect("focus input probe");
    window
}

#[gpui_kit::test]
fn only_the_configured_option_side_consumes_platform_text(cx: &mut TestAppContext) {
    let window = input_probe(cx);
    for (option, left_is_meta, right_is_meta) in [
        (MacosOptionAsAlt::None, false, false),
        (MacosOptionAsAlt::Left, true, false),
        (MacosOptionAsAlt::Right, false, true),
        (MacosOptionAsAlt::Both, true, true),
    ] {
        for (right_alt, is_meta) in [(false, left_is_meta), (true, right_is_meta)] {
            window
                .update(cx, |probe, _, _| {
                    probe.option_as_alt = option;
                    probe.modifier_sides = ModifierSideState {
                        left_alt: !right_alt,
                        right_alt,
                        ..ModifierSideState::default()
                    };
                    probe.received.clear();
                })
                .unwrap();
            cx.dispatch_keystroke(
                *window,
                gpui_kit::Keystroke {
                    modifiers: gpui_kit::Modifiers {
                        alt: true,
                        ..Default::default()
                    },
                    key: "a".into(),
                    key_char: Some("å".into()),
                },
            );
            cx.simulate_input(*window, "a");
            window
                .update(cx, |probe, _, _| {
                    let text = probe
                        .received
                        .iter()
                        .filter_map(|event| match event {
                            InputEvent::ImeCommit(text) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    assert_eq!(text, if is_meta { "a" } else { "åa" });
                })
                .unwrap();
        }
    }
}

#[rstest::rstest]
fn option_dead_keys_are_consumed_before_composition(#[values("e", "u", "i", "n", "`")] key: &str) {
    let event = KeyDownEvent {
        keystroke: gpui_kit::Keystroke {
            modifiers: gpui_kit::Modifiers {
                alt: true,
                ..Default::default()
            },
            key: key.to_owned(),
            key_char: Some(String::new()),
        },
        is_held: false,
        prefer_character_input: false,
    };
    assert!(bootty_gpui::direct_input::terminal_owns_key_down(
        &event,
        MacosOptionAsAlt::Left,
        ModifierSideState {
            left_alt: true,
            ..Default::default()
        },
    ));
}

#[gpui_kit::test]
fn physical_key_and_text_commit_wake_the_frame(cx: &mut TestAppContext) {
    let window = input_probe(cx);

    cx.simulate_input(*window, "a");

    window
        .update(cx, |probe, _, _| {
            assert_eq!(
                probe.received,
                vec![
                    InputEvent::Key {
                        key: Key::Letter('a'),
                        pressed: true,
                        repeat: false,
                        modifiers: bootty_gpui::Modifiers::default(),
                    },
                    InputEvent::ImeCommit("a".to_owned()),
                ]
            );
        })
        .expect("read input probe");
}

#[gpui_kit::test]
fn shifted_printable_text_reaches_the_terminal_input_handler(cx: &mut TestAppContext) {
    let window = input_probe(cx);

    cx.simulate_input(*window, "100%");

    window
        .update(cx, |probe, _, _| {
            let committed = probe
                .received
                .iter()
                .filter_map(|event| match event {
                    InputEvent::ImeCommit(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            assert_eq!(committed, "100%");
        })
        .expect("read shifted printable input");
}

#[gpui_kit::test]
fn ime_composition_and_paste_commit_wake_the_frame(cx: &mut TestAppContext) {
    let window = input_probe(cx);

    window
        .update(cx, |probe, window, cx| {
            let mut handler = probe.input.ime_handler();
            handler.replace_and_mark_text_in_range(None, "文", Some(1..1), window, cx);
            handler.replace_text_in_range(None, "pasted 文", window, cx);
        })
        .expect("send IME input");
    cx.run_until_parked();

    window
        .update(cx, |probe, _, _| {
            assert_eq!(
                probe.received,
                vec![
                    InputEvent::ImePreedit {
                        text: "文".to_owned(),
                        selected_range_utf16: Some(1..1),
                    },
                    InputEvent::ImeCommit("pasted 文".to_owned()),
                ]
            );
        })
        .expect("read input probe");
}

#[gpui_kit::test]
fn external_file_drop_delivers_paths_once_and_ignores_cancelled_drags(cx: &mut TestAppContext) {
    let handle = input_probe(cx);
    let position = gpui_kit::point(gpui_kit::px(20.), gpui_kit::px(20.));
    let paths = vec!["/tmp/a file.txt".into(), "/tmp/it's here.png".into()];
    for submit in [false, true] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.dispatch_event(
                gpui_kit::FileDropEvent::Entered {
                    position,
                    paths: gpui_kit::ExternalPaths(paths.clone().into()),
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                if submit {
                    gpui_kit::FileDropEvent::Submit { position }
                } else {
                    gpui_kit::FileDropEvent::Exited
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(gpui_kit::FileDropEvent::Ended.to_platform_input(), cx);
        })
        .unwrap();
        handle
            .update(cx, |probe, _, _| {
                assert_eq!(
                    probe.dropped_paths,
                    if submit { paths.clone() } else { vec![] }
                );
                assert_eq!(
                    probe.input.drain_frame().dropped_file_paths,
                    Vec::<std::path::PathBuf>::new()
                );
            })
            .unwrap();
    }
}

#[rstest::rstest]
#[case::window_owner(true)]
#[case::terminal_view(false)]
fn focus_loss_clears_held_input_and_preserves_queued_text(#[case] owns_window: bool) {
    let mut input = InputAccumulator::default();
    input.mouse_down(&gpui_kit::MouseDownEvent {
        button: gpui_kit::MouseButton::Left,
        modifiers: gpui_kit::Modifiers {
            control: true,
            ..gpui_kit::Modifiers::default()
        },
        ..gpui_kit::MouseDownEvent::default()
    });
    let pressed = input.drain_frame();
    assert!(pressed.modifiers.control);
    assert_eq!(
        pressed.pressed_mouse_button,
        Some(bootty_gpui::PointerButton::Left)
    );
    input.ime_commit("queued text");

    if owns_window {
        input.window_focused(false);
    } else {
        input.observe_window_focus(false);
    }

    let frame = input.drain_frame();
    assert!(!frame.window_focused);
    assert_eq!(frame.modifiers, bootty_gpui::Modifiers::default());
    assert_eq!(frame.pressed_mouse_button, None);
    let mut expected = vec![InputEvent::ImeCommit("queued text".to_owned())];
    if owns_window {
        expected.extend([
            InputEvent::WindowFocused(false),
            InputEvent::ModifiersChanged(bootty_gpui::Modifiers::default()),
        ]);
    }
    assert_eq!(frame.events, expected);
}
