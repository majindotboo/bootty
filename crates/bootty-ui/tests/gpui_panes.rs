#![cfg(test)]

use bootty_control::{CommandInvocation, CommandTarget, ResourceKind};
use bootty_ui::{
    gpui::{DraggedPane, GpuiPane, GpuiPaneColors, GpuiPaneSnapshot},
    workspace_composition::pane_drop_command,
};
use gpui_kit::component::{
    ActiveTheme as _,
    dock::{
        BasePanel, DockArea, DockEvent, DockLayout, DockPlacement, PaneRef, Panel, PanelEvent,
        PanelId, PanelInfo, PanelState,
    },
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Modifiers,
    MouseButton, ParentElement, Render, Subscription, TestAppContext, Window, div, point,
    prelude::*, px,
};
use pretty_assertions::assert_eq;
use std::{cell::RefCell, rc::Rc};

fn target() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Session,
        handle: "session".into(),
        generation: 1,
    }
}
struct Pane {
    id: String,
    focus: FocusHandle,
}
impl EventEmitter<PanelEvent> for Pane {}
impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for Pane {
    fn panel_name(&self) -> &'static str {
        "bootty.terminal-pane"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.panel_name().into(),
            children: Vec::new(),
            info: PanelInfo::panel(serde_json::json!({"pane":self.id})),
        }
    }
}
impl Panel for Pane {}
impl Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        GpuiPane::new(
            GpuiPaneSnapshot {
                id: self.id.clone(),
                target: Some(target()),
                terminal: div().size_full(),
                focused: true,
                progress: None,
                width: 300.,
                corner_radius: 0.,
                focus_border_width: 0.,
                inactive_dim: 0.,
                window_dim: 0.,
                animation_seconds: 0.,
                colors: GpuiPaneColors {
                    background: theme.background,
                    focus_border: theme.primary,
                    progress_track: theme.border,
                    progress_normal: theme.primary,
                    progress_error: theme.danger,
                    progress_warning: theme.warning,
                },
            },
            |_, _, _| {},
        )
    }
}
struct DockProbe {
    area: Entity<DockArea>,
    panes: [Entity<Pane>; 2],
    commands: Rc<RefCell<Vec<CommandInvocation>>>,
    changes: usize,
    _subscription: Subscription,
}
impl DockProbe {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let area = cx.new(|cx| DockArea::new("pane-test", None, window, cx));
        let panes = ["a", "b"].map(|id| {
            cx.new(|cx| Pane {
                id: id.into(),
                focus: cx.focus_handle(),
            })
        });
        area.update(cx, |area, cx| {
            area.set_center(
                DockLayout::h_split()
                    .child(DockLayout::tabs().panel(panes[0].clone()), Some(px(300.)))
                    .child(DockLayout::tabs().panel(panes[1].clone()), Some(px(300.))),
                window,
                cx,
            );
        });
        let commands = Rc::new(RefCell::new(Vec::new()));
        let subscription = cx.subscribe(&area, |this, area, event, cx| match event {
            DockEvent::LayoutChanged => this.changes = this.changes.saturating_add(1),
            DockEvent::DragDrop { item, target } => {
                let drag = item.value().downcast_ref::<DraggedPane>().unwrap();
                let area = area.read(cx);
                let node = area
                    .layout(DockPlacement::Center)
                    .unwrap()
                    .find_node(target.node())
                    .unwrap();
                let PaneRef::Tabs { panels, .. } = node.kind() else {
                    panic!("drop on split")
                };
                let pane = this
                    .panes
                    .iter()
                    .find(|pane| panels.contains(&PanelId::from(pane.entity_id())))
                    .unwrap()
                    .read(cx);
                if let Some(command) = pane_drop_command(drag, &pane.id, target.placement()) {
                    this.commands.borrow_mut().push(command);
                }
            }
        });
        Self {
            area,
            panes,
            commands,
            changes: 0,
            _subscription: subscription,
        }
    }
}
impl Render for DockProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(600.)).h(px(400.)).child(self.area.clone())
    }
}
#[gpui_kit::test]
fn host_drag_reports_edge_move_and_center_swap_without_mutating_dock(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (probe, cx) = cx.add_window_view(DockProbe::new);
    for (fraction, command) in [(0.1, "pane.move"), (0.5, "pane.swap")] {
        let source = cx
            .debug_bounds("pane-drag-handle-a")
            .expect("pane handle")
            .center();
        let target = cx.debug_bounds("terminal-pane-b").expect("pane content");
        let destination = point(
            target.left() + target.size.width * fraction,
            target.top() + target.size.height * 0.5,
        );
        cx.simulate_mouse_down(source, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            source + point(px(-12.), px(12.)),
            Some(MouseButton::Left),
            Modifiers::none(),
        );
        cx.simulate_mouse_move(destination, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(destination, MouseButton::Left, Modifiers::none());
        probe.read_with(cx, |probe, _| {
            let commands = probe.commands.borrow();
            assert_eq!(
                commands.last().expect("Dock reports host drop").command,
                command
            );
            assert_eq!(probe.changes, 0, "host drops leave Dock topology untouched");
        });
    }
}
#[gpui_kit::test]
fn dock_resize_reports_one_layout_change(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (probe, cx) = cx.add_window_view(DockProbe::new);
    cx.update(|window, cx| {
        let area = probe.read(cx).area.clone();
        let node = area
            .read(cx)
            .layout(DockPlacement::Center)
            .unwrap()
            .root()
            .id();
        area.update(cx, |area, cx| {
            area.set_split_sizes(node, vec![px(200.), px(400.)], window, cx);
        });
    });
    cx.run_until_parked();
    probe.read_with(cx, |probe, cx| {
        assert_eq!(probe.changes, 1);
        let layout =
            bootty_ui::workspace_composition::mux_pane_layout(&probe.area.read(cx).dump(cx).center)
                .unwrap();
        let bootty_mux::snapshot::MuxPaneLayout::Split { weights, .. } = layout else {
            panic!("split")
        };
        assert!((weights[0] - 1. / 3.).abs() < 0.000_01);
    });
}
