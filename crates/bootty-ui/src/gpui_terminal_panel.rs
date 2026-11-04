//! A stable Dock panel per server pane. Terminal views remain owned by the workspace.
use crate::{
    gpui::{GpuiPane, GpuiPaneSnapshot},
    gpui_terminal_view::CachedTerminalView,
    gpui_workspace::GpuiWorkspace,
};
use bootty_mux::workspace::ScopedWindowId;
use gpui_kit::component::ElementExt as _;
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, Subscription, WeakEntity, Window, div, prelude::*,
};

pub struct TerminalPanel {
    pub(crate) key: String,
    pub(crate) pane_id: String,
    pub(crate) window_id: ScopedWindowId,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) active: bool,
    title: SharedString,
    owner: WeakEntity<GpuiWorkspace>,
    focus: FocusHandle,
    snapshot: Option<GpuiPaneSnapshot<CachedTerminalView>>,
    _focus_subscription: Subscription,
}
impl TerminalPanel {
    pub(crate) fn new(
        key: String,
        pane_id: String,
        window_id: ScopedWindowId,
        owner: WeakEntity<GpuiWorkspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscription = cx.on_focus(&focus, window, |this, window, cx| {
            let owner = this.owner.clone();
            let pane = this.pane_id.clone();
            window.defer(cx, move |window, cx| {
                _ = owner.update(cx, |owner, cx| owner.focus_terminal_pane(&pane, window, cx));
            });
        });
        Self {
            key,
            pane_id,
            window_id,
            bounds: None,
            active: false,
            title: "Terminal".into(),
            owner,
            focus,
            snapshot: None,
            _focus_subscription: subscription,
        }
    }
    pub(crate) fn set_window(&mut self, id: ScopedWindowId, title: String) {
        self.window_id = id;
        self.title = title.into();
    }
    pub(crate) fn publish(
        &mut self,
        snapshot: GpuiPaneSnapshot<CachedTerminalView>,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = &mut self.snapshot {
            previous.animation_seconds = snapshot.animation_seconds;
        }
        if self.snapshot.as_ref() != Some(&snapshot) {
            self.snapshot = Some(snapshot);
            cx.notify();
        }
    }
}
impl EventEmitter<PanelEvent> for TerminalPanel {}
impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.terminal-pane"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.active != active {
            self.active = active;
            let owner = self.owner.clone();
            cx.defer(move |cx| {
                _ = owner.update(cx, |_, cx| cx.notify());
            });
        }
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.active = false;
        let owner = self.owner.clone();
        cx.defer(move |cx| {
            _ = owner.update(cx, |_, cx| cx.notify());
        });
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.panel_name().into(),
            children: Vec::new(),
            info: PanelInfo::panel(serde_json::json!({"pane": self.pane_id, "key": self.key})),
        }
    }
}
impl Panel for TerminalPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone())
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let terminal = self.snapshot.clone().map(|snapshot| {
            let owner = self.owner.clone();
            GpuiPane::new(snapshot, move |invocation, window, cx| {
                _ = owner.update(cx, |owner, cx| {
                    owner.invoke_gpui_command(invocation, window, cx);
                });
            })
            .into_any_element()
        });
        let content = self
            .owner
            .update(cx, |owner, cx| owner.terminal_panel_content(terminal, cx))
            .ok();
        let weak = cx.weak_entity();
        div()
            .size_full()
            .track_focus(&self.focus)
            .overflow_hidden()
            .on_prepaint(move |bounds, _, cx| {
                _ = weak.update(cx, |this, cx| {
                    if this.bounds != Some(bounds) {
                        this.bounds = Some(bounds);
                        let owner = this.owner.clone();
                        cx.defer(move |cx| {
                            _ = owner.update(cx, |_, cx| cx.notify());
                        });
                    }
                });
            })
            .children(content)
    }
}
