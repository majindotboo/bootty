//! Native navigation panels backed by the shared chrome projection.

use crate::gpui::chrome::GpuiChrome;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement, Render, Styled, Window, div,
};

fn panel_state(name: &str) -> PanelState {
    PanelState {
        panel_name: name.into(),
        children: Vec::new(),
        info: PanelInfo::panel(serde_json::Value::Null),
    }
}

pub struct SessionsPanel {
    chrome: Entity<GpuiChrome>,
    focus: FocusHandle,
}

impl SessionsPanel {
    pub(crate) fn new(chrome: Entity<GpuiChrome>, cx: &mut Context<Self>) -> Self {
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();
        Self {
            chrome,
            focus: cx.focus_handle(),
        }
    }
}

impl EventEmitter<PanelEvent> for SessionsPanel {}
impl Focusable for SessionsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for SessionsPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.sessions"
    }
    fn dump(&self, _: &App) -> PanelState {
        panel_state(self.panel_name())
    }
}
impl Panel for SessionsPanel {
    fn tab_name(&self, _: &App) -> Option<gpui_kit::SharedString> {
        Some("Sessions".into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Sessions"
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for SessionsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.chrome.update(cx, GpuiChrome::dock_sidebar);
        div()
            .track_focus(&self.focus)
            // The dock frame is focusable for keyboard navigation, not blank clicks.
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| {
                window.prevent_default();
            })
            .size_full()
            .overflow_hidden()
            .children(content)
    }
}
