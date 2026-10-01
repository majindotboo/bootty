use std::time::Duration;

use gpui_kit::{
    AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, StyledText, Subscription, Task, Window,
    component::{
        ActiveTheme, Disableable, Selectable, Sizable,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        tab::{Tab, TabBar},
    },
    div,
};
use serde_json::Value;

use crate::{
    TerminalPresentation,
    connection::{CommandResult, Connection, Invocation, Target},
    workspace::{LiveWorkspace, Session, Space},
};

/// Owns the phone connection and transient view selection. Desktop owns every resource/mutation.
pub struct WorkspaceView {
    #[cfg(target_os = "ios")]
    pub(crate) connection_task: Option<Task<()>>,
    connection: Option<Connection>,
    connected: bool,
    active: bool,
    spaces: Vec<Space>,
    selected: Option<(String, Session)>,
    selected_terminal: Option<Target>,
    selected_pane: Option<Target>,
    created: Option<Target>,
    terminal: TerminalPresentation,
    error: Option<String>,
    command_error: Option<String>,
    revision: u64,
    poll_task: Option<Task<()>>,
    command_task: Option<Task<()>>,
    pending: bool,
    confirmation: Option<Invocation>,
    create_space: Option<String>,
    input: Entity<InputState>,
    name: Entity<InputState>,
    cwd: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type terminal input…"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Session name"));
        let cwd = cx.new(|cx| InputState::new(window, cx).placeholder("/absolute/project/path"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.send_input(window, cx);
            }
        });
        let subscriptions = vec![subscription];
        #[cfg(target_os = "ios")]
        let subscriptions = {
            use gpui_kit::Focusable;
            let mut subscriptions = subscriptions;
            // Remove this adapter when the pinned iOS backend implements
            // PlatformWindow::text_input_state_changed for GPUI's input handler.
            for field in [&input, &name, &cwd] {
                let focus = field.read(cx).focus_handle(cx);
                subscriptions.push(cx.on_focus(&focus, window, |this, window, cx| {
                    this.update_keyboard(window, cx);
                }));
                subscriptions.push(cx.on_blur(&focus, window, |this, window, cx| {
                    this.update_keyboard(window, cx);
                }));
            }
            subscriptions
        };
        Self {
            #[cfg(target_os = "ios")]
            connection_task: None,
            connection: None,
            connected: false,
            active: true,
            spaces: vec![],
            selected: None,
            selected_terminal: None,
            selected_pane: None,
            created: None,
            terminal: TerminalPresentation::default(),
            error: None,
            command_error: None,
            revision: 0,
            poll_task: None,
            command_task: None,
            pending: false,
            confirmation: None,
            create_space: None,
            input,
            name,
            cwd,
            _subscriptions: subscriptions,
        }
    }

    #[cfg(target_os = "ios")]
    fn update_keyboard(&self, window: &Window, cx: &Context<Self>) {
        use gpui_kit::Focusable;
        if [&self.input, &self.name, &self.cwd]
            .iter()
            .any(|field| field.read(cx).focus_handle(cx).is_focused(window))
        {
            gpui_mobile::show_keyboard();
        } else {
            gpui_mobile::hide_keyboard();
        }
    }

    pub fn connect(&mut self, connection: Result<Connection, String>, cx: &mut Context<Self>) {
        self.poll_task = None;
        self.command_task = None;
        self.pending = false;
        self.connected = false;
        self.spaces.clear();
        self.selected = None;
        self.selected_terminal = None;
        self.selected_pane = None;
        self.created = None;
        self.terminal = TerminalPresentation::default();
        self.confirmation = None;
        self.command_error = None;
        self.revision = self.revision.wrapping_add(1);
        match connection {
            Ok(connection) => {
                self.connection = Some(connection);
                self.error = None;
                self.resume(cx);
            }
            Err(error) => {
                self.connection = None;
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.resume(cx);
        } else {
            self.poll_task = None;
            self.connected = false;
        }
        cx.notify();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        if !self.active || self.connection.is_none() || self.poll_task.is_some() {
            return;
        }
        self.poll_task = Some(cx.spawn(async |view, cx| {
            loop {
                let Ok(request) = view.update(cx, |this, _| {
                    this.connection.clone().map(|connection| {
                        (connection, this.selected_terminal.clone(), this.revision)
                    })
                }) else {
                    break;
                };
                let Some((connection, terminal, revision)) = request else {
                    break;
                };
                let result = cx
                    .background_executor()
                    .spawn(async move { LiveWorkspace::refresh(&connection, terminal.as_ref()) })
                    .await;
                if view
                    .update(cx, |this, cx| {
                        match result {
                            Ok(live) => {
                                this.connected = live.capture_error.is_none();
                                this.error = live.capture_error;
                                this.spaces = live.spaces;
                                this.select_created();
                                if revision == this.revision
                                    && let Some((scope, selected)) = &this.selected
                                {
                                    let current =
                                        this.spaces
                                            .iter()
                                            .find(|space| &space.scope == scope)
                                            .and_then(|space| {
                                                space.sessions.iter().find(|session| {
                                                    session.target == selected.target
                                                })
                                            });
                                    if let Some(current) = current {
                                        if this.selected_terminal.as_ref().is_some_and(|target| {
                                            !LiveWorkspace::contains_terminal(&this.spaces, target)
                                        }) {
                                            this.selected_terminal
                                                .clone_from(&current.terminal_target);
                                            this.selected_pane.clone_from(&current.pane_target);
                                            this.revision = this.revision.wrapping_add(1);
                                        }
                                        this.selected = Some((scope.clone(), current.clone()));
                                        this.terminal = live.terminal.unwrap_or_default();
                                    } else {
                                        this.selected = None;
                                        this.selected_terminal = None;
                                        this.selected_pane = None;
                                        this.terminal = TerminalPresentation::default();
                                    }
                                }
                            }
                            Err(error) => {
                                this.connected = false;
                                this.error = Some(error);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                // Read refreshes reconnect automatically; mutations are never queued or replayed.
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
    }

    fn select_created(&mut self) {
        let Some(target) = &self.created else {
            return;
        };
        let Some((scope, session)) = self.spaces.iter().find_map(|space| {
            space
                .sessions
                .iter()
                .find(|session| {
                    session.target == *target
                        || session.terminal_target.as_ref() == Some(target)
                        || session
                            .windows
                            .iter()
                            .flat_map(|tab| &tab.panes)
                            .any(|pane| &pane.terminal_target == target)
                })
                .map(|session| (space.scope.clone(), session.clone()))
        }) else {
            return;
        };
        let terminal = if target.kind == "terminal" {
            Some(target.clone())
        } else {
            session.terminal_target.clone()
        };
        self.selected_pane = session
            .windows
            .iter()
            .flat_map(|tab| &tab.panes)
            .find(|pane| Some(&pane.terminal_target) == terminal.as_ref())
            .map(|pane| pane.target.clone())
            .or_else(|| session.pane_target.clone());
        self.selected_terminal = terminal;
        self.selected = Some((scope, session));
        self.created = None;
        self.create_space = None;
        self.terminal = TerminalPresentation::default();
        self.revision = self.revision.wrapping_add(1);
    }

    fn select(&mut self, scope: String, session: Session, cx: &mut Context<Self>) {
        self.selected_terminal.clone_from(&session.terminal_target);
        self.selected_pane.clone_from(&session.pane_target);
        self.selected = Some((scope, session));
        self.terminal = TerminalPresentation::default();
        self.command_error = None;
        self.confirmation = None;
        self.create_space = None;
        self.revision = self.revision.wrapping_add(1);
        self.poll_task = None;
        self.resume(cx);
        cx.notify();
    }

    fn terminal_target(&self) -> Option<Target> {
        self.selected_terminal.clone()
    }

    fn topology(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = if command == "new_tab" {
            self.selected
                .as_ref()
                .map(|(_, session)| session.target.clone())
        } else {
            self.selected_pane.clone()
        };
        if let Some(target) = target {
            self.run(
                vec![Invocation::new(command, vec![], Some(target))],
                false,
                window,
                cx,
            );
        }
    }

    fn run(
        &mut self,
        invocations: Vec<Invocation>,
        clear_input: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending || !self.connected {
            return;
        }
        let Some(connection) = self.connection.clone() else {
            return;
        };
        self.pending = true;
        self.command_error = None;
        self.confirmation = None;
        cx.notify();
        self.command_task = Some(cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut value = Value::Null;
                    for invocation in invocations {
                        match connection.invoke(&invocation)? {
                            CommandResult::Value(result) => value = result,
                            CommandResult::Confirmation(confirmation) => {
                                let mut invocation = invocation;
                                invocation.confirmation = Some(confirmation);
                                return Ok((value, Some(invocation)));
                            }
                        }
                    }
                    Ok::<_, String>((value, None))
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok((value, confirmation)) => {
                        if confirmation.is_none() {
                            this.created = value
                                .get("terminal_target")
                                .or_else(|| value.get("terminal"))
                                .or_else(|| value.get("created"))
                                .and_then(|target| {
                                    serde_json::from_value::<Target>(target.clone()).ok()
                                })
                                .filter(|target| {
                                    matches!(target.kind.as_str(), "session" | "terminal")
                                });
                        }
                        if this.created.is_some() {
                            window.blur(cx);
                        }
                        this.confirmation = confirmation;
                        if clear_input && this.confirmation.is_none() {
                            this.input
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                    Err(error) => this.command_error = Some(error),
                }
                // Re-read authoritative topology/output instead of guessing mutation success.
                this.poll_task = None;
                this.resume(cx);
                cx.notify();
            });
        }));
    }

    fn send_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.terminal_target() else {
            return;
        };
        let text = self.input.read(cx).value().to_string();
        let mut commands = Vec::new();
        if !text.is_empty() {
            commands.push(Invocation::new(
                "terminal.paste",
                vec![text],
                Some(target.clone()),
            ));
        }
        commands.push(Invocation::new("terminal.submit", vec![], Some(target)));
        self.run(commands, true, window, cx);
    }

    fn key(&mut self, bytes: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.terminal_target() else {
            return;
        };
        self.run(
            vec![Invocation::new(
                "terminal.write",
                vec![bytes.into()],
                Some(target),
            )],
            false,
            window,
            cx,
        );
    }

    fn create(&mut self, provider: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self
            .spaces
            .iter()
            .find(|space| Some(&space.scope) == self.create_space.as_ref())
        else {
            return;
        };
        let cwd = self.cwd.read(cx).value().to_string();
        if !cwd.starts_with('/') {
            self.command_error = Some("Enter an absolute project directory on the computer".into());
            cx.notify();
            return;
        }
        let invocation = if let Some(provider) = provider {
            Invocation::new(
                format!("agents.{provider}.start"),
                vec![cwd],
                Some(space.target.clone()),
            )
        } else {
            Invocation::new(
                "session.create",
                vec![self.name.read(cx).value().to_string(), cwd],
                Some(space.target.clone()),
            )
        };
        self.run(vec![invocation], false, window, cx);
    }

    fn render_spaces(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let mut content = div().flex().flex_col().gap_4();
        if self.spaces.is_empty() {
            return content.child("No Spaces are open on this computer");
        }
        for space in &self.spaces {
            let scope = space.scope.clone();
            let mut group = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_lg().child(space.name.clone()))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} · {}", space.host, space.backend)),
                );
            for session in &space.sessions {
                let session = session.clone();
                let scope = scope.clone();
                group = group.child(
                    Button::new(gpui_kit::SharedString::from(session.target.handle.clone()))
                        .large()
                        .outline()
                        .w_full()
                        .min_h_12()
                        .h_auto()
                        .accessibility_label(session.name.clone())
                        .child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .child(session.name.clone()),
                        )
                        .disabled(!self.connected)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            window.blur(cx);
                            this.select(scope.clone(), session.clone(), cx);
                        })),
                );
            }
            group = group.child(
                Button::new(gpui_kit::SharedString::from(format!("create-{scope}")))
                    .large()
                    .outline()
                    .label("New session…")
                    .min_h_12()
                    .disabled(!self.connected || self.pending)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.blur(cx);
                        this.create_space = Some(scope.clone());
                        this.command_error = None;
                        cx.notify();
                    })),
            );
            content = content.child(group);
        }
        content
    }

    fn render_create(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let title = self
            .spaces
            .iter()
            .find(|space| Some(&space.scope) == self.create_space.as_ref())
            .map_or("New session".into(), |space| {
                format!("New session · {}", space.name)
            });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                Button::new("cancel-create")
                    .ghost()
                    .large()
                    .label("Back to Spaces")
                    .min_h_12()
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.blur(cx);
                        this.create_space = None;
                        cx.notify();
                    })),
            )
            .child(div().text_lg().child(title))
            .child("Project directory on the computer")
            .child(Input::new(&self.cwd).large())
            .child("Session name")
            .child(Input::new(&self.name).large())
            .child(
                Button::new("create-shell")
                    .large()
                    .primary()
                    .label("Create shell")
                    .min_h_12()
                    .disabled(self.pending || !self.connected)
                    .on_click(cx.listener(|this, _, window, cx| this.create(None, window, cx))),
            )
            .child("Or launch an agent terminal in this project")
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("start-codex")
                            .large()
                            .outline()
                            .label("Codex")
                            .min_h_12()
                            .disabled(self.pending || !self.connected)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create(Some("codex"), window, cx);
                            })),
                    )
                    .child(
                        Button::new("start-claude")
                            .large()
                            .outline()
                            .label("Claude")
                            .min_h_12()
                            .disabled(self.pending || !self.connected)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create(Some("claude"), window, cx);
                            })),
                    )
                    .child(
                        Button::new("start-pi")
                            .large()
                            .outline()
                            .label("Pi")
                            .min_h_12()
                            .disabled(self.pending || !self.connected)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create(Some("pi"), window, cx);
                            })),
                    ),
            )
    }

    fn choose_pane(&mut self, pane: crate::workspace::Pane, cx: &mut Context<Self>) {
        self.selected_terminal = Some(pane.terminal_target);
        self.selected_pane = Some(pane.target);
        self.terminal = TerminalPresentation::default();
        self.revision = self.revision.wrapping_add(1);
        self.poll_task = None;
        self.resume(cx);
        cx.notify();
    }

    fn render_terminal_header(
        &self,
        session: &Session,
        typing: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("back-to-spaces")
                    .ghost()
                    .large()
                    .label("Spaces")
                    .min_h_12()
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.blur(cx);
                        this.selected = None;
                        this.selected_terminal = None;
                        this.selected_pane = None;
                        this.confirmation = None;
                        this.revision = this.revision.wrapping_add(1);
                        this.poll_task = None;
                        this.resume(cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_lg()
                    .truncate()
                    .child(session.name.clone()),
            )
            .child(
                Button::new("close-session")
                    .ghost()
                    .large()
                    .label(if typing { "Tabs" } else { "Close…" })
                    .min_h_12()
                    .disabled(self.pending || !self.connected)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.blur(cx);
                        if typing {
                            cx.notify();
                            return;
                        }
                        if let Some((_, session)) = &this.selected {
                            let mut invocation = Invocation::new(
                                "session.close",
                                vec![],
                                Some(session.target.clone()),
                            );
                            invocation.confirm();
                            this.confirmation = Some(invocation);
                            cx.notify();
                        }
                    })),
            )
    }

    fn render_tabs(&self, session: &Session, cx: &Context<Self>) -> gpui_kit::Div {
        let disabled = self.pending || !self.connected;
        let selected_tab = session
            .windows
            .iter()
            .position(|tab| {
                tab.panes
                    .iter()
                    .any(|pane| Some(&pane.terminal_target) == self.selected_terminal.as_ref())
            })
            .unwrap_or(0);
        let mut content = div().flex().flex_col().gap_2();
        if !session.windows.is_empty() {
            let tabs = session
                .windows
                .iter()
                .map(|tab| {
                    let pane = tab.panes.first().cloned();
                    Tab::new()
                        .label(tab.name.clone())
                        .aria_label(format!("Terminal tab {}", tab.name))
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(pane) = &pane {
                                this.choose_pane(pane.clone(), cx);
                            }
                        }))
                })
                .collect::<Vec<_>>();
            content = content.child(
                TabBar::new("terminal-tabs")
                    .large()
                    .selected_index(selected_tab)
                    .children(tabs),
            );
        }
        if let Some(tab) = session
            .windows
            .get(selected_tab)
            .filter(|tab| tab.panes.len() > 1)
        {
            let mut panes = div().flex().flex_wrap().gap_2();
            for (ix, pane) in tab.panes.iter().enumerate() {
                let pane = pane.clone();
                panes = panes.child(
                    Button::new(gpui_kit::SharedString::from(pane.target.handle.clone()))
                        .large()
                        .outline()
                        .label(format!("Pane {}", ix + 1))
                        .min_h_12()
                        .selected(self.selected_terminal.as_ref() == Some(&pane.terminal_target))
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_pane(pane.clone(), cx);
                        })),
                );
            }
            content = content.child(panes);
        }
        content
    }

    fn render_topology(&self, session: &Session, cx: &Context<Self>) -> gpui_kit::Div {
        let mut content = div().flex().gap_2();
        for (command, label) in [
            ("new_tab", "New tab"),
            ("split_right", "Split right"),
            ("split_down", "Split down"),
        ] {
            content = content.child(
                Button::new(command)
                    .large()
                    .outline()
                    .label(label)
                    .min_h_12()
                    .disabled(
                        !self.connected
                            || self.pending
                            || !session.topology_supported
                            || (command != "new_tab" && self.selected_pane.is_none()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.topology(command, window, cx);
                    })),
            );
        }
        content
    }

    fn render_terminal_input(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let disabled = self.pending || !self.connected;
        let mut keys = div().id("terminal-keys").flex().overflow_x_scroll().gap_2();
        for (id, label, bytes) in [
            ("escape", "Esc", "\u{1b}"),
            ("tab", "Tab", "\t"),
            ("interrupt", "Ctrl+C", "\u{3}"),
            ("up", "↑", "\u{1b}[A"),
            ("down", "↓", "\u{1b}[B"),
            ("left", "←", "\u{1b}[D"),
            ("right", "→", "\u{1b}[C"),
        ] {
            keys = keys.child(
                Button::new(id)
                    .large()
                    .outline()
                    .label(label)
                    .accessibility_label(format!("Terminal {id} key"))
                    .flex_shrink_0()
                    .min_h_12()
                    .min_w_12()
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.key(bytes, window, cx);
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.input).large().disabled(disabled)),
                    )
                    .child(
                        Button::new("send-input")
                            .large()
                            .primary()
                            .label("Send ↵")
                            .accessibility_label("Send terminal input and Enter")
                            .min_h_12()
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.send_input(window, cx);
                            })),
                    ),
            )
            .child(keys)
    }

    fn render_terminal(
        &self,
        session: &Session,
        typing: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Div {
        let mut content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(self.render_terminal_header(session, typing, cx));
        // Give output room while typing; Tabs dismisses input and restores topology.
        if !typing {
            content = content
                .child(self.render_tabs(session, cx))
                .child(self.render_topology(session, cx));
        }
        let content = content.child(
            div()
                .id("terminal-output")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .overflow_x_scroll()
                .child(
                    div()
                        .font_family("Menlo")
                        .text_sm()
                        .whitespace_nowrap()
                        .child(
                            StyledText::new(if self.terminal.text.is_empty() {
                                "Waiting for live terminal output…".to_owned()
                            } else {
                                self.terminal.text.clone()
                            })
                            .with_highlights(self.terminal.highlights.clone()),
                        ),
                ),
        );
        if self.terminal_target().is_none() {
            content.child("This backend has no terminal target available")
        } else {
            content.child(self.render_terminal_input(cx))
        }
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let terminal_mode =
            self.selected.is_some() && self.create_space.is_none() && self.confirmation.is_none();
        let mut content = div().flex().flex_col().gap_4().p_4().w_full().min_w_0();
        if let Some(connection) = &self.connection {
            content = content.child(div().text_sm().text_color(theme.muted_foreground).child(
                format!(
                    "{} · {}",
                    if self.connected {
                        "Live"
                    } else {
                        "Connecting…"
                    },
                    connection.address()
                ),
            ));
        } else {
            content = content
                .child(div().text_lg().child("Control Bootty on your computer"))
                .child("Enable remote control in Bootty, copy its pairing code, then tap Connect.");
        }
        if let Some(error) = &self.error {
            content = content.child(div().text_color(theme.danger).child(error.clone()));
        }
        if let Some(error) = &self.command_error {
            content = content.child(div().text_color(theme.danger).child(error.clone()));
        }
        if self.pending {
            content = content.child("Sending command…");
        }
        if let Some(invocation) = &self.confirmation {
            let invocation = invocation.clone();
            content = content
                .child(self.selected.as_ref().map_or_else(
                    || "Close this session and its running processes?".to_owned(),
                    |(_, session)| format!("Close “{}” and its running processes?", session.name),
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("cancel-close")
                                .large()
                                .outline()
                                .label("Cancel")
                                .min_h_12()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirmation = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("confirm-close")
                                .large()
                                .danger()
                                .label("Close session")
                                .min_h_12()
                                .disabled(!self.connected || self.pending)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.run(vec![invocation.clone()], false, window, cx);
                                })),
                        ),
                );
        } else if self.create_space.is_some() {
            content = content.child(self.render_create(cx));
        } else if let Some((_, session)) = &self.selected {
            content = content.child(self.render_terminal(
                session,
                self.input.read(cx).focus_handle(cx).is_focused(window),
                cx,
            ));
        } else if self.connection.is_some() {
            content = content.child(self.render_spaces(cx));
        }
        if terminal_mode {
            content = content.h_full().min_h_0();
        }
        let root = div()
            .id("workspace-scroll")
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground);
        if terminal_mode {
            root.child(content)
        } else {
            root.overflow_y_scroll().child(content)
        }
    }
}
