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

#[derive(Clone, PartialEq, Eq)]
pub enum WorkspacePaneView {
    Terminal(CachedTerminalView),
    NativeAgent(Entity<crate::gpui_agent_session::NativeAgentSessionView>),
    NativePlaceholder {
        loading: bool,
        background: gpui_kit::Hsla,
        foreground: gpui_kit::Hsla,
    },
}

impl IntoElement for WorkspacePaneView {
    type Element = gpui_kit::AnyElement;

    fn into_element(self) -> Self::Element {
        match self {
            Self::Terminal(view) => view.into_any_element(),
            Self::NativeAgent(view) => view.into_any_element(),
            Self::NativePlaceholder {
                loading,
                background,
                foreground,
            } => div()
                .size_full()
                .bg(background)
                .text_color(foreground)
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .when(loading, |view| {
                    view.child(gpui_kit::component::spinner::Spinner::new())
                })
                .child(if loading {
                    "Loading conversation…"
                } else {
                    "Conversation unavailable"
                })
                .into_any_element(),
        }
    }
}

/// Dock owns placement. The binding owns the window and all of its terminal runtimes.
pub struct TerminalPanel {
    origin: Option<crate::gpui_dock::surfaces::TerminalSurfaceOrigin>,
    binding_target: bootty_control::CommandTarget,
    pub(crate) window_id: ScopedWindowId,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) active: bool,
    pub(crate) visible: bool,
    title: SharedString,
    owner: WeakEntity<GpuiWorkspace>,
    focus: FocusHandle,
    prepared: Option<(TerminalGeometry, Vec<String>)>,
    snapshot: Option<GpuiPaneWorkspaceSnapshot<WorkspacePaneView>>,
    _focus_subscription: Subscription,
    _owner_subscription: Option<Subscription>,
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
        let focus_subscription = cx.on_focus(&focus, window, |this, window, cx| {
            this.focus_terminal(window, cx);
        });
        let owner_subscription = owner
            .upgrade()
            .map(|owner| cx.observe(&owner, |_, _, cx| cx.notify()));
        Self {
            origin: None,
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
            _owner_subscription: owner_subscription,
        }
    }

    pub(crate) fn with_origin(
        mut self,
        origin: crate::gpui_dock::surfaces::TerminalSurfaceOrigin,
    ) -> Self {
        self.origin = Some(origin);
        self
    }

    pub(crate) fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        _ = self.owner.update(cx, |owner, cx| {
            owner.focus_terminal_window(&self.binding_target, &self.window_id, window, cx);
        });
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
        snapshot: GpuiPaneWorkspaceSnapshot<WorkspacePaneView>,
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
        if self.origin.is_some() {
            "bootty.terminal-window"
        } else {
            "bootty.terminal"
        }
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
        let info = self.origin.as_ref().map_or_else(
            || {
                serde_json::json!({
                    "session": self.window_id.session_id(), "window": self.window_id.window_id(),
                })
            },
            |origin| serde_json::json!(origin),
        );
        PanelState {
            panel_name: self.panel_name().to_owned(),
            children: Vec::new(),
            info: PanelInfo::panel(info),
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
        let terminal = self.snapshot.clone().map(|snapshot| {
            let owner = self.owner.clone();
            let target = self.binding_target.clone();
            let id = self.window_id.clone();
            GpuiPaneWorkspace::new(snapshot, move |intent, window, cx| {
                _ = owner.update(cx, |owner, cx| {
                    owner.apply_terminal_window_intent(&target, &id, intent, window, cx);
                });
            })
            .with_single_pane_handle(true)
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

/// A backend-rendered client remains one surface, regardless of its inner topology.
pub struct TerminalAttachmentPanel {
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) active: bool,
    title: SharedString,
    terminal: Entity<GpuiTerminalView>,
    owner: WeakEntity<GpuiWorkspace>,
    _owner_subscription: Option<Subscription>,
    native_revision: u64,
    native_panes: Vec<(String, String, bootty_terminal::geometry::SurfaceRect)>,
}

impl TerminalAttachmentPanel {
    pub(crate) fn new(
        terminal: Entity<GpuiTerminalView>,
        owner: WeakEntity<GpuiWorkspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner_subscription = owner
            .upgrade()
            .map(|owner| cx.observe(&owner, |_, _, cx| cx.notify()));
        Self {
            bounds: None,
            active: false,
            title: "Terminal".into(),
            terminal,
            owner,
            _owner_subscription: owner_subscription,
            native_revision: 0,
            native_panes: Vec::new(),
        }
    }

    pub(crate) fn publish_native_layout(
        &mut self,
        revision: u64,
        panes: Vec<(String, String, bootty_terminal::geometry::SurfaceRect)>,
        cx: &mut Context<Self>,
    ) {
        if self.native_revision != revision || self.native_panes != panes {
            self.native_revision = revision;
            self.native_panes = panes;
            cx.notify();
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let terminal = CachedTerminalView(self.terminal.clone()).into_any_element();
        let content = self
            .owner
            .update(cx, |owner, cx| {
                owner.terminal_panel_content(Some(terminal), cx)
            })
            .ok();
        let weak = cx.weak_entity();
        let overlays = self.bounds.map_or_default(|bounds| {
            self.owner
                .update(cx, |owner, cx| {
                    owner.native_agent_pane_overlays(bounds, &self.native_panes, window, cx)
                })
                .unwrap_or_default()
        });
        div()
            .relative()
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
            .children(overlays)
    }
}
