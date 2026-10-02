//! Native provider history and account actions, using the shared command mailbox.
use bootty_agents::{AgentKind, TerminalSessionHistory};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::*,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum ReadKind {
    History,
    Account,
}

pub struct AgentHistory {
    provider: AgentKind,
    cwd: String,
    target: CommandTarget,
    sender: BoundAppCommandSender,
    query: Entity<InputState>,
    sessions: Vec<TerminalSessionHistory>,
    selected: Option<String>,
    loading: bool,
    pending: bool,
    error: Option<String>,
    account: String,
}

impl AgentHistory {
    pub fn new(
        provider: AgentKind,
        cwd: String,
        target: CommandTarget,
        sender: BoundAppCommandSender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search session history"));
        cx.subscribe(&query, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.selected = None;
                cx.notify();
            }
        })
        .detach();
        let view = Self {
            provider,
            cwd,
            target,
            sender,
            query,
            sessions: Vec::new(),
            selected: None,
            loading: true,
            pending: false,
            error: None,
            account: if provider == AgentKind::Pi {
                "Use /login in the Pi terminal to manage accounts.".into()
            } else {
                "Checking account…".into()
            },
        };
        view.read(ReadKind::History, window, cx);
        if provider != AgentKind::Pi {
            view.read(ReadKind::Account, window, cx);
        }
        view
    }

    fn invocation(&self, operation: &str, arguments: Vec<String>) -> CommandInvocation {
        let mut request = CommandInvocation::new(
            format!("agents.{}.{operation}", self.provider),
            arguments,
            Caller::CommandPalette,
        );
        request.target = Some(self.target.clone());
        request
    }

    fn read(&self, kind: ReadKind, window: &Window, cx: &Context<Self>) {
        let (operation, arguments) = match kind {
            ReadKind::History => ("sessions", vec![self.cwd.clone()]),
            ReadKind::Account => ("account.status", Vec::new()),
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .unwrap_or_else(Instant::now);
        let response = self.sender.submit(
            self.invocation(operation, arguments),
            deadline,
            CommandCancellation::new(),
        );
        cx.spawn_in(window, async move |view, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    response
                        .map_err(|_| "The command owner is unavailable.".to_owned())
                        .and_then(|receiver| {
                            receiver
                                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                                .map_err(|_| "The command response was interrupted.".to_owned())
                        })
                })
                .await;
            _ = view.update_in(cx, |this, _, cx| {
                match kind {
                    ReadKind::History => {
                        this.loading = false;
                        match outcome {
                            Ok(CommandOutcome::Success { value, .. }) => {
                                match serde_json::from_value(
                                    value.get("sessions").cloned().unwrap_or_default(),
                                ) {
                                    Ok(sessions) => this.sessions = sessions,
                                    Err(_) => {
                                        this.error =
                                            Some("Session history could not be read.".into());
                                    }
                                }
                            }
                            Ok(outcome) => {
                                this.error = crate::commands::command_outcome_message(&outcome);
                            }
                            Err(message) => this.error = Some(message),
                        }
                    }
                    ReadKind::Account => {
                        this.account = match outcome {
                            Ok(CommandOutcome::Success { value, .. })
                                if value.get("loggedIn").and_then(serde_json::Value::as_bool)
                                    == Some(true) =>
                            {
                                "Signed in".into()
                            }
                            Ok(CommandOutcome::Success { .. }) => "Not signed in".into(),
                            _ => "Account status unavailable".into(),
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open(&mut self, operation: &str, window: &Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let arguments = match operation {
            "resume" | "fork" => {
                let Some(id) = self.selected.clone() else {
                    return;
                };
                if !self.sessions.iter().any(|session| session.id == id) {
                    return;
                }
                vec![id, self.cwd.clone()]
            }
            "start" => vec![self.cwd.clone()],
            _ => Vec::new(),
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .unwrap_or_else(Instant::now);
        let response = self.sender.submit(
            self.invocation(operation, arguments),
            deadline,
            CommandCancellation::new(),
        );
        self.pending = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    response
                        .map_err(|_| "The command owner is unavailable.".to_owned())
                        .and_then(|receiver| {
                            receiver
                                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                                .map_err(|_| "The command response was interrupted.".to_owned())
                        })
                })
                .await;
            _ = view.update_in(cx, |this, window, cx| {
                this.pending = false;
                match outcome {
                    Ok(CommandOutcome::Success { .. }) => window.close_dialog(cx),
                    Ok(outcome) => this.error = crate::commands::command_outcome_message(&outcome),
                    Err(message) => this.error = Some(message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn account_header(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(crate::gpui::icon(
                self.provider.icon(),
                16.0,
                cx.theme().foreground,
            ))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.account.clone()),
            )
            .child(
                Button::new("agent-history-login")
                    .outline()
                    .label("Sign in…")
                    .disabled(self.pending)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open("account.login", window, cx);
                    })),
            )
    }

    fn session_row(&self, session: &TerminalSessionHistory, cx: &Context<Self>) -> Button {
        let id = session.id.clone();
        let selected = self.selected.as_ref() == Some(&id);
        Button::new(SharedString::from(format!("agent-history-{id}")))
            .ghost()
            .w_full()
            .h_auto()
            .flex_shrink_0()
            .justify_start()
            .when(selected, |button| button.bg(cx.theme().secondary_hover))
            .accessibility_label(session.title.clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .w_full()
                    .min_w_0()
                    .py_1()
                    .child(
                        div()
                            .font_medium()
                            .min_w_0()
                            .truncate()
                            .child(session.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(session.cwd.to_string_lossy().into_owned()),
                    )
                    .when_some(
                        i64::try_from(session.updated_at)
                            .ok()
                            .and_then(chrono::DateTime::from_timestamp_millis)
                            .map(|date| {
                                date.with_timezone(&chrono::Local)
                                    .format("%b %e · %H:%M")
                                    .to_string()
                            }),
                        |row, date| {
                            row.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(date),
                            )
                        },
                    ),
            )
            .disabled(self.pending)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(id.clone());
                cx.notify();
            }))
    }
}

impl Focusable for AgentHistory {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Render for AgentHistory {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query.read(cx).value().to_lowercase();
        let visible: Vec<_> = self
            .sessions
            .iter()
            .filter(|session| {
                session.title.to_lowercase().contains(&query) || session.id.contains(&query)
            })
            .collect();
        let empty = if self.loading {
            "Loading session history…"
        } else if self.sessions.is_empty() {
            "No saved sessions for this project. Open a new agent terminal to get started."
        } else {
            "No matching sessions."
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.account_header(cx))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.cwd.clone()),
            )
            .child(Input::new(&self.query).aria_label("Search session history"))
            .child(
                div()
                    .id("agent-history-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .h(gpui_kit::rems(if visible.is_empty() { 4.0 } else { 20.0 }))
                    .when(visible.is_empty(), |list| {
                        list.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .py_3()
                                .child(empty),
                        )
                    })
                    .children(
                        visible
                            .into_iter()
                            .map(|session| self.session_row(session, cx)),
                    )
                    .overflow_y_scrollbar(),
            )
            .when_some(self.error.clone(), |body, error| {
                body.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("agent-history-new")
                            .outline()
                            .label("New terminal")
                            .disabled(self.pending)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open("start", window, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("agent-history-fork")
                            .outline()
                            .label("Fork")
                            .disabled(self.pending || self.selected.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open("fork", window, cx)),
                            ),
                    )
                    .child(
                        Button::new("agent-history-resume")
                            .primary()
                            .label("Resume")
                            .disabled(self.pending || self.selected.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open("resume", window, cx)),
                            ),
                    ),
            )
    }
}
