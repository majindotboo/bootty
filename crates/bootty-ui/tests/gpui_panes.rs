#![cfg(test)]

use bootty_ui::gpui as bootty_gpui;
use std::{cell::RefCell, rc::Rc};

use bootty_ui::gpui::{
    GpuiPaneColors, GpuiPaneDividerSnapshot, GpuiPaneSnapshot, GpuiPaneWorkspace,
    GpuiPaneWorkspaceSnapshot, PaneRect, PaneSplitDirection,
};
use gpui_kit::{
    Context, IntoElement, Modifiers, MouseButton, Point, Render, TestAppContext, Window, black,
    blue, div, point, prelude::*, px,
};

struct PaneProbe {
    count: usize,
}

impl Render for PaneProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let panes = (0..self.count)
            .map(|index| GpuiPaneSnapshot {
                id: format!("pane-{index}"),
                rect: PaneRect::new(
                    f32::from(u16::try_from(index).expect("pane index fits u16")) * 100.0,
                    0.0,
                    100.0,
                    80.0,
                ),
                terminal: div().size_full().into_any_element(),
                focused: index == 0,
                progress: None,
            })
            .collect();
        GpuiPaneWorkspace::new(
            GpuiPaneWorkspaceSnapshot {
                arrangement_target: None,
                area: PaneRect::new(
                    0.0,
                    0.0,
                    f32::from(u16::try_from(self.count).expect("pane count fits u16")) * 100.0,
                    80.0,
                ),
                panes,
                dividers: Vec::new(),
                gap: 1.0,
                corner_radius: 0.0,
                focus_border_width: 2.0,
                inactive_dim: 0.0,
                window_dim: 0.0,
                animation_seconds: 0.0,
                colors: GpuiPaneColors {
                    background: black(),
                    divider: black(),
                    divider_hover: blue(),
                    focus_border: blue(),
                    progress_track: black(),
                    progress_normal: blue(),
                    progress_error: blue(),
                    progress_warning: blue(),
                    empty_text: black(),
                },
                empty_message: None,
            },
            |_, _, _| {},
        )
    }
}

#[gpui_kit::test]
fn a_single_pane_does_not_draw_a_redundant_focus_border(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| PaneProbe { count: 1 });

    assert!(cx.debug_bounds("terminal-pane-workspace").is_some());
    assert!(
        cx.debug_bounds("terminal-pane-focus-border-pane-0")
            .is_none()
    );
}

#[gpui_kit::test]
fn split_panes_keep_the_focused_pane_indicator(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| PaneProbe { count: 2 });

    assert!(
        cx.debug_bounds("terminal-pane-focus-border-pane-0")
            .is_some()
    );
    assert!(
        cx.debug_bounds("terminal-pane-focus-border-pane-1")
            .is_none()
    );
}

struct PaneLayerProbe;

impl Render for PaneLayerProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        GpuiPaneWorkspace::new(
            GpuiPaneWorkspaceSnapshot {
                arrangement_target: None,
                area: PaneRect::new(10.0, 20.0, 201.0, 80.0),
                panes: vec![
                    GpuiPaneSnapshot {
                        id: "active".to_owned(),
                        rect: PaneRect::new(10.0, 20.0, 100.0, 80.0),
                        terminal: div().size_full().into_any_element(),
                        focused: true,
                        progress: None,
                    },
                    GpuiPaneSnapshot {
                        id: "inactive".to_owned(),
                        rect: PaneRect::new(111.0, 20.0, 100.0, 80.0),
                        terminal: div().size_full().into_any_element(),
                        focused: false,
                        progress: None,
                    },
                ],
                dividers: vec![GpuiPaneDividerSnapshot {
                    path: vec![0],
                    direction: PaneSplitDirection::Right,
                    rect: PaneRect::new(110.0, 20.0, 1.0, 80.0),
                    area: PaneRect::new(10.0, 20.0, 201.0, 80.0),
                }],
                gap: 1.0,
                corner_radius: 0.0,
                focus_border_width: 2.0,
                inactive_dim: 0.25,
                window_dim: 0.0,
                animation_seconds: 0.0,
                colors: GpuiPaneColors {
                    background: black(),
                    divider: black(),
                    divider_hover: blue(),
                    focus_border: blue(),
                    progress_track: black(),
                    progress_normal: blue(),
                    progress_error: blue(),
                    progress_warning: blue(),
                    empty_text: black(),
                },
                empty_message: None,
            },
            |_, _, _| {},
        )
    }
}

#[gpui_kit::test]
fn pane_layers_keep_backgrounds_and_separator_at_their_owners(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| PaneLayerProbe);

    assert!(cx.debug_bounds("terminal-pane-surface-active").is_some());
    assert!(cx.debug_bounds("terminal-pane-surface-inactive").is_some());
    assert!(
        cx.debug_bounds("terminal-pane-inactive-dim-inactive")
            .is_some()
    );
    assert!(
        cx.debug_bounds("terminal-pane-inactive-dim-active")
            .is_none()
    );
    assert!(cx.debug_bounds("terminal-divider-visual-0").is_some());
}

struct PanePointerProbe {
    intents: Rc<RefCell<Vec<bootty_gpui::GpuiPaneIntent>>>,
}

impl Render for PanePointerProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let panes = (0..2)
            .map(|index| GpuiPaneSnapshot {
                id: format!("pane-{index}"),
                rect: PaneRect::new(
                    f32::from(u16::try_from(index).expect("pane index fits u16")) * 100.0,
                    0.0,
                    100.0,
                    80.0,
                ),
                terminal: div().size_full().into_any_element(),
                focused: index == 0,
                progress: None,
            })
            .collect();
        let intents = Rc::clone(&self.intents);
        GpuiPaneWorkspace::new(
            GpuiPaneWorkspaceSnapshot {
                arrangement_target: None,
                area: PaneRect::new(0.0, 0.0, 200.0, 80.0),
                panes,
                dividers: Vec::new(),
                gap: 0.0,
                corner_radius: 0.0,
                focus_border_width: 0.0,
                inactive_dim: 0.0,
                window_dim: 0.0,
                animation_seconds: 0.0,
                colors: GpuiPaneColors {
                    background: black(),
                    divider: black(),
                    divider_hover: blue(),
                    focus_border: blue(),
                    progress_track: black(),
                    progress_normal: blue(),
                    progress_error: blue(),
                    progress_warning: blue(),
                    empty_text: black(),
                },
                empty_message: None,
            },
            move |intent, _, _| intents.borrow_mut().push(intent),
        )
    }
}

#[gpui_kit::test]
fn non_left_pane_presses_focus_the_pointer_pane(cx: &mut TestAppContext) {
    let intents = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view({
        let intents = Rc::clone(&intents);
        move |_, _| PanePointerProbe { intents }
    });
    let position: Point<gpui_kit::Pixels> = point(px(150.0), px(40.0));

    cx.simulate_mouse_down(position, MouseButton::Middle, Modifiers::none());
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::none());

    assert_eq!(
        intents.borrow().as_slice(),
        [
            bootty_gpui::GpuiPaneIntent::Focus("pane-1".to_owned()),
            bootty_gpui::GpuiPaneIntent::Focus("pane-1".to_owned()),
        ]
    );
}
