//! Center panels own presentation; their commands retain the captured task destination.

use bootty_control::{Caller, CommandInvocation};
use gpui_kit::component::{
    ActiveTheme as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState},
    kbd::Kbd,
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, prelude::*,
};

use crate::surface_creation::PendingNewSurface;

pub struct SurfaceCommand(pub CommandInvocation);

use crate::commands::SurfaceChooserAction;
const CHOOSER_CONTEXT: &str = "SurfaceChooser";
const CHOICES: [SurfaceChooserAction; 6] = [
    SurfaceChooserAction::Agent,
    SurfaceChooserAction::Terminal,
    SurfaceChooserAction::Claude,
    SurfaceChooserAction::Codex,
    SurfaceChooserAction::Pi,
    SurfaceChooserAction::EditProfiles,
];

#[derive(Clone, Copy)]
struct SurfaceChoice {
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    kind: &'static str,
    profile: Option<&'static str>,
}

fn key_hint(index: usize, focus: &FocusHandle, window: &Window) -> Option<Kbd> {
    let action = crate::gpui_actions::InvokeCommand::new(CommandInvocation::from_action(
        CHOICES.get(index)?.command(),
        Caller::Keybinding,
    ));
    Kbd::binding_for_action_in(&action, focus, window)
}

enum SurfaceContent {
    Unavailable,
    Chooser(PendingNewSurface),
    AgentForm {
        request: PendingNewSurface,
        view: Entity<crate::gpui::DialogView>,
    },
    Terminal {
        origin: crate::gpui_dock::surfaces::TerminalSurfaceOrigin,
        view: Option<Entity<crate::gpui_terminal_panel::TerminalPanel>>,
    },
}

pub struct SurfacePanel {
    content: SurfaceContent,
    focus: FocusHandle,
    subscriptions: Vec<Subscription>,
    option_focus: [FocusHandle; 6],
    active: bool,
}

impl SurfacePanel {
    pub fn chooser(request: PendingNewSurface, cx: &mut Context<Self>) -> Self {
        Self {
            content: SurfaceContent::Chooser(request),
            focus: cx.focus_handle().tab_stop(true),
            subscriptions: Vec::new(),
            option_focus: std::array::from_fn(|_| cx.focus_handle()),
            active: false,
        }
    }

    pub(crate) fn unavailable(cx: &mut Context<Self>) -> Self {
        Self {
            content: SurfaceContent::Unavailable,
            focus: cx.focus_handle(),
            subscriptions: Vec::new(),
            option_focus: std::array::from_fn(|_| cx.focus_handle()),
            active: false,
        }
    }

    pub(crate) fn terminal(
        origin: crate::gpui_dock::surfaces::TerminalSurfaceOrigin,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            content: SurfaceContent::Terminal { origin, view: None },
            focus: cx.focus_handle(),
            subscriptions: Vec::new(),
            option_focus: std::array::from_fn(|_| cx.focus_handle()),
            active: false,
        }
    }

    pub(crate) const fn terminal_origin(
        &self,
    ) -> Option<&crate::gpui_dock::surfaces::TerminalSurfaceOrigin> {
        match &self.content {
            SurfaceContent::Terminal { origin, .. } => Some(origin),
            _ => None,
        }
    }

    pub(crate) fn terminal_view(
        &self,
    ) -> Option<Entity<crate::gpui_terminal_panel::TerminalPanel>> {
        match &self.content {
            SurfaceContent::Terminal { view, .. } => view.clone(),
            _ => None,
        }
    }

    pub(crate) fn publish_terminal(
        &mut self,
        view: Entity<crate::gpui_terminal_panel::TerminalPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SurfaceContent::Terminal { view: held, .. } = &mut self.content {
            if held.as_ref() == Some(&view) {
                return;
            }
            view.update(cx, |view, cx| {
                BasePanel::set_active(view, self.active, window, cx);
            });
            *held = Some(view);
            cx.notify();
        }
    }

    pub(crate) fn retain_subscription(&mut self, subscription: Subscription) {
        self.subscriptions.push(subscription);
    }

    pub(crate) fn show_agent_form(
        &mut self,
        view: Entity<crate::gpui::DialogView>,
        cx: &mut Context<Self>,
    ) {
        if let SurfaceContent::Chooser(request) = &self.content {
            self.content = SurfaceContent::AgentForm {
                request: request.clone(),
                view,
            };
            cx.notify();
        }
    }

    fn choose(&self, kind: &str, profile: Option<&str>, cx: &mut Context<Self>) {
        let (SurfaceContent::Chooser(request) | SurfaceContent::AgentForm { request, .. }) =
            &self.content
        else {
            return;
        };
        let mut arguments = vec![request.id.to_string(), kind.to_owned()];
        arguments.extend(profile.map(str::to_owned));
        cx.emit(SurfaceCommand(CommandInvocation::new(
            "surface.choose",
            arguments,
            Caller::Internal,
        )));
    }

    fn cancel(&self, cx: &mut Context<Self>) {
        let (SurfaceContent::Chooser(request) | SurfaceContent::AgentForm { request, .. }) =
            &self.content
        else {
            return;
        };
        cx.emit(SurfaceCommand(CommandInvocation::new(
            "surface.cancel",
            vec![request.id.to_string()],
            Caller::Internal,
        )));
    }

    fn chooser_content(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .items_center()
            .justify_center()
            .p_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(gpui_kit::rems(24.0))
                    .gap_2()
                    .children(
                        [
                            ("surface-choose-agent", "Agent", "bot", "agent", None),
                            (
                                "surface-choose-terminal",
                                "Terminal",
                                "square-terminal",
                                "terminal",
                                None,
                            ),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(
                            |(index, (id, label, icon, kind, profile))| {
                                self.choice_button(
                                    SurfaceChoice {
                                        id,
                                        label,
                                        icon,
                                        kind,
                                        profile,
                                    },
                                    index,
                                    window,
                                    cx,
                                )
                            },
                        ),
                    )
                    .child(div().h(gpui_kit::px(1.0)).bg(cx.theme().border).my_2())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Terminal profiles"),
                    )
                    .children(
                        [
                            (
                                "claude",
                                "Claude Code",
                                bootty_agents::AgentKind::Claude.icon(),
                            ),
                            ("codex", "Codex", bootty_agents::AgentKind::Codex.icon()),
                            ("pi", "Pi", bootty_agents::AgentKind::Pi.icon()),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (provider, label, icon))| {
                            self.choice_button(
                                SurfaceChoice {
                                    id: provider,
                                    label,
                                    icon,
                                    kind: "profile",
                                    profile: Some(provider),
                                },
                                index.saturating_add(2),
                                window,
                                cx,
                            )
                        }),
                    )
                    .child(self.chooser_actions(window, cx)),
            )
    }

    fn chooser_actions(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let [_, _, _, _, _, edit_focus] = &self.option_focus;
        div().flex().flex_col().gap_2().child(
            div().w_full().track_focus(edit_focus).child(
                Button::new("surface-edit-profiles")
                    .debug_selector(|| "surface-edit-profiles".to_owned())
                    .ghost()
                    .w_full()
                    .when(edit_focus.contains_focused(window, cx), |button| {
                        button.bg(cx.theme().accent)
                    })
                    .justify_start()
                    .accessibility_label("Edit profiles")
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_start()
                            .gap_2()
                            .child(crate::gpui::icon(
                                "settings",
                                16.0,
                                cx.theme().muted_foreground,
                            ))
                            .child(div().flex_1().child("Edit profiles"))
                            .children(key_hint(5, &self.focus, window)),
                    )
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SurfaceCommand(CommandInvocation::new(
                            "open_setting",
                            vec!["agents.codex.enabled".to_owned()],
                            Caller::Internal,
                        )));
                    })),
            ),
        )
    }

    fn choice_button(
        &self,
        choice: SurfaceChoice,
        index: usize,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let SurfaceChoice {
            id,
            label,
            icon,
            kind,
            profile,
        } = choice;
        let focus = self.option_focus.get(index).unwrap_or(&self.focus);
        div().w_full().track_focus(focus).child(
            Button::new(id)
                .debug_selector(move || id.to_owned())
                .secondary()
                .when(focus.contains_focused(window, cx), |button| {
                    button
                        .bg(cx.theme().accent)
                        .border_color(cx.theme().muted_foreground)
                })
                .w_full()
                .h_10()
                .justify_start()
                .gap_3()
                .accessibility_label(label)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .justify_start()
                        .gap_3()
                        .child(
                            div()
                                .id(format!("surface-icon-{id}"))
                                .debug_selector(move || format!("surface-icon-{id}"))
                                .flex_none()
                                .child(crate::gpui::icon(icon, 20.0, cx.theme().foreground)),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .children(key_hint(index, &self.focus, window)),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.choose(kind, profile, cx))),
        )
    }

    fn choose_index(&self, index: usize, cx: &mut Context<Self>) {
        if !matches!(self.content, SurfaceContent::Chooser(_)) {
            return;
        }
        match index {
            0 => self.choose("agent", None, cx),
            1 => self.choose("terminal", None, cx),
            2 => self.choose("profile", Some("claude"), cx),
            3 => self.choose("profile", Some("codex"), cx),
            4 => self.choose("profile", Some("pi"), cx),
            5 => cx.emit(SurfaceCommand(CommandInvocation::new(
                "open_setting",
                vec!["agents.codex.enabled".to_owned()],
                Caller::Internal,
            ))),
            _ => self.cancel(cx),
        }
    }

    pub(crate) fn chooser_focused(&self, window: &Window, cx: &App) -> bool {
        matches!(self.content, SurfaceContent::Chooser(_))
            && self.focus.contains_focused(window, cx)
    }

    /// Apply the captured chooser navigation supplied by the host command path.
    pub fn navigate(
        &self,
        action: SurfaceChooserAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.content, SurfaceContent::Chooser(_)) {
            if action == SurfaceChooserAction::Cancel {
                self.cancel(cx);
            }
            return;
        }
        if action == SurfaceChooserAction::Cancel {
            self.cancel(cx);
            return;
        }
        let current = self
            .option_focus
            .iter()
            .position(|focus| focus.contains_focused(window, cx))
            .unwrap_or(0);
        let next = match action {
            SurfaceChooserAction::Next => current
                .saturating_add(1)
                .checked_rem(self.option_focus.len()),
            SurfaceChooserAction::Previous => Some(
                current
                    .checked_sub(1)
                    .unwrap_or_else(|| self.option_focus.len().saturating_sub(1)),
            ),
            SurfaceChooserAction::First => Some(0),
            SurfaceChooserAction::Last => Some(self.option_focus.len().saturating_sub(1)),
            SurfaceChooserAction::Confirm => {
                self.choose_index(current, cx);
                None
            }
            action => {
                if let Some(index) = CHOICES.iter().position(|choice| *choice == action) {
                    self.choose_index(index, cx);
                }
                None
            }
        };
        if let Some(focus) = next.and_then(|index| self.option_focus.get(index)) {
            focus.focus(window, cx);
            cx.notify();
        }
    }
}

impl EventEmitter<SurfaceCommand> for SurfacePanel {}
impl EventEmitter<PanelEvent> for SurfacePanel {}
impl Focusable for SurfacePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            SurfaceContent::Terminal {
                view: Some(view), ..
            } => view.read(cx).focus_handle(cx),
            SurfaceContent::Chooser(_) => self.option_focus[0].clone(),
            _ => self.focus.clone(),
        }
    }
}
impl BasePanel for SurfacePanel {
    fn panel_name(&self) -> &'static str {
        match self.content {
            SurfaceContent::Unavailable => "bootty.unavailable",
            SurfaceContent::Terminal { .. } => "bootty.terminal-window",
            SurfaceContent::Chooser(_) | SurfaceContent::AgentForm { .. } => {
                "bootty.surface-chooser"
            }
        }
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if let Some(view) = self.terminal_view() {
            view.update(cx, |view, cx| {
                BasePanel::set_active(view, active, window, cx);
            });
        }
    }

    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active = false;
        if let Some(view) = self.terminal_view() {
            view.update(cx, |view, cx| BasePanel::on_removed(view, window, cx));
        }
    }

    fn dump(&self, _: &App) -> PanelState {
        let info = match &self.content {
            SurfaceContent::Unavailable => serde_json::Value::Null,
            SurfaceContent::Terminal { origin, .. } => serde_json::json!(origin),
            SurfaceContent::Chooser(request) | SurfaceContent::AgentForm { request, .. } => {
                serde_json::json!({"request":request.id})
            }
        };
        PanelState {
            panel_name: self.panel_name().to_owned(),
            children: Vec::new(),
            info: PanelInfo::panel(info),
        }
    }
}
impl Panel for SurfacePanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(match self.content {
            SurfaceContent::Unavailable => "Unavailable".into(),
            SurfaceContent::Terminal { .. } => "Terminal".into(),
            SurfaceContent::Chooser(_) | SurfaceContent::AgentForm { .. } => "New tab".into(),
        })
    }

    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tab_name(cx).unwrap_or_default()
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for SurfacePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.content {
            SurfaceContent::Unavailable => {
                div().p_4().child("Panel unavailable").into_any_element()
            }
            SurfaceContent::Terminal {
                view: Some(view), ..
            } => div().size_full().child(view.clone()).into_any_element(),
            SurfaceContent::Terminal { .. } => div()
                .size_full()
                .p_4()
                .child("Terminal unavailable")
                .into_any_element(),
            SurfaceContent::Chooser(_) => self.chooser_content(window, cx).into_any_element(),
            SurfaceContent::AgentForm { view, .. } => div()
                .id("surface-agent-form")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .child(view.clone())
                .into_any_element(),
        };
        div()
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .key_context(if matches!(self.content, SurfaceContent::Chooser(_)) {
                CHOOSER_CONTEXT
            } else {
                "BoottySurfaceChooser"
            })
            .on_action(
                cx.listener(|_, action: &crate::gpui_actions::InvokeCommand, _, cx| {
                    if action.invocation().command.starts_with("ui.surface.") {
                        cx.emit(SurfaceCommand(action.invocation().clone()));
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .child(content)
    }
}
