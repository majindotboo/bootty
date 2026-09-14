#![cfg(test)]

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
