//! A provider subagent's conversation in its own tab, drawn the way its parent's is. The
//! provider keeps the subagent's writer; the tab reads it through the parent.
use crate::gpui_agent_session::NativeAgentSessionView;
use gpui_kit::component::{
    ActiveTheme as _,
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
    /// The subagent's conversation, drawn by the conversation view.
    view: Option<Entity<NativeAgentSessionView>>,
    /// The child's status when this tab last read it.
    read_status: Option<bootty_agents::NativeToolStatus>,
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
            view: None,
            read_status: None,
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

    fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.read_status = self.status(cx);
        if let Some(parent) = &self.parent {
            parent.update(cx, |parent, cx| {
                parent.refresh_subagent(&self.id, window, cx);
            });
        }
    }

    /// The child's status as its parent last reported it.
    fn status(&self, cx: &App) -> Option<bootty_agents::NativeToolStatus> {
        self.parent
            .as_ref()
            .and_then(|parent| parent.read(cx).subagent(&self.id))
            .map(|agent| agent.status)
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
                        // Read-only provider threads have no subscription. Follow the child while
                        // it runs, and read once more when its parent reports it settled or
                        // started a later turn.
                        let status = this.status(cx);
                        if status == Some(bootty_agents::NativeToolStatus::Running)
                            || status != this.read_status
                        {
                            this.refresh(window, cx);
                        }
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
        let (Some(agent), Some(record)) = (
            parent.read(cx).subagent(&self.id).cloned(),
            parent.read(cx).subagent_record(&self.id),
        ) else {
            return div()
                .size_full()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("This subagent is no longer in its parent conversation.")
                .into_any_element();
        };
        let pending = parent.read(cx).subagent_pending(&self.id);
        if let Some(view) = &self.view {
            if !view.read(cx).shows(&agent, &record) {
                let view = view.clone();
                cx.defer_in(window, move |_, window, cx| {
                    view.update(cx, |view, cx| view.show_subagent(agent, record, window, cx));
                });
            }
        } else {
            let sender = parent.read(cx).command_sender();
            self.view =
                Some(cx.new(|cx| {
                    NativeAgentSessionView::new_subagent(agent, record, sender, window, cx)
                }));
        }
        div()
            .id("subagent-conversation-tab")
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .when_some(pending, |tab, pending| {
                tab.child(
                    div()
                        .debug_selector(|| "subagent-pending".into())
                        .p_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(pending),
                )
            })
            .children(self.view.clone())
            .into_any_element()
    }
}
