//! Native navigation panels backed by the shared chrome projection.

use crate::gpui::chrome::GpuiChrome;
use crate::gpui_agent_session::{NativeAgentSessionView, subagent_avatar, subagent_label};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    Styled, Window, div,
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

/// The selected conversation's delegated subagents, shown with the right-side tool panels.
pub struct SubagentsPanel {
    focus: FocusHandle,
    parents: Vec<Entity<NativeAgentSessionView>>,
    observations: Vec<gpui_kit::Subscription>,
}

impl SubagentsPanel {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            parents: Vec::new(),
            observations: Vec::new(),
        }
    }

    pub(crate) fn update_subagents(
        &mut self,
        parents: Vec<Entity<NativeAgentSessionView>>,
        cx: &mut Context<Self>,
    ) {
        if self.parents == parents {
            return;
        }
        self.observations = parents
            .iter()
            .map(|parent| cx.observe(parent, |_, _, cx| cx.notify()))
            .collect();
        self.parents = parents;
        cx.notify();
    }

    fn render_subagents(&self, cx: &Context<Self>) -> impl IntoElement {
        let groups = self
            .parents
            .iter()
            .filter(|parent| !parent.read(cx).subagents().is_empty());
        let empty = groups.clone().next().is_none();
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("subagents-hub")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .when(empty, |view| {
                        view.child(
                            div()
                                .p_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No subagents"),
                        )
                    })
                    .children(groups.map(|parent| {
                        let view = parent.read(cx);
                        div()
                            .mb_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .px_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(view.conversation_title().to_owned()),
                            )
                            .children(view.subagents().into_iter().map(|agent| {
                                let owner = parent.clone();
                                let id = agent.id.clone();
                                let status =
                                    crate::gpui_agent_session::subagent_status_label(agent.status);
                                Button::new(format!("hub-subagent:{}:{id}", parent.entity_id()))
                                    .ghost()
                                    .small()
                                    .w_full()
                                    .justify_start()
                                    .child(subagent_avatar(agent, cx))
                                    .accessibility_label(format!(
                                        "Open subagent {}",
                                        subagent_label(agent)
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(subagent_label(agent)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(status),
                                    )
                                    .on_click(move |_, _, cx| {
                                        owner.update(cx, |parent, cx| {
                                            parent.open_subagent(id.clone(), cx);
                                        });
                                    })
                            }))
                    })),
            )
    }
}

impl EventEmitter<PanelEvent> for SubagentsPanel {}
impl Focusable for SubagentsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for SubagentsPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.subagents"
    }
    fn dump(&self, _: &App) -> PanelState {
        panel_state(self.panel_name())
    }
}
impl Panel for SubagentsPanel {
    fn tab_name(&self, _: &App) -> Option<gpui_kit::SharedString> {
        Some("Subagents".into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Subagents"
    }
}
impl Render for SubagentsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_subagents(cx)
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
