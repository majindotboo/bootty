#![cfg(test)]

use std::{cell::Cell, rc::Rc};

use bootty_ui::gpui::{UiPalette, init_theme};
use gpui_kit::component::Root;
use gpui_kit::{
    AppContext as _, Context, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext,
    WeakEntity, Window, div, prelude::*,
};
use pretty_assertions::assert_eq;

struct RootChild {
    clicks: Rc<Cell<usize>>,
}

impl Render for RootChild {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root-child-button")
            .debug_selector(|| "root-child-button".to_owned())
            .size_full()
            .on_click(cx.listener(|this, _, _, cx| {
                this.clicks.set(
                    this.clicks
                        .get()
                        .checked_add(1)
                        .expect("click count fits usize"),
                );
                cx.notify();
            }))
    }
}

#[gpui_kit::test]
fn root_forwards_pointer_input_and_releases_its_weak_child(cx: &TestAppContext) {
    cx.update(|cx| init_theme(UiPalette::default(), cx));
    let clicks = Rc::new(Cell::new(0));
    let child_slot = Rc::new(Cell::new(None::<WeakEntity<RootChild>>));
    let opened_child = Rc::clone(&child_slot);
    let child_clicks = Rc::clone(&clicks);
    let window = cx.update(|cx| {
        cx.open_window(gpui_kit::WindowOptions::default(), move |window, cx| {
            let child = cx.new(|_| RootChild {
                clicks: child_clicks,
            });
            opened_child.set(Some(child.downgrade()));
            cx.new(|cx| Root::new(child, window, cx).bordered(false))
        })
        .expect("open rooted window")
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let root = window.root(&mut cx).expect("window root has Root type");
    cx.update(|_, cx| {
        assert!(root.read(cx).view().clone().downcast::<RootChild>().is_ok());
    });
    drop(root);

    let bounds = cx
        .debug_bounds("root-child-button")
        .expect("root child is laid out");
    cx.simulate_click(bounds.center(), Modifiers::none());
    assert_eq!(clicks.get(), 1);

    let child = child_slot.take().expect("capture weak rooted child");
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
    assert!(child.upgrade().is_none());
}
