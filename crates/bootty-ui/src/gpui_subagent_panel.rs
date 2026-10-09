//! Read-only child conversation tabs; the provider retains the child writer.
use crate::gpui_agent_session::NativeAgentSessionView;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState},
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Task, Window, div,
};

pub type SubagentPanels =
    std::rc::Rc<std::cell::RefCell<std::collections::BTreeMap<String, Entity<SubagentPanel>>>>;

pub struct SubagentPanel {
    parent: Option<Entity<NativeAgentSessionView>>,
    parent_id: String,
    refresh_on_render: bool,
    id: String,
    key: String,
    title: String,
    focus: FocusHandle,
    active: bool,
    tick: Option<Task<()>>,
    observe: Option<Subscription>,
}

impl SubagentPanel {
    /// Display a provider-owned child without acquiring its writer.
    pub fn new(
        parent: Entity<NativeAgentSessionView>,
        id: String,
        key: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = parent.read(cx).subagent(&id).map_or_else(
            || "Subagent".into(),
            crate::gpui_agent_session::subagent_label,
        );
        let mut panel = Self::pending(
            parent.read(cx).conversation_id().to_owned(),
            id,
            key,
            title,
            cx,
        );
        panel.attach_parent(parent, cx);
        panel
    }

    fn pending(
        parent_id: String,
        id: String,
        key: String,
        title: String,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            parent: None,
            parent_id,
            refresh_on_render: false,
            id,
            key,
            title,
            focus: cx.focus_handle(),
            active: false,
            tick: None,
            observe: None,
        }
    }

    pub(crate) fn restored(value: &serde_json::Value, cx: &mut App) -> Option<Entity<Self>> {
        let parent = value.get("parent")?.as_str()?.to_owned();
        let id = value.get("id")?.as_str()?.to_owned();
        let key = value.get("key")?.as_str()?.to_owned();
        let title = value
            .get("title")
            .and_then(serde_json::Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("Subagent")
            .chars()
            .take(80)
            .collect();
        Some(cx.new(|cx| Self::pending(parent, id, key, title, cx)))
    }

    pub(crate) fn parent_id(&self) -> &str {
        &self.parent_id
    }

    pub(crate) fn attach_parent(
        &mut self,
        parent: Entity<NativeAgentSessionView>,
        cx: &mut Context<Self>,
    ) {
        if self.parent.is_some() {
            return;
        }
        self.observe = Some(cx.observe(&parent, |this, parent, cx| {
            if let Some(agent) = parent.read(cx).subagent(&this.id) {
                this.title = crate::gpui_agent_session::subagent_label(agent);
            }
            cx.notify();
        }));
        self.parent = Some(parent);
        self.refresh_on_render = true;
        cx.notify();
    }

    fn refresh(&self, window: &Window, cx: &mut Context<Self>) {
        if let Some(parent) = &self.parent {
            parent.update(cx, |parent, cx| {
                parent.refresh_subagent(&self.id, window, cx);
            });
        }
    }
}
impl EventEmitter<PanelEvent> for SubagentPanel {}
impl Focusable for SubagentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for SubagentPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.subagent"
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.active == active && (!active || self.tick.is_some()) {
            return;
        }
        self.active = active;
        self.tick = None;
        if !active || self.parent.is_none() {
            return;
        }
        cx.defer_in(window, |this, window, cx| this.refresh(window, cx));
        self.tick = Some(cx.spawn_in(window, async move |owner, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                let keep = owner
                    .update_in(cx, |this, window, cx| {
                        if !this.active {
                            return false;
                        }
                        // Read-only provider threads have no subscription. Keep following
                        // the displayed child, including later turns after it becomes idle.
                        this.refresh(window, cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }
    fn on_removed(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.active = false;
        self.tick = None;
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.panel_name().into(),
            children: Vec::new(),
            info: PanelInfo::panel(
                serde_json::json!({"key": self.key, "parent": self.parent_id, "id": self.id, "title": self.title}),
            ),
        }
    }
}
impl Panel for SubagentPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone().into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .min_w_0()
            .max_w(gpui_kit::rems(10.))
            .text_ellipsis()
            .child(self.title.clone())
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for SubagentPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.refresh_on_render {
            self.refresh_on_render = false;
            cx.defer_in(window, |this, window, cx| {
                this.refresh(window, cx);
                if this.active && this.tick.is_none() {
                    this.set_active(true, window, cx);
                }
            });
        }
        let Some(parent) = self.parent.clone() else {
            return div()
                .size_full()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Open the parent conversation from Sessions to reconnect this tab.")
                .into_any_element();
        };
        let source = parent.clone();
        div()
            .id("subagent-conversation-tab")
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(
                div()
                    .p_3()
                    .flex()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Subagent · read-only"),
                    )
                    .child(
                        Button::new("subagent-refresh")
                            .ghost()
                            .small()
                            .label("Refresh")
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    )
                    .child(
                        Button::new("subagent-parent")
                            .ghost()
                            .small()
                            .label("Open parent")
                            .on_click(move |_, _, cx| {
                                parent.update(cx, |parent, cx| parent.open_parent(cx));
                            }),
                    ),
            )
            .child(
                div()
                    .id("subagent-conversation-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(
                        div()
                            .max_w(gpui_kit::rems(48.))
                            .mx_auto()
                            .child(source.read(cx).render_subagent_conversation(&self.id, cx)),
                    ),
            )
            .into_any_element()
    }
}
