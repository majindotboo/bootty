//! Stable pane entities and translation of Dock events into mux commands.
use super::{WorkspaceDock, register_factory};
use crate::{gpui::DraggedPane, gpui_terminal_panel::TerminalPanel};
use bootty_control::{CommandInvocation, CommandTarget};
use bootty_mux::workspace::{BindingRuntime, ScopedWindowId};
use gpui_kit::component::dock::{
    AnyDrag, DockArea, DockPlacement, DropTarget, PaneRef, PanelId, PanelInfo, panel_handle,
};
use gpui_kit::{App, AppContext as _, Context, Entity, Window, px};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

#[derive(Default)]
pub(super) struct PanePanels(Rc<RefCell<HashMap<String, Entity<TerminalPanel>>>>);
impl PanePanels {
    pub(super) fn new(area: &Entity<DockArea>, cx: &mut App) -> Self {
        let panels = Self::default();
        let held = panels.0.clone();
        register_factory(
            area,
            "bootty.terminal-pane",
            Rc::new(move |info, _, _| {
                let PanelInfo::Panel(info) = info else {
                    return None;
                };
                let key = info.get("key").and_then(serde_json::Value::as_str)?;
                held.borrow().get(key).cloned().map(panel_handle)
            }),
            cx,
        );
        panels
    }
}

impl WorkspaceDock {
    pub(crate) fn visible_terminal_surface_panels(&self, cx: &App) -> Vec<Entity<TerminalPanel>> {
        let area = self.area.read(cx);
        self.panes
            .0
            .borrow()
            .values()
            .filter(|panel| {
                area.layout(DockPlacement::Center)
                    .is_some_and(|tree| tree.contains_panel(PanelId::from(panel.entity_id())))
            })
            .cloned()
            .collect()
    }
    pub(crate) fn select_terminal_panel(
        &self,
        selected: &ScopedWindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self
            .visible_terminal_surface_panels(cx)
            .into_iter()
            .find(|panel| {
                panel.read(cx).window_id == *selected
                    && self.focused_pane.as_deref() == Some(panel.read(cx).pane_id.as_str())
            })
        {
            self.area.update(cx, |area, cx| {
                area.select_panel(PanelId::from(panel.entity_id()), window, cx);
            });
        }
    }
    pub(crate) fn sync_terminals(
        &mut self,
        binding: &BindingRuntime,
        target: Option<CommandTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring.is_some() {
            return;
        }
        let selected = binding.current_window_id();
        let Some(observed) = binding
            .mux()
            .sessions()
            .iter()
            .find(|session| session.id == selected.session_id())
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == selected.window_id())
            })
        else {
            self.clear_terminals(window, cx);
            return;
        };
        let Some(layout) = observed.layout.clone() else {
            self.clear_terminals(window, cx);
            return;
        };
        self.pane_target = target;
        self.focused_pane.clone_from(&observed.anchor.pane_id);
        let live = binding
            .mux()
            .sessions()
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .filter_map(|pane| pane.pane_id.as_deref())
            .map(|pane| binding.pane_widget_key(pane))
            .collect::<std::collections::HashSet<_>>();
        self.panes
            .0
            .borrow_mut()
            .retain(|key, _| live.contains(key));
        let title = &observed.name;
        let Some(projected) = bootty_mux::pane_layout::PaneLayout::from_mux_layout(&layout) else {
            self.error = Some("The native mux returned an invalid pane layout".into());
            return;
        };
        for pane in projected.panes() {
            let key = binding.pane_widget_key(&pane);
            let panel = self
                .panes
                .0
                .borrow_mut()
                .entry(key.clone())
                .or_insert_with(|| {
                    cx.new(|cx| {
                        TerminalPanel::new(
                            key,
                            pane,
                            selected.clone(),
                            self.owner.clone(),
                            window,
                            cx,
                        )
                    })
                })
                .clone();
            panel.update(cx, |panel, _| {
                panel.set_window(selected.clone(), title.to_owned());
            });
        }
        if self.projected.as_ref().is_some_and(|(id, previous)| {
            *id == selected && crate::workspace_composition::pane_layout_matches(previous, &layout)
        }) {
            return;
        }
        let area = self.area.read(cx);
        let mut size = window.viewport_size();
        if area.is_dock_open(DockPlacement::Left) {
            size.width -= area.dock_size(DockPlacement::Left).unwrap_or_default();
        }
        size.width = size.width.max(px(1.));
        size.height = (size.height - self.titlebar.read(cx).height()).max(px(1.));
        let mut state = area.dump(cx);
        state.center = crate::workspace_composition::pane_layout_state(&layout, size, &|pane| {
            binding.pane_widget_key(pane)
        });
        self.projected = Some((selected, layout));
        self.pending_resize = None;
        self.error = self
            .area
            .update(cx, |area, cx| area.load(state, window, cx))
            .err()
            .map(|error| error.to_string());
    }
    pub(super) fn drop_pane(
        &mut self,
        item: &AnyDrag,
        target: DropTarget,
        window: &Window,
        cx: &Context<Self>,
    ) {
        let Some(invocation) = self.pane_drop_invocation(item, target, cx) else {
            return;
        };
        self.submit_command(invocation, window, cx);
    }
    fn pane_drop_invocation(
        &self,
        item: &AnyDrag,
        target: DropTarget,
        cx: &App,
    ) -> Option<CommandInvocation> {
        let drag = item.value().downcast_ref::<DraggedPane>()?;
        if self.pane_target.as_ref() != Some(&drag.target) {
            return None;
        }
        let area = self.area.read(cx);
        let node = area
            .layout(DockPlacement::Center)?
            .find_node(target.node())?;
        let PaneRef::Tabs { panels, .. } = node.kind() else {
            return None;
        };
        let [id] = panels else {
            return None;
        };
        let panels = self.panes.0.borrow();
        let target_pane = panels
            .values()
            .find(|panel| PanelId::from(panel.entity_id()) == *id)?
            .read(cx);
        crate::workspace_composition::pane_drop_command(
            drag,
            &target_pane.pane_id,
            target.placement(),
        )
    }
}

/// Read committed Dock slot preferences, not measurements changed by a window resize.
impl WorkspaceDock {
    pub(super) fn pane_layout(&self, cx: &App) -> Option<bootty_mux::snapshot::MuxPaneLayout> {
        struct Source<'a> {
            area: &'a DockArea,
            cx: &'a App,
        }
        impl gpui_kit::component::dock::PanelSource for Source<'_> {
            fn panel_name(&self, id: PanelId) -> &'static str {
                self.area
                    .panel(id)
                    .map_or("", |panel| panel.panel_name(self.cx))
            }
            fn is_visible(&self, _: PanelId) -> bool {
                true
            }
            fn dump(&self, id: PanelId) -> gpui_kit::component::dock::PanelState {
                self.area
                    .panel(id)
                    .map_or_else(gpui_kit::component::dock::PanelState::default, |panel| {
                        panel.dump(self.cx)
                    })
            }
        }
        let area = self.area.read(cx);
        let state = area
            .layout(DockPlacement::Center)?
            .to_state(&Source { area, cx });
        crate::workspace_composition::mux_pane_layout(&state)
    }
}
