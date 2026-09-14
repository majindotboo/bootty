#![cfg(test)]

use bootty_ui::gpui::{OverlayHost, OverlayView};
use gpui_kit::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Modifiers, MouseButton, Render, TestAppContext, Window, div, point, prelude::*, px,
};

struct TestOverlay {
    focus: FocusHandle,
}

impl TestOverlay {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

impl Focusable for TestOverlay {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<DismissEvent> for TestOverlay {}
impl OverlayView for TestOverlay {}

impl Render for TestOverlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .size(px(120.0))
            .child("overlay")
    }
}

struct HostProbe {
    background_focus: FocusHandle,
    host: Entity<OverlayHost>,
    overlay: Entity<TestOverlay>,
    replacement: Entity<TestOverlay>,
}

impl HostProbe {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            background_focus: cx.focus_handle(),
            host: cx.new(|_| OverlayHost::new()),
            overlay: cx.new(TestOverlay::new),
            replacement: cx.new(TestOverlay::new),
        }
    }
}

impl Render for HostProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.background_focus)
            .child(self.host.clone())
    }
}

fn host_probe(cx: &TestAppContext) -> gpui_kit::WindowHandle<HostProbe> {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        cx.open_window(gpui_kit::WindowOptions::default(), |_, cx| {
            cx.new(HostProbe::new)
        })
        .expect("open overlay host probe")
    })
}

#[gpui_kit::test]
fn dismissal_event_restores_the_exact_previous_focus(cx: &mut TestAppContext) {
    let window = host_probe(cx);
    window
        .update(cx, |probe, window, cx| {
            probe.background_focus.focus(window, cx);
            let host = probe.host.clone();
            let overlay = probe.overlay.clone();
            host.update(cx, |host, cx| host.present(overlay, window, cx));
        })
        .expect("present overlay");
    cx.run_until_parked();

    window
        .update(cx, |probe, window, cx| {
            assert!(probe.overlay.read(cx).focus.is_focused(window));
            assert!(probe.host.read(cx).active::<TestOverlay>().is_some());
            probe.overlay.update(cx, |_, cx| cx.emit(DismissEvent));
        })
        .expect("emit dismissal");
    cx.run_until_parked();

    window
        .update(cx, |probe, window, cx| {
            assert!(!probe.host.read(cx).has_active());
            assert!(probe.background_focus.is_focused(window));
        })
        .expect("verify restored focus");
}

#[gpui_kit::test]
fn explicit_dismiss_is_a_noop_without_an_active_overlay(cx: &mut TestAppContext) {
    let window = host_probe(cx);

    window
        .update(cx, |probe, window, cx| {
            assert!(!probe.host.update(cx, |host, cx| host.dismiss(window, cx)));
        })
        .expect("dismiss empty host");
}

#[gpui_kit::test]
fn escape_does_not_dismiss_an_anchored_overlay(cx: &mut TestAppContext) {
    let window = host_probe(cx);
    window
        .update(cx, |probe, window, cx| {
            probe.background_focus.focus(window, cx);
            probe.host.update(cx, |host, cx| {
                host.present(probe.overlay.clone(), window, cx);
            });
        })
        .expect("present overlay");
    cx.run_until_parked();

    cx.simulate_keystrokes(*window, "escape");
    cx.run_until_parked();

    window
        .update(cx, |probe, window, cx| {
            assert!(probe.host.read(cx).has_active());
            assert!(probe.overlay.read(cx).focus.is_focused(window));
        })
        .expect("verify anchored overlay remains active");
}

#[gpui_kit::test]
fn clicking_outside_does_not_dismiss_an_anchored_overlay(cx: &mut TestAppContext) {
    let (probe, cx) = cx.add_window_view(|_, cx| {
        gpui_kit::component::init(cx);
        HostProbe::new(cx)
    });
    cx.update(|window, cx| {
        probe.update(cx, |probe, cx| {
            probe.background_focus.focus(window, cx);
            probe.host.update(cx, |host, cx| {
                host.present(probe.overlay.clone(), window, cx);
            });
        });
    });
    cx.run_until_parked();

    cx.simulate_mouse_down(
        point(px(790.0), px(590.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();

    cx.update(|window, cx| {
        probe.update(cx, |probe, cx| {
            assert!(probe.host.read(cx).has_active());
            assert!(probe.background_focus.is_focused(window));
        });
    });
}

#[gpui_kit::test]
fn replacement_restores_focus_before_the_first_overlay(cx: &mut TestAppContext) {
    let window = host_probe(cx);
    window
        .update(cx, |probe, window, cx| {
            probe.background_focus.focus(window, cx);
            let host = probe.host.clone();
            host.update(cx, |host, cx| {
                host.present(probe.overlay.clone(), window, cx);
            });
        })
        .expect("present first overlay");
    cx.run_until_parked();

    window
        .update(cx, |probe, window, cx| {
            let host = probe.host.clone();
            host.update(cx, |host, cx| {
                host.present(probe.replacement.clone(), window, cx);
            });
        })
        .expect("replace overlay");
    cx.run_until_parked();

    window
        .update(cx, |probe, _window, cx| {
            probe.replacement.update(cx, |_, cx| cx.emit(DismissEvent));
        })
        .expect("dismiss replacement");
    cx.run_until_parked();

    window
        .update(cx, |probe, window, _cx| {
            assert!(probe.background_focus.is_focused(window));
        })
        .expect("verify background focus");
}
