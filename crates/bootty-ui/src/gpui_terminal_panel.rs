use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::{
    ElementExt as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement,
    Pixels, Render, SharedString, Styled, Subscription, WeakEntity, Window, div, prelude::*,
};

use crate::{
    gpui::{GpuiPaneWorkspace, GpuiPaneWorkspaceSnapshot},
    gpui_terminal_view::{CachedTerminalView, GpuiTerminalView},
    gpui_workspace::GpuiWorkspace,
};
use bootty_mux::workspace::ScopedWindowId;
use bootty_terminal::geometry::TerminalGeometry;

/// Dock owns placement. The binding owns the window and all of its terminal runtimes.
pub struct TerminalPanel {
    binding_target: bootty_control::CommandTarget,
    pub(crate) window_id: ScopedWindowId,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) active: bool,
    pub(crate) visible: bool,
    title: SharedString,
    owner: WeakEntity<GpuiWorkspace>,
    focus: FocusHandle,
    prepared: Option<(TerminalGeometry, Vec<String>)>,
    snapshot: Option<GpuiPaneWorkspaceSnapshot<CachedTerminalView>>,
    _focus_subscription: Subscription,
}

impl TerminalPanel {
    pub(crate) fn new(
        binding_target: bootty_control::CommandTarget,
        window_id: ScopedWindowId,
        title: String,
        owner: WeakEntity<GpuiWorkspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let focus_subscription = cx.on_focus_in(&focus, window, |this, window, cx| {
            let id = this.window_id.clone();
            _ = this.owner.update(cx, |owner, cx| {
                owner.focus_terminal_window(&this.binding_target, &id, window, cx);
            });
        });
        Self {
            binding_target,
            window_id,
            title: title.into(),
            owner,
            focus,
            bounds: None,
            active: false,
            visible: true,
            snapshot: None,
            prepared: None,
            _focus_subscription: focus_subscription,
        }
    }

    pub(crate) fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        if self.title != title {
            self.title = title.into();
            cx.notify();
        }
    }

    pub(crate) fn set_binding_target(&mut self, target: bootty_control::CommandTarget) {
        self.binding_target = target;
    }

    pub(crate) fn set_window_id(&mut self, id: ScopedWindowId, cx: &mut Context<Self>) {
        if self.window_id != id {
            self.window_id = id;
            self.prepared = None;
            self.snapshot = None;
            cx.notify();
        }
    }

    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            cx.emit(PanelEvent::LayoutChanged);
            cx.notify();
        }
    }

    pub(crate) fn prepare(
        &mut self,
        geometry: TerminalGeometry,
        panes: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let preparation = (geometry, panes);
        if self.prepared.as_ref() == Some(&preparation) {
            return;
        }
        self.prepared = Some(preparation);
        cx.defer_in(window, |this, _, cx| {
            if !this.active || !this.visible {
                this.prepared = None;
                return;
            }
            let Some((geometry, _)) = this.prepared.as_ref() else {
                return;
            };
            _ = this.owner.update(cx, |owner, cx| {
                owner.prepare_terminal_window(&this.binding_target, &this.window_id, *geometry, cx);
            });
        });
    }

    pub(crate) fn publish(
        &mut self,
        title: String,
        snapshot: GpuiPaneWorkspaceSnapshot<CachedTerminalView>,
        cx: &mut Context<Self>,
    ) {
        let title: SharedString = title.into();
        // The owner schedules progress animation. Time alone must not cause a notify loop.
        if let Some(previous) = &mut self.snapshot {
            previous.animation_seconds = snapshot.animation_seconds;
        }
        let changed = self.title != title || self.snapshot.as_ref() != Some(&snapshot);
        self.title = title;
        self.snapshot = Some(snapshot);
        if changed {
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
    fn visible(&self, _: &App) -> bool {
        self.visible
    }
    fn panel_name(&self) -> &'static str {
        "bootty.terminal"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        let owner = self.owner.clone();
        cx.defer(move |cx| {
            _ = owner.update(cx, |_, cx| cx.notify());
        });
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
            panel_name: self.panel_name().to_owned(),
            children: Vec::new(),
            info: PanelInfo::panel(serde_json::json!({
                "session": self.window_id.session_id(), "window": self.window_id.window_id(),
            })),
        }
    }
}
impl Panel for TerminalPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }
    fn title_suffix(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        Some(
            Button::new("close-terminal")
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .size_4()
                .accessibility_label("Close terminal pane")
                .tooltip("Close terminal pane")
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    let id = this.window_id.clone();
                    _ = this.owner.update(cx, |owner, cx| {
                        owner.close_terminal_window(&this.binding_target, &id, cx);
                    });
                })),
        )
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> PopupMenu {
        let owner = self.owner.clone();
        let target = self.binding_target.clone();
        let id = self.window_id.clone();
        menu.item(
            PopupMenuItem::new("Close terminal pane").on_click(move |_, _, cx| {
                _ = owner.update(cx, |owner, cx| {
                    owner.close_terminal_window(&target, &id, cx);
                });
            }),
        )
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.snapshot.clone().and_then(|snapshot| {
            let owner = self.owner.clone();
            let target = self.binding_target.clone();
            let id = self.window_id.clone();
            let terminal = GpuiPaneWorkspace::new(snapshot, move |intent, window, cx| {
                _ = owner.update(cx, |owner, cx| {
                    owner.apply_terminal_window_intent(&target, &id, intent, window, cx);
                });
            })
            .with_single_pane_handle(true)
            .into_any_element();
            self.owner
                .update(cx, |owner, cx| {
                    owner.terminal_panel_content(Some(terminal), cx)
                })
                .ok()
        });
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

/// A backend-rendered client remains one surface, regardless of its inner topology.
pub struct TerminalAttachmentPanel {
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) active: bool,
    title: SharedString,
    terminal: Entity<GpuiTerminalView>,
    owner: WeakEntity<GpuiWorkspace>,
}

impl TerminalAttachmentPanel {
    pub(crate) fn new(
        terminal: Entity<GpuiTerminalView>,
        owner: WeakEntity<GpuiWorkspace>,
    ) -> Self {
        Self {
            bounds: None,
            active: false,
            title: "Terminal".into(),
            terminal,
            owner,
        }
    }

    pub(crate) fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        if self.title != title {
            self.title = title.into();
            cx.notify();
        }
    }
}
impl EventEmitter<PanelEvent> for TerminalAttachmentPanel {}
impl Focusable for TerminalAttachmentPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.focus_handle(cx)
    }
}
impl BasePanel for TerminalAttachmentPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.attachment"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        let owner = self.owner.clone();
        cx.defer(move |cx| {
            _ = owner.update(cx, |_, cx| cx.notify());
        });
    }
}
impl Panel for TerminalAttachmentPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for TerminalAttachmentPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let terminal = CachedTerminalView(self.terminal.clone()).into_any_element();
        let content = self
            .owner
            .update(cx, |owner, cx| {
                owner.terminal_panel_content(Some(terminal), cx)
            })
            .ok();
        let weak = cx.weak_entity();
        div()
            .size_full()
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
