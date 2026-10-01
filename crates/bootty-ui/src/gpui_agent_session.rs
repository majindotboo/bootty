//! Native session presentation. Provider processes and transcripts belong to bootty-agents.
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use bootty_agents::{AgentKind, NativeSessionRecord, NativeSessionSnapshot, NativeSessionStatus};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    collapsible::Collapsible,
    input::{InputEvent, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    text::{TextView, TextViewState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ListAlignment,
    ListState, ParentElement, Render, Styled, Subscription, Window, div, list, prelude::*,
};
use serde_json::Value;

#[derive(Clone)]
pub enum OpenNativeSession {
    Record(Box<NativeSessionRecord>),
    Existing(String),
    AccountTerminal(bootty_control::CommandTarget),
}

pub struct NativeAgentSessionView {
    record: NativeSessionRecord,
    sender: BoundAppCommandSender,
    composer: Entity<TextareaState>,
    transcript: BTreeMap<String, Entity<TextViewState>>,
    tool_disclosures: BTreeMap<String, bool>,
    list: ListState,
    history: Vec<(String, String)>,
    show_history: bool,
    show_account: bool,
    account: Option<Value>,
    login: Option<Value>,
    answers: BTreeMap<(String, String), Entity<TextareaState>>,
    pending: Option<String>,
    error: Option<String>,
    queue: Vec<(u64, String)>,
    next_queued_id: u64,
    sending_queued: Option<u64>,
    _subscriptions: Vec<Subscription>,
}

impl NativeAgentSessionView {
    pub(crate) fn new(
        record: NativeSessionRecord,
        sender: BoundAppCommandSender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder("Ask your agent…")
        });
        let subscription = cx.subscribe_in(&composer, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                this.send_prompt(window, cx);
            }
            cx.notify();
        });
        let mut this = Self {
            record,
            sender,
            composer,
            transcript: BTreeMap::new(),
            tool_disclosures: BTreeMap::new(),
            // Overdraw represents the rendered viewport boundary, not product spacing.
            list: ListState::new(0, ListAlignment::Bottom, gpui_kit::px(256.)),
            history: Vec::new(),
            show_history: false,
            show_account: false,
            account: None,
            login: None,
            answers: BTreeMap::new(),
            pending: None,
            error: None,
            queue: Vec::new(),
            next_queued_id: 0,
            sending_queued: None,
            _subscriptions: vec![subscription],
        };
        this.sync_transcript(cx);
        this.sync_answers(window, cx);
        this.sync_tool_disclosures();
        this
    }

    pub(crate) fn update_record(
        &mut self,
        record: NativeSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.record.id != record.id {
            return;
        }
        if self.record.snapshot.revision == record.snapshot.revision
            && self.record.generation == record.generation
            && self.record.snapshot.status == record.snapshot.status
            && self.record.title == record.title
        {
            return;
        }
        let transcript_changed = self.record.snapshot.transcript != record.snapshot.transcript;
        self.record = record;
        if transcript_changed {
            self.sync_transcript(cx);
        }
        self.sync_answers(window, cx);
        self.sync_tool_disclosures();
        self.send_queued(window, cx);
        cx.notify();
    }

    pub(crate) fn set_history(&mut self, records: &[NativeSessionRecord], cx: &mut Context<Self>) {
        let history = records
            .iter()
            .filter(|record| record.binding_id == self.record.binding_id)
            .map(|record| (record.id.clone(), record.title.clone()))
            .collect::<Vec<_>>();
        if self.history != history {
            self.history = history;
            cx.notify();
        }
    }

    fn sync_transcript(&mut self, cx: &mut Context<Self>) {
        let follow = self.list.is_scrolled_to_end().unwrap_or(true);
        let mut first_change = None;
        self.transcript.retain(|id, _| {
            self.record
                .snapshot
                .transcript
                .iter()
                .any(|item| &item.id == id)
        });
        for (ix, item) in self.record.snapshot.transcript.iter().enumerate() {
            if let Some(state) = self.transcript.get(&item.id) {
                state.update(cx, |state, cx| state.set_text(&item.text, cx));
            } else {
                self.transcript.insert(
                    item.id.clone(),
                    cx.new(|cx| TextViewState::markdown(&item.text, cx)),
                );
            }
            first_change.get_or_insert(ix);
        }
        let count = self.record.snapshot.transcript.len();
        let old_count = self.list.item_count();
        if count > old_count {
            self.list
                .splice(old_count..old_count, count.saturating_sub(old_count));
        } else if count < old_count {
            self.list.splice(count..old_count, 0);
        } else if first_change.is_some() {
            self.list.remeasure();
        }
        if follow {
            self.list.scroll_to_end();
        }
    }

    fn sync_tool_disclosures(&mut self) {
        self.tool_disclosures.retain(|id, _| {
            self.record
                .snapshot
                .transcript
                .iter()
                .any(|item| &item.id == id && item.complete && is_tool_output(&item.role))
        });
        if is_busy(self.record.snapshot.status) {
            return;
        }
        let follow = self.list.is_scrolled_to_end().unwrap_or(true);
        let mut changed = false;
        for item in &self.record.snapshot.transcript {
            if item.complete
                && is_tool_output(&item.role)
                && let std::collections::btree_map::Entry::Vacant(entry) =
                    self.tool_disclosures.entry(item.id.clone())
            {
                entry.insert(false);
                changed = true;
            }
        }
        if changed {
            self.list.remeasure();
            if follow {
                self.list.scroll_to_end();
            }
        }
    }

    fn sync_answers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.answers.retain(|(id, _), _| {
            self.record
                .snapshot
                .requests
                .iter()
                .any(|request| &request.id == id)
        });
        for request in &self.record.snapshot.requests {
            if is_approval(&request.method) {
                continue;
            }
            let mut questions = field(&request.parameters, "questions")
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|question| field(question, "id").as_str())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if questions.is_empty() {
                questions.push(String::new());
            }
            for question in questions {
                self.answers
                    .entry((request.id.clone(), question))
                    .or_insert_with(|| {
                        cx.new(|cx| {
                            TextareaState::new(window, cx)
                                .auto_grow(2, 5)
                                .placeholder("Your answer…")
                        })
                    });
            }
        }
    }

    fn command(
        &mut self,
        operation: &str,
        arguments: Vec<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending.is_some() {
            return;
        }
        let command = format!("{}.{}", self.record.config.provider.module(), operation);
        let mut invocation = CommandInvocation::from_action(&command, Caller::Internal);
        invocation.target = Some(self.record.target());
        invocation.arguments = arguments;
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(format!("Could not send agent command: {error:?}"));
                cx.notify();
                return;
            }
        };
        self.pending = Some(operation.to_owned());
        self.error = None;
        let operation = operation.to_owned();
        let generation = self.record.generation;
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                if this.record.generation != generation {
                    return;
                }
                this.pending = None;
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        this.accept_result(&operation, value, window, cx);
                    }
                    Ok(outcome) => {
                        this.error = Some(
                            crate::commands::command_outcome_message(&outcome)
                                .unwrap_or_else(|| "Agent command failed".to_owned()),
                        );
                    }
                    Err(error) => {
                        this.error = Some(format!("Agent command did not complete: {error}"));
                    }
                }
                if this.error.is_some() {
                    this.sending_queued = None;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn accept_result(
        &mut self,
        operation: &str,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match operation {
            "prompt" => {
                if let Some(id) = self.sending_queued.take() {
                    self.queue.retain(|(queued_id, _)| *queued_id != id);
                } else {
                    self.composer
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            "resume" | "fork" => {
                match serde_json::from_value::<NativeSessionRecord>(value.clone()) {
                    Ok(record) => {
                        cx.emit(OpenNativeSession::Record(Box::new(record)));
                        return;
                    }
                    Err(error) => {
                        self.error = Some(format!("Could not open agent session: {error}"));
                    }
                }
            }
            "account.status" => self.account = Some(value.clone()),
            "account.login" => self.login = Some(value.clone()),
            "account.logout" => {
                self.account = None;
                self.login = value.get("terminal_target").map(|_| value.clone());
            }
            _ => {}
        }
        if let Ok(snapshot) = serde_json::from_value::<NativeSessionSnapshot>(value) {
            let transcript_changed = self.record.snapshot.transcript != snapshot.transcript;
            self.record.snapshot = snapshot;
            if transcript_changed {
                self.sync_transcript(cx);
            }
            self.sync_answers(window, cx);
            self.sync_tool_disclosures();
        }
    }

    fn send_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.composer.read(cx).value().to_string();
        if message.trim().is_empty() || self.pending.is_some() {
            return;
        }
        if is_busy(self.record.snapshot.status) {
            if self.queue.len() >= 32 {
                self.error = Some(
                    "The follow-up queue is full. Remove or send a queued message.".to_owned(),
                );
                cx.notify();
                return;
            }
            self.queue.push((self.next_queued_id, message));
            self.next_queued_id = self.next_queued_id.wrapping_add(1);
            // The queued draft remains view-owned until the shared command path accepts it.
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
            cx.notify();
        } else {
            self.command("prompt", vec![message], window, cx);
        }
    }

    fn send_queued(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.record.snapshot.status == NativeSessionStatus::Idle
            && self.pending.is_none()
            && self.error.is_none()
            && let Some((id, message)) = self.queue.first()
        {
            let id = *id;
            let message = message.clone();
            self.command("prompt", vec![message], window, cx);
            if self.pending.is_some() {
                self.sending_queued = Some(id);
            }
        }
    }

    pub(crate) fn open_history(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.show_history = true;
        self.command("history", vec![], window, cx);
        cx.notify();
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let busy = self.pending.is_some();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(crate::gpui::icon(
                provider_icon(self.record.config.provider),
                16.0,
                cx.theme().foreground,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .child(self.record.title.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(status_name(self.record.snapshot.status)),
            )
            .child(
                Button::new("native-history")
                    .label("History")
                    .small()
                    .ghost()
                    .selected(self.show_history)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_history = !this.show_history;
                        if this.show_history {
                            this.command("history", vec![], window, cx);
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("native-account")
                    .label("Account")
                    .small()
                    .ghost()
                    .selected(self.show_account)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_account = !this.show_account;
                        if this.show_account {
                            this.command("account.status", vec![], window, cx);
                        }
                        cx.notify();
                    })),
            )
            .when(
                self.record.snapshot.status == NativeSessionStatus::Stopped,
                |row| {
                    row.child(
                        Button::new("native-resume")
                            .label("Resume")
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.command("resume", vec![], window, cx);
                            })),
                    )
                },
            )
            .child(
                Button::new("native-fork")
                    .label("Fork")
                    .tooltip("Start another session from this conversation")
                    .small()
                    .ghost()
                    .disabled(busy || self.record.snapshot.session_id.is_none())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.command("fork", vec![], window, cx)),
                    ),
            )
    }

    fn render_message(
        &self,
        ix: usize,
        owner: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> gpui_kit::AnyElement {
        let Some(item) = self.record.snapshot.transcript.get(ix) else {
            return div().into_any_element();
        };
        if item.complete && is_tool_output(&item.role) {
            let open = self.tool_disclosures.get(&item.id).copied().unwrap_or(true);
            let id = item.id.clone();
            let lines = item.text.lines().count().max(1);
            return div()
                .id(gpui_kit::SharedString::from(format!(
                    "message:{}:{id}",
                    self.record.id
                )))
                .px_4()
                .py_2()
                .min_w_0()
                .child(
                    Collapsible::new()
                        .open(open)
                        .child(
                            Button::new(gpui_kit::SharedString::from(format!("tool-output:{id}")))
                                .label(format!(
                                    "Tool output · {lines} {}",
                                    if lines == 1 { "line" } else { "lines" }
                                ))
                                .icon(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .small()
                                .ghost()
                                .accessibility_label(if open {
                                    "Hide tool output"
                                } else {
                                    "Show tool output"
                                })
                                .on_click(move |_, _, cx| {
                                    _ = owner.update(cx, |this, cx| {
                                        this.tool_disclosures.insert(id.clone(), !open);
                                        this.list.remeasure();
                                        cx.notify();
                                    });
                                }),
                        )
                        .when_some(self.transcript.get(&item.id), |body, state| {
                            body.content(TextView::new(state).selectable(true))
                        }),
                )
                .into_any_element();
        }
        let title = match item.role.as_str() {
            "user" => "You",
            "assistant" => provider_name(self.record.config.provider),
            "thinking" => "Thinking",
            "tool" | "toolResult" => "Tool",
            "change" => "Changes",
            _ => "Agent",
        };
        div()
            .id(gpui_kit::SharedString::from(format!(
                "message:{}:{}",
                self.record.id, item.id
            )))
            .px_4()
            .py_3()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(title)
                    .when(!item.complete, |row| row.child("In progress")),
            )
            .when_some(self.transcript.get(&item.id), |body, state| {
                body.child(TextView::new(state).selectable(true))
            })
            .into_any_element()
    }
}

impl EventEmitter<OpenNativeSession> for NativeAgentSessionView {}
impl Focusable for NativeAgentSessionView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl Render for NativeAgentSessionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        div()
            .id(gpui_kit::SharedString::from(self.record.id.clone()))
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_toolbar(cx))
            .when(self.show_account, |body| {
                body.child(self.render_account(cx))
            })
            .when(self.show_history, |body| {
                body.child(self.render_history(cx))
            })
            .when_some(
                self.error.as_ref().or(self.record.snapshot.error.as_ref()),
                |body, error| {
                    body.child(
                        div()
                            .px_4()
                            .py_2()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error.clone()),
                    )
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when(self.record.snapshot.transcript.is_empty(), |body| {
                        body.child(
                            div()
                                .p_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    if self.record.snapshot.status == NativeSessionStatus::Starting
                                    {
                                        "Connecting to agent…"
                                    } else {
                                        "Start a conversation about this project."
                                    },
                                ),
                        )
                    })
                    .when(!self.record.snapshot.transcript.is_empty(), |body| {
                        body.child(
                            list(self.list.clone(), move |ix, _, cx| {
                                owner.upgrade().map_or_else(
                                    || div().into_any_element(),
                                    |owner| {
                                        owner.read(cx).render_message(ix, owner.downgrade(), cx)
                                    },
                                )
                            })
                            .size_full(),
                        )
                    }),
            )
            .child(self.render_requests(cx))
            .child(self.render_composer(cx))
    }
}

fn is_tool_output(role: &str) -> bool {
    matches!(role, "tool" | "toolResult")
}

const fn provider_icon(provider: AgentKind) -> &'static str {
    match provider {
        AgentKind::Codex => "openai",
        AgentKind::Claude => "claude",
        AgentKind::Pi => "terminal",
    }
}

const fn provider_name(provider: AgentKind) -> &'static str {
    match provider {
        AgentKind::Codex => "Codex",
        AgentKind::Claude => "Claude",
        AgentKind::Pi => "Pi",
    }
}
const fn is_busy(status: NativeSessionStatus) -> bool {
    matches!(
        status,
        NativeSessionStatus::Working | NativeSessionStatus::Waiting | NativeSessionStatus::Starting
    )
}
const fn status_name(status: NativeSessionStatus) -> &'static str {
    match status {
        NativeSessionStatus::Starting => "Connecting",
        NativeSessionStatus::Idle => "Ready",
        NativeSessionStatus::Working => "Working",
        NativeSessionStatus::Waiting => "Needs input",
        NativeSessionStatus::Stopped => "Stopped",
        NativeSessionStatus::Error => "Failed",
    }
}
fn is_approval(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "can_use_tool"
            | "confirm"
    )
}

impl NativeAgentSessionView {
    fn render_composer(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let pending = self.pending.is_some();
        let stopped = self.record.snapshot.status == NativeSessionStatus::Stopped;
        let busy = is_busy(self.record.snapshot.status);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border);
        for (id, message) in &self.queue {
            let id = *id;
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(
                        Icon::new(IconName::Inbox)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(message.clone()))
                    .child(
                        Button::new(gpui_kit::SharedString::from(format!("queue-remove-{id}")))
                            .icon(IconName::Close)
                            .small()
                            .ghost()
                            .accessibility_label("Remove queued follow-up")
                            .tooltip("Remove queued follow-up")
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.queue.retain(|(queued_id, _)| *queued_id != id);
                                cx.notify();
                            })),
                    ),
            );
        }
        body.child(
            Textarea::new(&self.composer)
                .aria_label("Message to agent")
                .disabled(stopped || pending),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if stopped {
                            "Resume this session to continue"
                        } else if busy {
                            "Follow-ups send when this turn finishes"
                        } else {
                            "Enter to send · Shift+Enter for a new line"
                        }),
                )
                .when(
                    busy && self.record.snapshot.status != NativeSessionStatus::Starting,
                    |row| {
                        row.child(
                            Button::new("native-interrupt")
                                .label("Interrupt")
                                .icon(IconName::Pause)
                                .small()
                                .ghost()
                                .disabled(pending)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.command("interrupt", vec![], window, cx);
                                })),
                        )
                    },
                )
                .child(
                    Button::new("native-send")
                        .label(if busy { "Queue" } else { "Send" })
                        .icon(IconName::ArrowUp)
                        .small()
                        .primary()
                        .loading(self.pending.as_deref() == Some("prompt"))
                        .disabled(
                            stopped || pending || self.composer.read(cx).value().trim().is_empty(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.send_prompt(window, cx))),
                ),
        )
    }

    fn render_requests(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let mut body = div().flex().flex_col().max_h_64().min_h_0();
        for request in &self.record.snapshot.requests {
            let id = request.id.clone();
            let mut row = div()
                .id(gpui_kit::SharedString::from(format!("request:{id}")))
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(cx.theme().border)
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().child(request_title(&request.method)))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(request_description(&request.parameters)),
                );
            if is_approval(&request.method) {
                let decline = id.clone();
                row = row.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new(gpui_kit::SharedString::from(format!("decline:{id}")))
                                .label("Decline")
                                .small()
                                .disabled(self.pending.is_some())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.command(
                                        "approve",
                                        vec![decline.clone(), "false".to_owned()],
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new(gpui_kit::SharedString::from(format!("allow:{id}")))
                                .label("Allow once")
                                .small()
                                .primary()
                                .disabled(self.pending.is_some())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.command(
                                        "approve",
                                        vec![id.clone(), "true".to_owned()],
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                );
            } else {
                for ((request_id, question_id), answer) in &self.answers {
                    if request_id != &id {
                        continue;
                    }
                    let label = field(&request.parameters, "questions")
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|question| {
                            field(question, "id").as_str() == Some(question_id.as_str())
                        })
                        .and_then(|question| field(question, "question").as_str())
                        .unwrap_or("Answer");
                    row = row.child(div().text_sm().child(label.to_owned())).child(
                        Textarea::new(answer)
                            .aria_label(label.to_owned())
                            .disabled(self.pending.is_some()),
                    );
                }
                row = row.child(Button::new(gpui_kit::SharedString::from(format!("answer:{id}"))).label("Send answer").small().disabled(self.pending.is_some()).on_click(cx.listener(move |this, _, window, cx| {
                    let response = if this.record.config.provider == AgentKind::Codex {
                        let answers = this.answers.iter().filter(|((request_id, _), _)| request_id == &id).map(|((_, question_id), answer)| (question_id.clone(), serde_json::json!({"answers":[answer.read(cx).value().to_string()]}))).collect::<serde_json::Map<_, _>>();
                        serde_json::json!({"answers":answers})
                    } else { Value::String(this.answers.get(&(id.clone(),String::new())).map_or_else(String::new, |answer| answer.read(cx).value().to_string())) };
                    this.command("respond", vec![id.clone(), response.to_string()], window, cx);
                })));
            }

            body = body.child(row);
        }
        body.overflow_y_scrollbar().into_any_element()
    }

    fn render_history(&self, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let owner = cx.weak_entity();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .max_h_48()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().child("Conversations"))
                    .child(
                        Button::new("close-native-history")
                            .icon(IconName::Close)
                            .small()
                            .ghost()
                            .accessibility_label("Close session history")
                            .tooltip("Close session history")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_history = false;
                                cx.notify();
                            })),
                    ),
            );
        if self.history.is_empty() {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No conversations yet."),
            );
        }
        for (id, title) in &self.history {
            let id = id.clone();
            let owner = owner.clone();
            body = body.child(
                Button::new(gpui_kit::SharedString::from(format!("history:{id}")))
                    .label(title.clone())
                    .ghost()
                    .small()
                    .selected(id == self.record.id)
                    .on_click(move |_, _, cx| {
                        _ = owner
                            .update(cx, |_, cx| cx.emit(OpenNativeSession::Existing(id.clone())));
                    }),
            );
        }
        body.overflow_y_scrollbar().into_any_element()
    }

    fn render_login(login: &Value, cx: &Context<Self>) -> gpui_kit::Div {
        let url = field(login, "verificationUrl")
            .as_str()
            .or_else(|| field(login, "authUrl").as_str())
            .or_else(|| field(login, "verification_url").as_str())
            .map(str::to_owned);
        let terminal = login.get("terminal_target").cloned().and_then(|target| {
            serde_json::from_value::<bootty_control::CommandTarget>(target).ok()
        });
        let code = field(login, "userCode")
            .as_str()
            .or_else(|| field(login, "user_code").as_str())
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().child(if code.is_empty() {
                field(login, "message")
                    .as_str()
                    .unwrap_or("Complete sign-in with your provider.")
                    .to_owned()
            } else {
                format!("Sign-in code: {code}")
            }))
            .when_some(terminal, |body, target| {
                body.child(
                    Button::new("native-account-terminal")
                        .label("Open account terminal")
                        .small()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(OpenNativeSession::AccountTerminal(target.clone()));
                        })),
                )
            })
            .when_some(url, |body, url| {
                body.child(
                    Button::new("native-account-browser")
                        .label("Open sign-in")
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let invocation = CommandInvocation::new(
                                "link.open",
                                vec![url.clone()],
                                Caller::Internal,
                            );
                            if let Err(error) = this.sender.submit(
                                invocation,
                                Instant::now()
                                    .checked_add(Duration::from_secs(30))
                                    .unwrap_or_else(Instant::now),
                                CommandCancellation::new(),
                            ) {
                                this.error = Some(format!("Could not open sign-in: {error:?}"));
                                cx.notify();
                            }
                        })),
                )
            })
    }

    fn render_account(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let account = self
            .account
            .as_ref()
            .or(self.record.snapshot.account.as_ref());
        let description =
            account.map_or_else(|| "Checking account…".to_owned(), account_description);
        let providers = account.and_then(|account| field(account, "providers").as_array());
        let provider_buttons = providers.into_iter().flatten().filter_map(|provider| {
            let id = field(provider, "id").as_str()?.to_owned();
            let name = field(provider, "name").as_str().unwrap_or(&id).to_owned();
            Some(
                Button::new(gpui_kit::SharedString::from(format!("login-provider:{id}")))
                    .label(format!("Sign in to {name}…"))
                    .small()
                    .disabled(self.pending.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.command("account.login", vec![id.clone()], window, cx);
                    })),
            )
        });

        div()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().text_sm().child(format!(
                "{} account",
                provider_name(self.record.config.provider)
            )))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
            .when_some(self.login.as_ref(), |body, login| {
                body.child(Self::render_login(login, cx))
            })
            .children(provider_buttons)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(self.record.config.provider != AgentKind::Pi, |row| {
                        row.child(
                            Button::new("native-account-connect")
                                .label("Sign in…")
                                .small()
                                .disabled(self.pending.is_some())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.command("account.login", vec![], window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("native-account-refresh")
                            .label("Refresh")
                            .small()
                            .ghost()
                            .disabled(self.pending.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.command("account.status", vec![], window, cx);
                            })),
                    )
                    .child(
                        Button::new("native-account-logout")
                            .label("Sign out")
                            .small()
                            .ghost()
                            .disabled(self.pending.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.command("account.logout", vec![], window, cx);
                            })),
                    )
                    .child(
                        Button::new("native-account-close")
                            .label("Done")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_account = false;
                                cx.notify();
                            })),
                    ),
            )
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn request_title(method: &str) -> &'static str {
    match method {
        "item/commandExecution/requestApproval" => "Allow this command?",
        "item/fileChange/requestApproval" => "Allow these file changes?",
        "can_use_tool" => "Allow this tool?",
        "confirm" => "Agent asks for confirmation",
        _ => "Agent needs your answer",
    }
}
fn request_description(parameters: &Value) -> String {
    // Show the full permission scope before the user authorizes it.
    serde_json::to_string_pretty(parameters).unwrap_or_else(|_| parameters.to_string())
}
fn account_description(account: &Value) -> String {
    if let Some(email) = field(field(account, "account"), "email")
        .as_str()
        .or_else(|| field(account, "email").as_str())
    {
        return email.to_owned();
    }
    if field(account, "loggedIn") == true
        || field(account, "logged_in") == true
        || field(account, "account").is_object()
    {
        return "Signed in".to_owned();
    }
    if field(account, "authMethod") == "api_key"
        || field(field(account, "account"), "type") == "apiKey"
    {
        return "Using an API key".to_owned();
    }
    if let Some(providers) = account
        .as_array()
        .or_else(|| field(account, "providers").as_array())
    {
        return format!("{} available providers", providers.len());
    }
    "Not signed in".to_owned()
}

impl EventEmitter<gpui_kit::component::dock::PanelEvent> for NativeAgentSessionView {}
impl gpui_kit::component::dock::BasePanel for NativeAgentSessionView {
    fn panel_name(&self) -> &'static str {
        "bootty.native-session"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn zoomable(&self, _: &App) -> bool {
        false
    }
}
impl gpui_kit::component::dock::Panel for NativeAgentSessionView {
    fn tab_name(&self, _: &App) -> Option<gpui_kit::SharedString> {
        Some(self.record.title.clone().into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.record.title.clone()
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
