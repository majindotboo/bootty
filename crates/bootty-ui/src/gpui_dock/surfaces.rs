//! Fixed center composition uses real panel entities and exact service/backend targets.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use bootty_control::CommandTarget;
use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockLayout, DockPlacement, InsertTarget, PaneNode, PaneRef, PanelId,
    PanelInfo, PanelState, panel_handle,
};
use gpui_kit::{App, AppContext as _, Context, Entity, Focusable, WeakEntity, Window};

use super::{WorkspaceDock, register_factory};
use crate::gpui_surface_chooser::{SurfaceCommand, SurfacePanel};
use crate::surface_creation::{PendingNewSurface, SurfaceParent, SurfacePlacement};

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TerminalSurfaceOrigin {
    pub binding_id: String,
    pub task_identity: String,
    pub window_key: String,
}

#[derive(Default)]
pub(super) struct CenterSurfaces {
    chooser: Option<(PendingNewSurface, Entity<SurfacePanel>)>,
    chooser_previous_center: Option<String>,
    pub(super) terminals: Rc<RefCell<HashMap<TerminalSurfaceOrigin, Entity<SurfacePanel>>>>,
}

impl CenterSurfaces {
    pub(super) fn new(
        area: &Entity<DockArea>,
        workspace: WeakEntity<crate::gpui_workspace::GpuiWorkspace>,
        cx: &mut App,
    ) -> Self {
        let terminals = Rc::new(RefCell::new(HashMap::<
            TerminalSurfaceOrigin,
            Entity<SurfacePanel>,
        >::new()));
        let held_terminals = terminals.clone();
        let terminal_owner = workspace;
        register_factory(
            area,
            "bootty.terminal-window",
            Rc::new(move |info, window, cx| {
                let PanelInfo::Panel(info) = info else {
                    return panel_handle(cx.new(SurfacePanel::unavailable));
                };
                let Some(origin) =
                    serde_json::from_value::<TerminalSurfaceOrigin>(info.clone()).ok()
                else {
                    return panel_handle(cx.new(SurfacePanel::unavailable));
                };
                let key = origin.clone();
                if let Some(panel) = held_terminals.borrow().get(&key) {
                    return panel_handle(panel.clone());
                }
                let panel = cx.new(|cx| SurfacePanel::terminal(origin.clone(), cx));
                held_terminals.borrow_mut().insert(key, panel.clone());
                let owner = terminal_owner.clone();
                let window_handle = window.window_handle();
                cx.defer(move |cx| {
                    _ = cx.update_window(window_handle, |_, window, cx| {
                        _ = owner.update(cx, |owner, cx| {
                            owner.restore_terminal_surface(&origin, window, cx);
                        });
                    });
                });
                panel_handle(panel)
            }),
            cx,
        );
        Self {
            chooser: None,
            chooser_previous_center: None,
            terminals,
        }
    }

    pub(super) fn contains(&self, id: PanelId) -> bool {
        self.terminals
            .borrow()
            .values()
            .any(|panel| PanelId::from(panel.entity_id()) == id)
            || self
                .chooser
                .as_ref()
                .is_some_and(|(_, panel)| PanelId::from(panel.entity_id()) == id)
    }
}

fn subscribe_commands(
    panel: &Entity<SurfacePanel>,
    dock: &WeakEntity<WorkspaceDock>,
    window: &Window,
    cx: &mut App,
) {
    let dock = dock.clone();
    let window_handle = window.window_handle();
    let subscription = cx.subscribe(panel, move |_, event: &SurfaceCommand, cx| {
        _ = cx.update_window(window_handle, |_, window, cx| {
            _ = dock.update(cx, |dock, cx| {
                dock.submit_command(event.0.clone(), window, cx);
            });
        });
    });
    panel.update(cx, |panel, _| {
        panel.retain_subscription(subscription);
    });
}

// Splice only the current chooser slot. Rebuilding with live handles keeps concurrent
// changes and every surviving panel; loading an earlier snapshot would undo them.
fn splice_chooser(
    area: &DockArea,
    node: &PaneNode,
    measured: &PanelState,
    chooser: PanelId,
    parent: Option<PanelId>,
    replacement: Option<&std::sync::Arc<dyn BasePanelView>>,
    cx: &App,
) -> Option<DockLayout> {
    match node.kind() {
        PaneRef::Tabs { panels, active_ix } => {
            let mut layout = DockLayout::tabs();
            let mut count = 0usize;
            let mut selected = 0usize;
            for (ix, id) in panels.iter().enumerate() {
                let panel = if *id == chooser {
                    replacement
                } else {
                    area.panel(*id)
                };
                if let Some(panel) = panel {
                    if ix <= active_ix {
                        selected = count;
                    }
                    layout = layout.panel_view(panel.clone(), cx);
                    count = count.saturating_add(1);
                }
            }
            (count > 0).then(|| layout.active_index(selected))
        }
        PaneRef::Split {
            axis,
            children,
            sizes,
        } => {
            let mut layout = if axis == gpui_kit::Axis::Horizontal {
                DockLayout::h_split()
            } else {
                DockLayout::v_split()
            };
            let measured_sizes = match &measured.info {
                PanelInfo::Stack { sizes, .. } => Some(sizes),
                _ => None,
            };
            let slot_size = |ix: usize| {
                measured_sizes
                    .and_then(|sizes| sizes.get(ix))
                    .copied()
                    .filter(|size| size.as_f32() > 0.0)
                    .or_else(|| sizes.get(ix).copied().flatten())
            };
            let slots = children
                .iter()
                .enumerate()
                .map(|(ix, child)| {
                    let state = measured.children.get(ix).unwrap_or(measured);
                    (
                        child,
                        splice_chooser(area, child, state, chooser, parent, replacement, cx),
                    )
                })
                .collect::<Vec<_>>();
            let released = replacement
                .is_none()
                .then(|| {
                    slots
                        .iter()
                        .enumerate()
                        .find(|(_, (child, layout))| {
                            layout.is_none() && contains_panel(child, chooser)
                        })
                        .and_then(|(ix, _)| slot_size(ix))
                })
                .flatten();
            let mut count = 0usize;
            for (ix, (child, held)) in slots.into_iter().enumerate() {
                if let Some(held) = held {
                    let size = if parent.is_some_and(|parent| contains_panel(child, parent)) {
                        match (slot_size(ix), released) {
                            (Some(size), Some(released)) => Some(gpui_kit::px(
                                [size.as_f32(), released.as_f32()].into_iter().sum(),
                            )),
                            (size, _) => size,
                        }
                    } else {
                        slot_size(ix)
                    };
                    layout = layout.child(held, size);
                    count = count.saturating_add(1);
                }
            }
            (count > 0).then_some(layout)
        }
    }
}

fn contains_panel(node: &PaneNode, panel: PanelId) -> bool {
    match node.kind() {
        PaneRef::Tabs { panels, .. } => panels.contains(&panel),
        PaneRef::Split { children, .. } => children.iter().any(|node| contains_panel(node, panel)),
    }
}

impl WorkspaceDock {
    pub(super) const fn has_surface_chooser(&self) -> bool {
        self.surfaces.chooser.is_some()
    }
    pub(crate) fn surface_chooser_focused(&self, window: &Window, cx: &App) -> bool {
        self.surfaces
            .chooser
            .as_ref()
            .is_some_and(|(_, panel)| panel.read(cx).chooser_focused(window, cx))
    }

    pub(crate) fn navigate_surface_chooser(
        &self,
        id: u64,
        action: crate::commands::SurfaceChooserAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((request, panel)) = &self.surfaces.chooser
            && request.id == id
        {
            panel.update(cx, |panel, cx| panel.navigate(action, window, cx));
        }
    }

    pub(crate) fn outer_chooser_id(&self) -> Option<u64> {
        self.surfaces
            .chooser
            .as_ref()
            .filter(|(request, _)| request.placement == SurfacePlacement::Tab)
            .map(|(request, _)| request.id)
    }
    pub(crate) fn visible_terminal_surface_panels(
        &self,
        cx: &App,
    ) -> Vec<Entity<crate::gpui_terminal_panel::TerminalPanel>> {
        let tree = self.area.read(cx).layout(DockPlacement::Center);
        let mut panels = Vec::new();
        if tree.is_some_and(|tree| tree.contains_panel(PanelId::from(self.terminal.entity_id()))) {
            panels.push(self.terminal.clone());
        }
        panels.extend(
            self.surfaces
                .terminals
                .borrow()
                .values()
                .filter_map(|surface| {
                    tree.filter(|tree| tree.contains_panel(PanelId::from(surface.entity_id())))?;
                    surface.read(cx).terminal_view()
                }),
        );
        panels
    }

    pub(crate) fn terminal_surface_panels(
        &self,
        cx: &App,
    ) -> Vec<Entity<crate::gpui_terminal_panel::TerminalPanel>> {
        std::iter::once(self.terminal.clone())
            .chain(
                self.surfaces
                    .terminals
                    .borrow()
                    .values()
                    .filter_map(|panel| panel.read(cx).terminal_view()),
            )
            .collect()
    }

    pub(crate) fn terminal_surface_origins(&self, cx: &App) -> Vec<TerminalSurfaceOrigin> {
        self.surfaces
            .terminals
            .borrow()
            .values()
            .filter_map(|panel| panel.read(cx).terminal_origin().cloned())
            .collect()
    }

    pub(crate) fn publish_window_surface(
        &self,
        origin: &TerminalSurfaceOrigin,
        target: CommandTarget,
        id: bootty_mux::workspace::ScopedWindowId,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = origin.clone();
        let Some(panel) = self.surfaces.terminals.borrow().get(&key).cloned() else {
            return;
        };
        let view = panel.read(cx).terminal_view().unwrap_or_else(|| {
            cx.new(|cx| {
                crate::gpui_terminal_panel::TerminalPanel::new(
                    target.clone(),
                    id.clone(),
                    title.clone(),
                    self.owner.clone(),
                    window,
                    cx,
                )
                .with_origin(origin.clone())
            })
        });
        view.update(cx, |view, cx| {
            view.set_binding_target(target);
            view.set_window_id(id, cx);
            view.set_title(title, cx);
        });
        panel.update(cx, |panel, cx| panel.publish_terminal(view, window, cx));
    }

    pub(crate) fn attach_window_surface(
        &mut self,
        origin: &TerminalSurfaceOrigin,
        target: CommandTarget,
        presentation: super::TerminalWindowPresentation,
        request: &PendingNewSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same_window = self
            .terminal_panel_for_parent(&request.parent, cx)
            .is_some_and(|parent| parent.read(cx).window_id == presentation.id);
        if same_window {
            self.close_surface_chooser(Some(request.id), window, cx);
            self.select_terminal_panel(&presentation.id, window, cx);
            return;
        }
        let key = origin.clone();
        let held = self.surfaces.terminals.borrow().get(&key).cloned();
        let panel = held.unwrap_or_else(|| {
            let panel = cx.new(|cx| SurfacePanel::terminal(origin.clone(), cx));
            self.surfaces
                .terminals
                .borrow_mut()
                .insert(key, panel.clone());
            panel
        });
        self.publish_window_surface(
            origin,
            target,
            presentation.id,
            presentation.title,
            window,
            cx,
        );
        let replaced = self.finish_surface_chooser(
            Some(request.id),
            Some(&panel_handle(panel.clone())),
            window,
            cx,
        );
        if request.placement == SurfacePlacement::Tab {
            let key = crate::workspace_composition::surface_center_key(
                &origin.binding_id,
                &origin.task_identity,
                "window",
                &origin.window_key,
            );
            if replaced {
                self.rename_center(key, cx);
            } else {
                self.select_center(key, window, cx);
            }
        }
        if !replaced {
            self.place_center_panel(
                panel_handle(panel.clone()),
                &request.parent,
                request.placement,
                window,
                cx,
            );
        }
        panel.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn terminal_panel_for_parent(
        &self,
        parent: &SurfaceParent,
        cx: &App,
    ) -> Option<Entity<crate::gpui_terminal_panel::TerminalPanel>> {
        let target = match parent {
            SurfaceParent::Terminal(target) => target,
            SurfaceParent::Conversation(_) | SurfaceParent::Binding(_) => return None,
        };
        let path: Vec<String> = serde_json::from_str(&target.handle).ok()?;
        let [_, session, window, _] = path.as_slice() else {
            return None;
        };
        self.terminal_surface_panels(cx).into_iter().find(|panel| {
            let panel = panel.read(cx);
            panel.window_id.session_id() == session && panel.window_id.window_id() == window
        })
    }

    pub(crate) fn terminal_surface_focus(
        &self,
        selected: &bootty_mux::workspace::ScopedWindowId,
        cx: &App,
    ) -> Option<gpui_kit::FocusHandle> {
        self.terminal_surface_panels(cx)
            .into_iter()
            .find(|panel| panel.read(cx).window_id == *selected)
            .map(|panel| panel.read(cx).focus_handle(cx))
    }

    pub(crate) fn select_terminal_panel(
        &self,
        selected: &bootty_mux::workspace::ScopedWindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self
            .terminal_surface_panels(cx)
            .into_iter()
            .find(|panel| selected == &panel.read(cx).window_id)
            .unwrap_or_else(|| self.terminal.clone());
        let id = self
            .surfaces
            .terminals
            .borrow()
            .values()
            .find(|surface| surface.read(cx).terminal_view().as_ref() == Some(&panel))
            .map_or_else(
                || PanelId::from(panel.entity_id()),
                |surface| PanelId::from(surface.entity_id()),
            );
        self.area
            .update(cx, |area, cx| area.select_panel(id, window, cx));
    }

    pub(crate) fn show_surface_agent_form(
        &self,
        id: u64,
        view: Entity<crate::gpui::DialogView>,
        cx: &mut Context<Self>,
    ) {
        if let Some((request, panel)) = &self.surfaces.chooser
            && request.id == id
        {
            panel.update(cx, |panel, cx| panel.show_agent_form(view, cx));
        }
    }
    pub(crate) fn open_surface_chooser(
        &mut self,
        request: PendingNewSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_surface_chooser(None, window, cx);
        if request.placement == SurfacePlacement::Tab {
            self.surfaces.chooser_previous_center = self.center_task.clone();
            let key = crate::workspace_composition::surface_center_key(
                &request.binding.handle,
                &request.task_identity,
                "chooser",
                &request.id.to_string(),
            );
            self.select_center(key, window, cx);
        }
        let panel = cx.new(|cx| SurfacePanel::chooser(request.clone(), cx));
        subscribe_commands(&panel, &cx.weak_entity(), window, cx);
        self.place_center_panel(
            panel_handle(panel.clone()),
            &request.parent,
            request.placement,
            window,
            cx,
        );
        self.surfaces.chooser = Some((request, panel.clone()));
        panel.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(crate) fn close_surface_chooser(
        &mut self,
        request: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_surface_chooser(request, None, window, cx);
    }

    fn finish_surface_chooser(
        &mut self,
        request: Option<u64>,
        replacement: Option<&std::sync::Arc<dyn BasePanelView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((request, panel)) = self
            .surfaces
            .chooser
            .take_if(|(held, _)| request.is_none_or(|id| held.id == id))
        else {
            return false;
        };
        let previous = self.surfaces.chooser_previous_center.take();
        let parent = self.center_parent_panel(&request.parent, cx);
        let chooser = PanelId::from(panel.entity_id());
        let replaced = self.area.update(cx, |area, cx| {
            let Some(tree) = area.layout(DockPlacement::Center) else {
                return false;
            };
            if !tree.contains_panel(chooser) {
                return false;
            }
            let measured = area.dump(cx).center;
            let layout = splice_chooser(
                area,
                tree.root(),
                &measured,
                chooser,
                parent,
                replacement,
                cx,
            )
            .unwrap_or_else(DockLayout::tabs);
            area.set_center(layout, window, cx);
            true
        });
        if replacement.is_none() {
            if let Some(previous) = previous {
                let transient = self.center_task.clone();
                self.select_center(previous, window, cx);
                if let Some(transient) = transient {
                    self.centers.remove(&transient);
                }
            }
            let focus = parent.and_then(|parent| {
                self.area
                    .read(cx)
                    .panel(parent)
                    .map(|panel| panel.focus_handle(cx))
            });
            if let Some(focus) = focus {
                focus.focus(window, cx);
            }
        }
        cx.notify();
        replaced
    }

    pub(crate) fn reconcile_terminal_surfaces(
        &mut self,
        binding: &bootty_mux::workspace::BindingRuntime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring.is_some() || !binding.mux().has_session_snapshot() {
            return;
        }
        let closed = self
            .surfaces
            .terminals
            .borrow()
            .values()
            .filter_map(|surface| {
                let panel = surface.read(cx);
                let origin = panel.terminal_origin()?;
                let view = panel.terminal_view()?;
                (origin.binding_id == binding.scope().persistence_value().to_string()
                    && crate::workspace_composition::terminal_window_is_closed(
                        binding,
                        &origin.task_identity,
                        &view.read(cx).window_id,
                    ))
                .then(|| (surface.clone(), origin.clone()))
            })
            .collect::<Vec<_>>();
        self.area.update(cx, |area, cx| {
            for (panel, _) in &closed {
                area.remove_panel(panel.clone(), window, cx);
            }
        });
        for (_, origin) in closed {
            for center in self.centers.values_mut() {
                if !crate::workspace_composition::center_contains_window(center, &origin) {
                    continue;
                }
                *center =
                    crate::workspace_composition::remove_terminal_surface(center.clone(), &origin)
                        .unwrap_or_else(|| PanelState {
                            panel_name: "TabPanel".to_owned(),
                            children: Vec::new(),
                            info: PanelInfo::tabs(0),
                        });
            }
        }
    }

    fn place_center_panel(
        &mut self,
        panel: std::sync::Arc<dyn gpui_kit::component::dock::BasePanelView>,
        parent: &SurfaceParent,
        placement: SurfacePlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent_id = self.center_parent_panel(parent, cx);
        let node = parent_id.and_then(|parent| {
            self.area
                .read(cx)
                .layout(DockPlacement::Center)?
                .find_panel_node(parent)
        });
        if matches!(placement, SurfacePlacement::Split(_))
            && !matches!(parent, SurfaceParent::Binding(_))
            && node.is_none()
        {
            self.error = Some("The captured pane is no longer open".to_owned());
            return;
        }
        let id = panel.panel_id(cx);
        self.area.update(cx, |area, cx| {
            area.add_panel_view(panel, DockPlacement::Center, None, window, cx);
            if let Some(node) = node {
                let target = match placement {
                    SurfacePlacement::Tab => InsertTarget::Tabs {
                        node,
                        ix: None,
                        activate: true,
                    },
                    SurfacePlacement::Split(direction) => InsertTarget::Split {
                        node,
                        placement: match direction {
                            bootty_mux::pane_layout::SplitDirection::Right => {
                                gpui_kit::component::Placement::Right
                            }
                            bootty_mux::pane_layout::SplitDirection::Down => {
                                gpui_kit::component::Placement::Bottom
                            }
                        },
                        size: None,
                    },
                };
                area.move_panel(id, target, window, cx);
            }
        });
    }
    fn center_parent_panel(&self, parent: &SurfaceParent, cx: &App) -> Option<PanelId> {
        match parent {
            SurfaceParent::Conversation(_) | SurfaceParent::Terminal(_) => {
                let attachment = PanelId::from(self.attachment.entity_id());
                if self
                    .area
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .is_some_and(|tree| tree.contains_panel(attachment))
                {
                    return Some(attachment);
                }
                self.terminal_panel_for_parent(parent, cx).map(|panel| {
                    self.surfaces
                        .terminals
                        .borrow()
                        .values()
                        .find(|surface| surface.read(cx).terminal_view().as_ref() == Some(&panel))
                        .map_or_else(
                            || PanelId::from(panel.entity_id()),
                            |surface| PanelId::from(surface.entity_id()),
                        )
                })
            }
            SurfaceParent::Binding(_) => None,
        }
    }
}
