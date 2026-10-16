//! Provider-owned subagents: their conversations read through the parent, and their tabs show
//! them the way the parent's view shows its own.
use super::*;
use bootty_agents::NativeToolStatus;

#[derive(Default)]
pub(super) struct SubagentConversation {
    pub(super) items: Vec<NativeTranscriptItem>,
    pub(super) error: Option<String>,
}

impl NativeAgentSessionView {
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

    pub(super) fn accept_subagent_read(&mut self, pending: &str, value: Value) {
        let Ok(detail) = serde_json::from_value::<bootty_agents::NativeSubagentDetail>(value)
        else {
            self.subagent_read_error(pending);
            return;
        };
        if pending.strip_prefix("subagent-read:") != Some(detail.agent.id.as_str()) {
            self.subagent_read_error(pending);
            return;
        }
        self.accept_subagent_detail(detail);
    }

    pub(super) fn accept_subagent_detail(&mut self, detail: bootty_agents::NativeSubagentDetail) {
        for item in &mut self.record.snapshot.transcript {
            if let Some(agent) = &mut item.subagent
                && agent.id == detail.agent.id
                && agent.owner_session == detail.agent.owner_session
            {
                *agent = detail.agent.clone();
            }
        }
        self.subagent_details.insert(
            detail.agent.id,
            SubagentConversation {
                items: detail.transcript,
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

    /// The command sender subagent views share with this conversation.
    pub(crate) fn command_sender(&self) -> BoundAppCommandSender {
        self.sender.clone()
    }

    /// A subagent's conversation as its tab shows it: this conversation's record carrying the
    /// subagent's name and transcript. A subagent without a readable conversation shows its prompt
    /// and the result this conversation saw.
    pub(crate) fn subagent_record(&self, id: &str) -> Option<NativeSessionRecord> {
        let agent = self.subagent(id)?;
        let transcript = if agent.readable() {
            self.subagent_details
                .get(id)
                .map_or_else(Vec::new, |detail| detail.items.clone())
        } else {
            let result = self
                .record
                .snapshot
                .transcript
                .iter()
                .find(|item| item.subagent.as_ref().is_some_and(|agent| agent.id == id))
                .map_or_else(String::new, |item| item.text.clone());
            [
                ("prompt", "user", agent.prompt.clone()),
                ("result", "assistant", result),
            ]
            .into_iter()
            .filter(|(_, _, text)| !text.is_empty())
            .filter_map(|(key, role, text)| {
                serde_json::from_value(serde_json::json!({
                    "id": format!("subagent-{key}:{id}"), "role": role, "text": text,
                    "complete": true
                }))
                .ok()
            })
            .collect()
        };
        let mut snapshot =
            NativeSessionSnapshot::new(agent.provider.unwrap_or(self.record.config.provider));
        snapshot.status = if agent.status == NativeToolStatus::Running {
            NativeSessionStatus::Working
        } else {
            NativeSessionStatus::Idle
        };
        snapshot.transcript = transcript;
        let mut record = self.record.clone();
        record.title = subagent_label(agent);
        record.snapshot = snapshot;
        record.queued_messages.clear();
        record.attachments.clear();
        record.draft = None;
        record.side_chat = None;
        record.pending_initial_message = None;
        Some(record)
    }

    /// Why a readable subagent's tab has nothing to show yet: its first read is on its way, or
    /// failed.
    pub(crate) fn subagent_pending(&self, id: &str) -> Option<String> {
        let agent = self.subagent(id)?;
        if !agent.readable() {
            return None;
        }
        match self.subagent_details.get(id) {
            None => Some("Loading conversation…".to_owned()),
            Some(detail) if detail.items.is_empty() => detail.error.clone(),
            Some(_) => None,
        }
    }

    /// A view that shows a subagent's conversation from `record`, made by
    /// [`Self::subagent_record`].
    pub(crate) fn new_subagent(
        record: NativeSessionRecord,
        sender: BoundAppCommandSender,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::new(record, sender, true, window, cx);
        view.subagent = Some(id);
        view.history_loaded = true;
        view
    }

    /// Show the subagent's latest name, status and transcript.
    pub(crate) fn show_subagent(
        &mut self,
        record: NativeSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shows(&record) {
            return;
        }
        let previous = std::mem::replace(&mut self.record, record)
            .snapshot
            .transcript;
        self.sync_transcript(&previous, window, cx);
        cx.notify();
    }

    /// Whether `record` holds what this subagent view already shows.
    pub(crate) fn shows(&self, record: &NativeSessionRecord) -> bool {
        record.title == self.record.title
            && record.snapshot.status == self.record.snapshot.status
            && record.snapshot.transcript == self.record.snapshot.transcript
    }
}
