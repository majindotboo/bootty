//! Provider-owned children share their parent's read-only transcript path.
use super::*;
use bootty_agents::NativeToolStatus;

#[derive(Default)]
pub(super) struct SubagentConversation {
    pub(super) items: Vec<NativeTranscriptItem>,
    pub(super) states: Vec<Entity<TextViewState>>,
    pub(super) error: Option<String>,
}

impl NativeAgentSessionView {
    pub(crate) fn open_parent(&self, cx: &mut Context<Self>) {
        cx.emit(OpenNativeSession::Related {
            target: self.record.target(),
        });
    }
    pub(crate) fn subagent(&self, id: &str) -> Option<&bootty_agents::NativeSubagent> {
        if let Some(agent) = self
            .related
            .iter()
            .filter_map(|related| related.subagent.as_ref())
            .find(|detail| detail.agent.id == id)
        {
            return Some(&agent.agent);
        }
        self.record
            .snapshot
            .transcript
            .iter()
            .find_map(|item| item.subagent.as_ref().filter(|agent| agent.id == id))
    }

    pub(super) fn accept_subagent_read(
        &mut self,
        pending: &str,
        value: Value,
        cx: &mut Context<Self>,
    ) {
        let Ok(detail) = serde_json::from_value::<bootty_agents::NativeSubagentDetail>(value)
        else {
            self.subagent_read_error(pending);
            return;
        };
        if pending.strip_prefix("subagent-read:") != Some(detail.agent.id.as_str()) {
            self.subagent_read_error(pending);
            return;
        }
        self.accept_subagent_detail(detail, cx);
    }

    pub(super) fn accept_subagent_detail(
        &mut self,
        detail: bootty_agents::NativeSubagentDetail,
        cx: &mut Context<Self>,
    ) {
        for item in &mut self.record.snapshot.transcript {
            if let Some(agent) = &mut item.subagent
                && agent.id == detail.agent.id
                && agent.owner_session == detail.agent.owner_session
            {
                *agent = detail.agent.clone();
            }
        }
        if self
            .subagent_details
            .get(&detail.agent.id)
            .is_some_and(|old| old.items == detail.transcript && old.error.is_none())
        {
            return;
        }
        let states = detail
            .transcript
            .iter()
            .filter(|item| !item.text.is_empty() || item.tool.is_some())
            .map(|item| {
                let markdown = if item.text.is_empty() {
                    item.tool
                        .as_ref()
                        .map_or_else(String::new, tool_input_markdown)
                } else {
                    transcript_markdown(item)
                };
                cx.new(|cx| TextViewState::markdown(&markdown, cx))
            })
            .collect();
        self.subagent_details.insert(
            detail.agent.id,
            SubagentConversation {
                items: detail.transcript,
                states,
                error: None,
            },
        );
    }

    pub(super) fn subagent_read_error(&mut self, pending: &str) {
        if let Some(id) = pending.strip_prefix("subagent-read:") {
            self.subagent_details
                .entry(id.to_owned())
                .or_default()
                .error = Some("Could not refresh this conversation. Try again.".into());
        }
    }

    pub(crate) fn refresh_subagent(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        let Some(agent) = self.subagent(id) else {
            return;
        };
        if !agent.readable() {
            return;
        }
        if agent.owner_session != self.record.snapshot.session_id {
            self.subagent_details
                .entry(id.to_owned())
                .or_default()
                .error = Some("This subagent belongs to the source conversation.".into());
            cx.notify();
            return;
        }
        if self.provider_enabled() {
            self.command("subagent-read", vec![id.to_owned()], window, cx);
        } else {
            self.subagent_read_error(&format!("subagent-read:{id}"));
            cx.notify();
        }
    }

    pub(crate) fn subagents(&self) -> Vec<&bootty_agents::NativeSubagent> {
        let mut agents = BTreeMap::new();
        for related in &self.related {
            if let Some(detail) = &related.subagent {
                agents.insert(&detail.agent.id, &detail.agent);
            }
        }
        for item in &self.record.snapshot.transcript {
            if let Some(agent) = &item.subagent
                && agent
                    .owner_session
                    .as_deref()
                    .is_none_or(|owner| self.record.snapshot.session_id.as_deref() == Some(owner))
            {
                agents.insert(&agent.id, agent);
            }
        }
        agents.into_values().collect()
    }

    pub(crate) fn conversation_id(&self) -> &str {
        &self.record.id
    }

    pub(crate) fn conversation_title(&self) -> &str {
        &self.record.title
    }

    pub(crate) fn open_subagent(&mut self, id: String, cx: &mut Context<Self>) {
        self.working_indicator.subagents_expanded = false;
        cx.notify();
        cx.emit(OpenNativeSession::Subagent {
            source: self.record.target(),
            id,
        });
    }

    pub(crate) fn render_subagent_entry(
        agent: &bootty_agents::NativeSubagent,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let id = agent.id.clone();
        let status = super::subagent_status_label(agent.status);
        Button::new(format!("subagent-open:{id}"))
            .debug_selector({
                let id = id.clone();
                move || format!("subagent-open:{id}")
            })
            .ghost()
            .small()
            .w_full()
            .justify_start()
            .accessibility_label(format!("Open subagent {}", subagent_label(agent)))
            .child(subagent_avatar(agent, cx))
            .when_some(agent.provider, |row, provider| {
                let label = provider.display_name();
                row.child(
                    div()
                        .text_xs()
                        .text_color(crate::gpui::provider_color(label, cx))
                        .child(label),
                )
            })
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
            .on_click(cx.listener(move |this, _, _, cx| this.open_subagent(id.clone(), cx)))
    }

    pub(super) fn render_subagent_group(&self, cx: &Context<Self>) -> impl IntoElement {
        use gpui_kit::component::popover::Popover;
        let agents = self.subagents();
        let working = agents
            .iter()
            .filter(|agent| agent.status == NativeToolStatus::Running)
            .count();
        let failed = agents
            .iter()
            .filter(|agent| agent.status == NativeToolStatus::Failed)
            .count();
        let label = format!(
            "{} {}",
            agents.len(),
            if agents.len() == 1 {
                "subagent"
            } else {
                "subagents"
            }
        );
        let accessibility = format!("View {label}: {working} running, {failed} failed");
        div().flex().flex_none().when(!agents.is_empty(), |view| {
            view.child(
                div().flex().items_center().child(
                    Popover::new("subagent-track")
                        .anchor(gpui_kit::Anchor::BottomLeft)
                        .p_0()
                        .open(self.working_indicator.subagents_expanded)
                        .trigger(
                            Button::new("subagent-group")
                                .debug_selector(|| "subagent-group".into())
                                .small()
                                .h_7()
                                .outline()
                                .rounded_full()
                                .px_3()
                                .bg(cx.theme().background)
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    div().flex().items_center().children(
                                        agents
                                            .iter()
                                            .take(3)
                                            .map(|agent| subagent_avatar(agent, cx)),
                                    ),
                                )
                                .child(div().text_sm().child(label))
                                .when(working > 0, |pill| {
                                    pill.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().foreground)
                                            .child(format!("· {working} running")),
                                    )
                                })
                                .when(failed > 0, |pill| {
                                    pill.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .child(format!("· {failed} failed")),
                                    )
                                })
                                .accessibility_label(accessibility),
                        )
                        .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                            this.working_indicator.subagents_expanded = *open;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .id("subagent-group-entries")
                                .w(gpui_kit::rems(32.))
                                .max_h(gpui_kit::rems(27.5))
                                .overflow_y_scroll()
                                .p_1()
                                .flex()
                                .flex_col()
                                .children(
                                    agents
                                        .into_iter()
                                        .map(|agent| Self::render_subagent_entry(agent, cx)),
                                ),
                        ),
                ),
            )
        })
    }

    pub(crate) fn render_subagent_conversation(&self, id: &str, cx: &App) -> gpui_kit::AnyElement {
        let Some(agent) = self.subagent(id) else {
            return div()
                .p_4()
                .child("This subagent is no longer available in the parent conversation.")
                .into_any_element();
        };
        let unavailable = self
            .subagent_details
            .get(id)
            .is_some_and(|detail| detail.error.is_some());
        div().w_full().flex().flex_col().gap_4()
            .child(div().debug_selector({ let status = agent.status; move || format!("subagent-status:{status:?}") })
                .flex().items_center().gap_2().text_xs().text_color(cx.theme().muted_foreground)
                .child(if unavailable { "Status unavailable" } else { super::subagent_status_label(agent.status) })
                .when_some(agent.model.clone(), ParentElement::child))
            .when(!agent.prompt.is_empty(), |view| view.child(div().p_3().rounded_lg()
                .bg(cx.theme().muted).text_sm().child(agent.prompt.clone())))
            .when_some(self.subagent_details.get(id), |view, conversation| {
                view.when_some(conversation.error.as_ref(), |view, error| view.child(div().text_sm()
                    .text_color(cx.theme().muted_foreground).child(crate::error_catalog::ErrorNotice::from_text(error).to_string())))
                .children(conversation.items.iter().filter(|item| !item.text.is_empty() || item.tool.is_some()).zip(&conversation.states)
                    .map(|(item, state)| div().id(format!("subagent-message:{}", item.id))
                        .debug_selector({ let id = item.id.clone(); move || format!("subagent-message:{id}") })
                        .flex().flex_col().gap_1()
                        .child(div().text_xs().text_color(cx.theme().muted_foreground)
                            .child(item.tool.as_ref().map_or_else(|| item.role.clone(), |tool| super::tool_label(&tool.name).into_owned())))
                        .child(TextView::new(state).selectable(true))))
            })
            .when(!agent.readable(), |view| {
                let item = self.record.snapshot.transcript.iter().find(|item| item.subagent.as_ref().is_some_and(|agent| agent.id == id));
                view.when_some(item.and_then(|item| self.transcript.get(&item.id)), |view, state| {
                    view.child(TextView::new(state).selectable(true))
                })
            })
            .when(!self.subagent_details.contains_key(id), |view| view.child(div().text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if agent.readable() {
                    "Loading conversation…"
                } else {
                    "This provider reports the subagent's progress and result in the parent conversation."
                })))
            .into_any_element()
    }
}
