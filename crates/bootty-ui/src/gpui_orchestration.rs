//! Coordination setup and work status. Every mutation uses the application command mailbox.

use std::time::{Duration, Instant};

use bootty_agents::{AgentKind, OrchestrationRun, OrchestrationTaskState};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
    CommandTarget,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent},
    input::{Input, InputState, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    searchable_list::SearchableListItem,
    select::{Select, SelectEvent, SelectState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::*,
};
use serde_json::Value;

/// A live agent terminal projection supplied by the shell. The backend issues its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrchestrationAgentSession {
    pub name: String,
    pub provider: AgentKind,
    pub target: CommandTarget,
}

#[derive(Clone)]
struct Choice {
    id: String,
    label: SharedString,
}

impl SearchableListItem for Choice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &String {
        &self.id
    }
}

type Picker = Entity<SelectState<Vec<Choice>>>;

pub struct OrchestrationPanel {
    sender: BoundAppCommandSender,
    runs: Vec<OrchestrationRun>,
    sessions: Vec<OrchestrationAgentSession>,
    run: Picker,
    task: Picker,
    worker: Picker,
    session: Picker,
    run_title: Entity<InputState>,
    run_goal: Entity<TextareaState>,
    task_title: Entity<InputState>,
    task_prompt: Entity<TextareaState>,
    worker_name: Entity<InputState>,
    message: Entity<TextareaState>,
    pending: bool,
    active: bool,
    error: Option<String>,
}

impl OrchestrationPanel {
    pub(crate) fn new(
        sender: BoundAppCommandSender,
        runs: Vec<OrchestrationRun>,
        sessions: Vec<OrchestrationAgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let run = picker(window, cx);
        cx.subscribe_in(&run, window, |this, _, event, window, cx| {
            if matches!(event, SelectEvent::Confirm(_)) {
                this.sync_details(window, cx);
                cx.notify();
            }
        })
        .detach();
        let panel = Self {
            sender,
            runs,
            sessions,
            run,
            task: picker(window, cx),
            worker: picker(window, cx),
            session: picker(window, cx),
            run_title: input("Run title", window, cx),
            run_goal: textarea("What should these agents accomplish?", window, cx),
            task_title: input("Task title", window, cx),
            task_prompt: textarea("Instructions and acceptance criteria", window, cx),
            worker_name: input("Worker name", window, cx),
            message: textarea("Message to the selected worker", window, cx),
            pending: false,
            active: false,
            error: None,
        };
        for input in [&panel.run_title, &panel.task_title, &panel.worker_name] {
            cx.observe(input, |_, _, cx| cx.notify()).detach();
        }
        for input in [&panel.run_goal, &panel.task_prompt, &panel.message] {
            cx.observe(input, |_, _, cx| cx.notify()).detach();
        }
        for picker in [&panel.task, &panel.worker, &panel.session] {
            cx.observe(picker, |_, _, cx| cx.notify()).detach();
        }
        panel.sync_choices(window, cx);
        cx.spawn_in(window, async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if weak
                    .update_in(cx, |this, window, cx| {
                        if this.active && !this.pending && !this.editing(window, cx) {
                            this.refresh(window, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        panel
    }

    pub(crate) fn set_sessions(
        &mut self,
        sessions: Vec<OrchestrationAgentSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if sessions == self.sessions {
            return;
        }
        self.sessions = sessions;
        self.sync_sessions(window, cx);
        cx.notify();
    }

    fn editing(&self, window: &Window, cx: &App) -> bool {
        [&self.run_title, &self.task_title, &self.worker_name]
            .into_iter()
            .any(|input| input.focus_handle(cx).contains_focused(window, cx))
            || [&self.run_goal, &self.task_prompt, &self.message]
                .into_iter()
                .any(|input| input.focus_handle(cx).contains_focused(window, cx))
            || [&self.run, &self.task, &self.worker, &self.session]
                .into_iter()
                .any(|picker| picker.focus_handle(cx).contains_focused(window, cx))
    }

    fn selected_run(&self, cx: &App) -> Option<&OrchestrationRun> {
        let id = self.run.read(cx).selected_value()?;
        self.runs.iter().find(|run| &run.id == id)
    }

    fn sync_choices(&self, window: &mut Window, cx: &mut Context<Self>) {
        update_picker(
            &self.run,
            self.runs
                .iter()
                .map(|run| {
                    choice(
                        &run.id,
                        format!(
                            "{}{}",
                            run.title,
                            if run.finished { " · finished" } else { "" }
                        ),
                    )
                })
                .collect(),
            window,
            cx,
        );
        self.sync_details(window, cx);
        self.sync_sessions(window, cx);
    }

    fn sync_details(&self, window: &mut Window, cx: &mut Context<Self>) {
        let run = self.selected_run(cx);
        let tasks = run
            .into_iter()
            .flat_map(|run| &run.tasks)
            .map(|task| {
                choice(
                    &task.id,
                    format!("{} · {}", task.title, state_label(&task.state)),
                )
            })
            .collect();
        let workers = run
            .into_iter()
            .flat_map(|run| &run.workers)
            .map(|worker| choice(&worker.id, format!("{} · {}", worker.name, worker.provider)))
            .collect();
        update_picker(&self.task, tasks, window, cx);
        update_picker(&self.worker, workers, window, cx);
    }

    fn sync_sessions(&self, window: &mut Window, cx: &mut Context<Self>) {
        update_picker(
            &self.session,
            self.sessions
                .iter()
                .map(|session| {
                    choice(
                        &session.target.handle,
                        format!("{} · {}", session.name, session.provider),
                    )
                })
                .collect(),
            window,
            cx,
        );
    }

    fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.command("orchestration.run.list", Vec::new(), None, window, cx);
    }

    fn command(
        &mut self,
        command: &'static str,
        arguments: Vec<String>,
        target: Option<CommandTarget>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending {
            return;
        }
        let mut invocation = CommandInvocation::new(command, arguments, Caller::Internal);
        invocation.target = target;
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(format!("Coordination command unavailable: {error:?}"));
                cx.notify();
                return;
            }
        };
        self.pending = true;
        cx.notify();
        cx.spawn_in(window, async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok(CommandOutcome::Success { value, warnings }) => {
                        if command != "orchestration.run.list" || !warnings.is_empty() {
                            this.error = warnings.first().map(|warning| warning.message.clone());
                        }
                        if let Err(error) = this.accept(command, &value, window, cx) {
                            this.error = Some(error);
                        }
                    }
                    Ok(outcome) => {
                        this.error = crate::commands::command_outcome_message(&outcome)
                            .or_else(|| Some("Coordination command failed".to_owned()));
                    }
                    Err(error) => {
                        this.error = Some(format!("Coordination response unavailable: {error}"));
                    }
                }
                if command != "orchestration.run.list" {
                    this.refresh(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn accept(
        &mut self,
        command: &str,
        value: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if command == "orchestration.run.list" {
            self.runs = serde_json::from_value(
                value
                    .get("runs")
                    .ok_or("Coordination response has no runs")?
                    .clone(),
            )
            .map_err(|error| error.to_string())?;
            self.sync_choices(window, cx);
        } else if let Some(run) = value.get("run") {
            let run: OrchestrationRun =
                serde_json::from_value(run.clone()).map_err(|error| error.to_string())?;
            let id = run.id.clone();
            if let Some(previous) = self.runs.iter_mut().find(|previous| previous.id == id) {
                *previous = run;
            } else {
                self.runs.push(run);
            }
            self.sync_choices(window, cx);
            self.run
                .update(cx, |state, cx| state.set_selected_value(&id, window, cx));
            self.sync_details(window, cx);
            if command == "orchestration.run.create" {
                self.run_title
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.run_goal
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
        } else if let Some(run_id) = self.run.read(cx).selected_value().cloned() {
            let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) else {
                return Ok(());
            };
            if let Some(task) = value.get("task") {
                let task: bootty_agents::OrchestrationTask =
                    serde_json::from_value(task.clone()).map_err(|error| error.to_string())?;
                let id = task.id.clone();
                if let Some(previous) = run.tasks.iter_mut().find(|previous| previous.id == id) {
                    *previous = task;
                } else {
                    run.tasks.push(task);
                }
                self.sync_details(window, cx);
                self.task
                    .update(cx, |state, cx| state.set_selected_value(&id, window, cx));
                if command == "orchestration.task.create" {
                    self.task_title
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    self.task_prompt
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
            } else if let Some(worker) = value.get("worker") {
                let worker: bootty_agents::OrchestrationWorker =
                    serde_json::from_value(worker.clone()).map_err(|error| error.to_string())?;
                let id = worker.id.clone();
                if let Some(previous) = run.workers.iter_mut().find(|previous| previous.id == id) {
                    *previous = worker;
                } else {
                    run.workers.push(worker);
                }
                self.sync_details(window, cx);
                self.worker
                    .update(cx, |state, cx| state.set_selected_value(&id, window, cx));
                self.worker_name
                    .update(cx, |input, cx| input.set_value("", window, cx));
            } else if command == "orchestration.message.send" {
                self.message
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        Ok(())
    }

    fn create_run(&mut self, window: &Window, cx: &mut Context<Self>) {
        let title = self.run_title.read(cx).value().to_string();
        let goal = self.run_goal.read(cx).value().to_string();
        self.command(
            "orchestration.run.create",
            vec![title, goal],
            None,
            window,
            cx,
        );
    }

    fn create_task(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(run) = self.run.read(cx).selected_value().cloned() else {
            return;
        };
        self.command(
            "orchestration.task.create",
            vec![
                run,
                self.task_title.read(cx).value().to_string(),
                self.task_prompt.read(cx).value().to_string(),
            ],
            None,
            window,
            cx,
        );
    }

    fn attach_worker(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(run) = self.run.read(cx).selected_value().cloned() else {
            return;
        };
        let Some(handle) = self.session.read(cx).selected_value() else {
            return;
        };
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| &session.target.handle == handle)
        else {
            return;
        };
        self.command(
            "orchestration.worker.attach",
            vec![
                run,
                self.worker_name.read(cx).value().to_string(),
                session.provider.to_string(),
            ],
            Some(session.target.clone()),
            window,
            cx,
        );
    }

    fn dispatch(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (Some(run), Some(task), Some(worker)) = (
            self.run.read(cx).selected_value().cloned(),
            self.task.read(cx).selected_value().cloned(),
            self.worker.read(cx).selected_value().cloned(),
        ) else {
            return;
        };
        self.command(
            "orchestration.task.dispatch",
            vec![run, task, worker],
            None,
            window,
            cx,
        );
    }
}

fn choice(id: &str, label: String) -> Choice {
    Choice {
        id: id.to_owned(),
        label: label.into(),
    }
}

fn picker(window: &mut Window, cx: &mut Context<OrchestrationPanel>) -> Picker {
    cx.new(|cx| SelectState::new(Vec::<Choice>::new(), None, window, cx))
}

fn input(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<OrchestrationPanel>,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
}

fn textarea(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<OrchestrationPanel>,
) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .auto_grow(2, 4)
    })
}

fn update_picker(
    picker: &Picker,
    items: Vec<Choice>,
    window: &mut Window,
    cx: &mut Context<OrchestrationPanel>,
) {
    let selected = picker.read(cx).selected_value().cloned();
    let selected = selected
        .filter(|id| items.iter().any(|item| &item.id == id))
        .or_else(|| items.first().map(|item| item.id.clone()));
    picker.update(cx, |state, cx| {
        state.set_items(items, window, cx);
        if let Some(selected) = selected {
            state.set_selected_value(&selected, window, cx);
        } else {
            state.set_selected_index(None, window, cx);
        }
    });
}

const fn state_label(state: &OrchestrationTaskState) -> &'static str {
    match state {
        OrchestrationTaskState::Pending => "pending",
        OrchestrationTaskState::Dispatching => "dispatching",
        OrchestrationTaskState::Running => "running",
        OrchestrationTaskState::Completed => "completed",
        OrchestrationTaskState::Failed => "failed",
        OrchestrationTaskState::Interrupted => "interrupted",
    }
}

impl EventEmitter<PanelEvent> for OrchestrationPanel {}
impl Focusable for OrchestrationPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.run_title.focus_handle(cx)
    }
}
impl BasePanel for OrchestrationPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.coordination"
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.refresh(window, cx);
        }
    }
}
impl Panel for OrchestrationPanel {
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some("Coordination".into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Coordination"
    }
}

impl OrchestrationPanel {
    fn run_form(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child("New run")
            .child("Title")
            .child(Input::new(&self.run_title).disabled(self.pending))
            .child("Goal")
            .child(Textarea::new(&self.run_goal).disabled(self.pending))
            .child(
                Button::new("coordination-create-run")
                    .small()
                    .label("Create run")
                    .disabled(
                        self.pending
                            || self.run_title.read(cx).value().trim().is_empty()
                            || self.run_goal.read(cx).value().trim().is_empty(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.create_run(window, cx))),
            )
    }

    fn task_form(&self, editable: bool, cx: &Context<Self>) -> impl IntoElement {
        section(cx)
            .child("Add task")
            .child("Title")
            .child(Input::new(&self.task_title).disabled(self.pending))
            .child("Instructions")
            .child(Textarea::new(&self.task_prompt).disabled(self.pending))
            .child(
                Button::new("coordination-create-task")
                    .small()
                    .label("Add task")
                    .disabled(
                        !editable
                            || self.task_title.read(cx).value().trim().is_empty()
                            || self.task_prompt.read(cx).value().trim().is_empty(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.create_task(window, cx);
                    })),
            )
    }

    fn worker_form(&self, editable: bool, cx: &Context<Self>) -> impl IntoElement {
        section(cx)
            .child("Attach worker")
            .child("Agent terminal")
            .child(
                Select::new(&self.session)
                    .placeholder("Start an agent in a terminal first")
                    .disabled(self.pending)
                    .w_full(),
            )
            .child("Name")
            .child(Input::new(&self.worker_name).disabled(self.pending))
            .child(
                Button::new("coordination-attach-worker")
                    .small()
                    .label("Attach terminal")
                    .disabled(
                        !editable
                            || self.session.read(cx).selected_value().is_none()
                            || self.worker_name.read(cx).value().trim().is_empty(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.attach_worker(window, cx);
                    })),
            )
    }

    fn dispatch_form(
        &self,
        state: Option<&OrchestrationTaskState>,
        editable: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let can_dispatch = editable
            && state == Some(&OrchestrationTaskState::Pending)
            && self.worker.read(cx).selected_value().is_some();
        let can_retry = editable
            && state.is_some_and(|state| {
                matches!(
                    state,
                    OrchestrationTaskState::Failed | OrchestrationTaskState::Interrupted
                )
            });
        section(cx)
            .child("Task")
            .child(
                Select::new(&self.task)
                    .placeholder("No tasks yet")
                    .disabled(self.pending)
                    .w_full(),
            )
            .child("Worker")
            .child(
                Select::new(&self.worker)
                    .placeholder("No workers attached")
                    .disabled(self.pending)
                    .w_full(),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("coordination-dispatch")
                            .small()
                            .label("Dispatch task")
                            .disabled(!can_dispatch)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dispatch(window, cx);
                            })),
                    )
                    .child(
                        Button::new("coordination-retry")
                            .ghost()
                            .small()
                            .label("Retry")
                            .disabled(!can_retry)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let (Some(run), Some(task)) = (
                                    this.run.read(cx).selected_value().cloned(),
                                    this.task.read(cx).selected_value().cloned(),
                                ) {
                                    this.command(
                                        "orchestration.task.retry",
                                        vec![run, task],
                                        None,
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    ),
            )
    }

    fn message_form(
        &self,
        editable: bool,
        can_finish: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        section(cx)
            .child("Message selected worker")
            .child(Textarea::new(&self.message).disabled(self.pending))
            .child(
                Button::new("coordination-message-send")
                    .small()
                    .label("Send message")
                    .disabled(
                        !editable
                            || self.worker.read(cx).selected_value().is_none()
                            || self.message.read(cx).value().trim().is_empty(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let (Some(run), Some(worker)) = (
                            this.run.read(cx).selected_value().cloned(),
                            this.worker.read(cx).selected_value().cloned(),
                        ) {
                            this.command(
                                "orchestration.message.send",
                                vec![run, worker, this.message.read(cx).value().to_string()],
                                None,
                                window,
                                cx,
                            );
                        }
                    })),
            )
            .child(
                Button::new("coordination-finish-run")
                    .ghost()
                    .small()
                    .label("Finish run")
                    .disabled(!can_finish)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(run) = this.run.read(cx).selected_value().cloned() {
                            this.command("orchestration.run.finish", vec![run], None, window, cx);
                        }
                    })),
            )
    }
    fn run_content(
        &self,
        mut content: gpui_kit::Div,
        run: &OrchestrationRun,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let task = run
            .tasks
            .iter()
            .find(|task| Some(&task.id) == self.task.read(cx).selected_value());
        let editable = !self.pending && !run.finished;
        let can_finish = editable
            && !run.tasks.is_empty()
            && run
                .tasks
                .iter()
                .all(|task| task.state == OrchestrationTaskState::Completed);
        content = content
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(run.goal.clone()),
            )
            .child(div().child(format!(
                "{} tasks · {} workers · {} messages",
                run.tasks.len(),
                run.workers.len(),
                run.messages.len()
            )));
        if !run.finished {
            content = content
                .child(self.task_form(editable, cx))
                .child(self.worker_form(editable, cx));
        }
        content = content.child(self.dispatch_form(task.map(|task| &task.state), editable, cx));
        if let Some(task) = task {
            content = content.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(task.prompt.clone()),
            );
            if let Some(report) = &task.report {
                content = content.child(div().child(report.clone()));
            }
            if let Some(outcome) = &task.outcome
                && !matches!(outcome, CommandOutcome::Success { .. })
            {
                content = content.child(
                    div().text_color(cx.theme().danger).child(
                        crate::commands::command_outcome_message(outcome)
                            .unwrap_or_else(|| "Task delivery failed".to_owned()),
                    ),
                );
            }
        }
        if !run.finished {
            content = content.child(self.message_form(editable, can_finish, cx));
        }
        content = content.children(run.messages.iter().rev().take(8).map(|message| {
            section(cx)
                .child(format!("{} · {:?}", message.worker, message.state))
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.body.clone()),
                )
        }));
        content
    }
}

fn section(cx: &App) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .border_t_1()
        .border_color(cx.theme().border)
        .pt_3()
}

impl Render for OrchestrationPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .min_w_0()
            .text_sm()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child("Coordination")
                    .child(
                        Button::new("coordination-refresh")
                            .ghost()
                            .small()
                            .label(if self.pending {
                                "Updating…"
                            } else {
                                "Refresh"
                            })
                            .disabled(self.pending)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    ),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Coordinate tasks across your agent terminals."),
            );
        if let Some(error) = &self.error {
            content = content.child(div().text_color(cx.theme().danger).child(error.clone()));
        }
        content = content.child(self.run_form(cx)).child(
            section(cx).child("Run").child(
                Select::new(&self.run)
                    .placeholder("No runs yet")
                    .disabled(self.pending)
                    .w_full(),
            ),
        );
        if let Some(run) = self.selected_run(cx) {
            content = self.run_content(content, run, cx);
        }
        div()
            .id("coordination-panel")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(content)
            .overflow_y_scrollbar()
    }
}
