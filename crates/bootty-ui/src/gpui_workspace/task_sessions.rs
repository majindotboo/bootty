//! Saved task destinations, presented through the shared command mailbox.

use std::time::{Duration, Instant};

use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    tab::{Tab, TabBar},
};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::*, px,
};
use serde::Deserialize;

use super::GpuiWorkspace;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Destination {
    Active,
    Settled,
    Archived,
}

impl Destination {
    const ALL: [Self; 3] = [Self::Active, Self::Settled, Self::Archived];

    const fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Settled => "settled",
            Self::Archived => "archived",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Settled => "Settled",
            Self::Archived => "Archived",
        }
    }
}

#[derive(Deserialize)]
struct SavedSession {
    identity: String,
    title: String,
    cwd: String,
    lifecycle: Option<Destination>,
    attached_observed: bool,
}

#[derive(Clone, Copy)]
enum Request {
    List,
    Set(Destination),
}

struct SavedTasks {
    target: CommandTarget,
    sender: BoundAppCommandSender,
    query: Entity<InputState>,
    sessions: Vec<SavedSession>,
    selected: Option<String>,
    destination: Destination,
    pending: bool,
    error: Option<String>,
}

impl SavedTasks {
    fn new(
        target: CommandTarget,
        sender: BoundAppCommandSender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search saved tasks"));
        cx.subscribe(&query, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.selected = None;
                cx.notify();
            }
        })
        .detach();
        let mut view = Self {
            target,
            sender,
            query,
            sessions: Vec::new(),
            selected: None,
            destination: Destination::Active,
            pending: false,
            error: None,
        };
        view.request(Request::List, window, cx);
        view
    }

    fn invocation(&self, request: Request) -> Option<CommandInvocation> {
        let (command, arguments) = match request {
            Request::List => ("session.tasks", Vec::new()),
            Request::Set(destination) => {
                let identity = self.selected.as_ref()?;
                self.sessions
                    .iter()
                    .find(|session| &session.identity == identity)?;
                (
                    "session.task.set",
                    vec![identity.clone(), destination.name().to_owned()],
                )
            }
        };
        let mut invocation = CommandInvocation::new(command, arguments, Caller::CommandPalette);
        invocation.target = Some(self.target.clone());
        Some(invocation)
    }

    fn request(&mut self, request: Request, window: &Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(invocation) = self.invocation(request) else {
            return;
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(30))
            .unwrap_or_else(Instant::now);
        let response = self
            .sender
            .submit(invocation, deadline, CommandCancellation::new());
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
                                .map_err(|_| {
                                    "The saved task response was interrupted. Refresh to try again."
                                        .to_owned()
                                })
                        })
                })
                .await;
            _ = view.update_in(cx, |this, window, cx| {
                this.pending = false;
                match outcome {
                    Ok(CommandOutcome::Success { value, .. }) => match request {
                        Request::List => this.accept_sessions(value),
                        Request::Set(destination) => {
                            this.destination = destination;
                            this.request(Request::List, window, cx);
                        }
                    },
                    Ok(outcome) => this.error = crate::commands::command_outcome_message(&outcome),
                    Err(message) => this.error = Some(message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn accept_sessions(&mut self, value: serde_json::Value) {
        match serde_json::from_value::<Vec<SavedSession>>(value) {
            Ok(mut sessions) => {
                sessions.retain(|session| session.lifecycle.is_some());
                if self.selected.as_ref().is_some_and(|identity| {
                    !sessions.iter().any(|session| &session.identity == identity)
                }) {
                    self.selected = None;
                }
                self.sessions = sessions;
            }
            Err(_) => {
                self.error =
                    Some("Saved tasks could not be read. Refresh to try again.".to_owned());
            }
        }
    }

    fn tabs(&self, cx: &Context<Self>) -> TabBar {
        TabBar::new("saved-task-destinations")
            .selected_index(
                Destination::ALL
                    .iter()
                    .position(|destination| *destination == self.destination)
                    .unwrap_or_default(),
            )
            .children(
                Destination::ALL.map(|destination| {
                    Tab::new().label(destination.label()).disabled(self.pending)
                }),
            )
            .on_click(cx.listener(|this, index, _, cx| {
                if let Some(destination) = Destination::ALL.get(*index) {
                    this.destination = *destination;
                    this.selected = None;
                    cx.notify();
                }
            }))
    }

    fn row(&self, session: &SavedSession, cx: &Context<Self>) -> Button {
        let identity = session.identity.clone();
        let attachment = if session.attached_observed {
            "Attachment observed"
        } else {
            "No attachment observed"
        };
        Button::new(SharedString::from(format!("saved-task-{identity}")))
            .ghost()
            .w_full()
            .h_auto()
            .justify_start()
            .selected(self.selected.as_ref() == Some(&identity))
            .disabled(self.pending)
            .accessibility_label(format!("{}, {attachment}", session.title))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .w_full()
                    .min_w_0()
                    .py_1()
                    .child(div().font_medium().truncate().child(session.title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(session.cwd.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(attachment),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(identity.clone());
                cx.notify();
            }))
    }

    fn task_list(&self, cx: &Context<Self>) -> impl IntoElement {
        let query = self.query.read(cx).value().to_lowercase();
        let sessions: Vec<_> = self
            .sessions
            .iter()
            .filter(|session| {
                session.lifecycle == Some(self.destination)
                    && (session.title.to_lowercase().contains(&query)
                        || session.cwd.to_lowercase().contains(&query))
            })
            .collect();
        let empty = if self.pending {
            "Loading saved tasks…".to_owned()
        } else {
            format!("No {} tasks match this view.", self.destination.name())
        };
        div()
            .id("saved-task-list")
            .flex()
            .flex_col()
            .gap_1()
            .h_64()
            .min_h_0()
            .when(sessions.is_empty(), |list| {
                list.child(
                    div()
                        .py_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(empty),
                )
            })
            .children(sessions.into_iter().map(|session| self.row(session, cx)))
            .overflow_y_scrollbar()
    }

    fn actions(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                Button::new("saved-tasks-refresh")
                    .outline()
                    .label("Refresh")
                    .disabled(self.pending)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.request(Request::List, window, cx)),
                    ),
            )
            .child(div().flex_1())
            .children(
                [
                    (Destination::Settled, "Settle"),
                    (Destination::Archived, "Archive"),
                    (Destination::Active, "Restore"),
                ]
                .map(|(destination, label)| {
                    Button::new(SharedString::from(format!(
                        "saved-task-{}",
                        destination.name()
                    )))
                    .outline()
                    .label(label)
                    .disabled(
                        self.pending || self.selected.is_none() || self.destination == destination,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.request(Request::Set(destination), window, cx);
                    }))
                }),
            )
    }
}

impl Focusable for SavedTasks {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Render for SavedTasks {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_col().gap_3()
            .child(self.tabs(cx))
            .child(Input::new(&self.query).aria_label("Search saved tasks").disabled(self.pending))
            .child(self.task_list(cx))
            .when_some(self.error.clone(), |body, error| body.child(div().text_sm().text_color(cx.theme().danger).child(error)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground)
                .child("Destinations are saved choices. Settle, Archive and Restore leave terminals unchanged. Attachment labels reflect the last backend snapshot."))
            .child(self.actions(cx))
    }
}

impl GpuiWorkspace {
    pub(super) fn open_saved_tasks(
        &mut self,
        title: String,
        target: CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.close_overlay_dialogs();
        self.dialogs.clear_presentation(window, cx);
        let sender = self.state.app_command_sender(Caller::CommandPalette);
        let view = cx.new(|cx| SavedTasks::new(target, sender, window, cx));
        let content = view.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let content = content.clone();
            dialog
                .title(format!("Saved tasks · {title}"))
                // Dialog width is a resolved native-window boundary, scaled with UI text.
                .w(px(f32::from(window.rem_size()) * 36.0))
                .content(move |body, _, _| body.child(content.clone()))
        });
        cx.defer_in(window, move |_, window, cx| {
            crate::window::restore_keyboard_focus(window);
            view.focus_handle(cx).focus(window, cx);
        });
    }
}
