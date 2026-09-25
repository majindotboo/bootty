//! Factories scoped to a `DockArea`, with saved placeholders for unavailable panels.

use gpui_kit::component::dock::{
    BasePanel, BasePanelView, DockArea, Panel, PanelEvent, PanelInfo, panel_handle, register_panel,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, Global,
    IntoElement, ParentElement, Render, Styled, Window, div,
};
use std::{collections::HashMap, rc::Rc, sync::Arc};

pub(super) type PanelFactory =
    Rc<dyn Fn(&PanelInfo, &mut Window, &mut App) -> Arc<dyn BasePanelView>>;

#[derive(Default)]
struct NativePanels(HashMap<(EntityId, String), PanelFactory>);
impl Global for NativePanels {}

pub(super) fn register_factory(
    area: &Entity<DockArea>,
    name: &str,
    factory: PanelFactory,
    cx: &mut App,
) {
    if cx.try_global::<NativePanels>().is_none() {
        cx.set_global(NativePanels::default());
    }
    cx.global_mut::<NativePanels>()
        .0
        .insert((area.entity_id(), name.to_owned()), factory);
    let name = name.to_owned();
    register_panel(cx, &name.clone(), move |context, window, cx| {
        let factory = cx
            .global::<NativePanels>()
            .0
            .get(&(context.dock_area().entity_id(), name.clone()))
            .cloned();
        if let Some(factory) = factory {
            factory(context.info(), window, cx)
        } else {
            let state = context.state().clone();
            panel_handle(cx.new(|cx| UnavailablePanel {
                state,
                focus: cx.focus_handle(),
            }))
        }
    });
}
struct UnavailablePanel {
    state: gpui_kit::component::dock::PanelState,
    focus: FocusHandle,
}
impl BasePanel for UnavailablePanel {
    fn panel_name(&self) -> &'static str {
        "bootty.unavailable"
    }
    fn dump(&self, _: &App) -> gpui_kit::component::dock::PanelState {
        self.state.clone()
    }
}
impl Panel for UnavailablePanel {}
impl EventEmitter<PanelEvent> for UnavailablePanel {}
impl Focusable for UnavailablePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for UnavailablePanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_2().child(format!(
            "The {} panel is unavailable in this workspace.",
            self.state.panel_name
        ))
    }
}

pub(super) fn register<P: Panel>(area: &Entity<DockArea>, panel: Entity<P>, cx: &mut App) {
    let handle = panel_handle(panel);
    let name = handle.panel_name(cx);
    register_factory(area, name, Rc::new(move |_, _, _| handle.clone()), cx);
}

pub(super) fn unregister(area: EntityId, cx: &mut App) {
    cx.global_mut::<NativePanels>()
        .0
        .retain(|(id, _), _| *id != area);
}
