//! Live projection of provider-owned sessions; every action uses the command catalog.
use crate::gpui::ElementTooltip as _;
use crate::{gpui::chrome::GpuiChrome, state::agent_attention::AgentOverview};
use bootty_control::{BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::*,
};

pub struct AgentsPanel {
    sender: BoundAppCommandSender,
    chrome: Entity<GpuiChrome>,
    entries: Vec<AgentOverview>,
    selected: Option<bootty_control::CommandTarget>,
    focus: FocusHandle,
    query: Entity<InputState>,
    attention_only: bool,
    /// Rows with a command in flight; `None` is Next unread. Other rows stay usable.
    pending: Vec<Option<bootty_control::CommandTarget>>,
    error: Option<String>,
}
impl AgentsPanel {
    pub(crate) fn new(
        sender: BoundAppCommandSender,
        chrome: Entity<GpuiChrome>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Find agents…"));
        cx.subscribe(&query, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        Self {
            query,
            attention_only: false,
            sender,
            chrome,
            entries: Vec::new(),
            selected: None,
            focus: cx.focus_handle(),
            pending: Vec::new(),
            error: None,
        }
    }
    pub(crate) fn update_entries(
        &mut self,
        entries: Vec<AgentOverview>,
        selected: Option<bootty_control::CommandTarget>,
        cx: &mut Context<Self>,
    ) {
        if self.entries != entries || self.selected != selected {
            self.entries = entries;
            self.selected = selected;
            cx.notify();
        }
    }
    fn invoke(
        &mut self,
        entry: Option<AgentOverview>,
        action: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let key = entry.as_ref().map(|entry| entry.target.clone());
        if self.pending.contains(&key) {
            return;
        }
        let mut commands = Vec::new();
        if matches!(action, "resume" | "fork") {
            let mut focus = CommandInvocation::from_action("agents.focus", Caller::Internal);
            focus.target = entry.as_ref().map(|entry| entry.target.clone());
            commands.push(focus);
        }
        let name = entry.as_ref().map_or_else(
            || "agents.next".to_owned(),
            |entry| {
                if action == "focus" {
                    "agents.focus".to_owned()
                } else {
                    format!("agents.{}.{action}", entry.provider)
                }
            },
        );
        let mut command = CommandInvocation::from_action(&name, Caller::Internal);
        if let Some(entry) = entry {
            command.target = Some(entry.target);
            if action == "acknowledge" {
                command.arguments = vec![entry.attention_sequence];
            }
        }
        commands.push(command);
        let failed = match action {
            "resume" => "Could not resume the agent",
            "fork" => "Could not fork the agent",
            "acknowledge" => "Could not mark the agent as seen",
            "focus" => "Could not open the agent",
            _ => "Could not update the agent",
        };
        let sender = self.sender.clone();
        self.pending.push(key.clone());
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    for command in commands {
                        let receiver = sender
                            .submit(
                                command,
                                std::time::Instant::now()
                                    .checked_add(std::time::Duration::from_secs(30))
                                    .unwrap_or_else(std::time::Instant::now),
                                CommandCancellation::new(),
                            )
                            .map_err(|error| {
                                crate::error_catalog::command_send_message(error).to_owned()
                            })?;
                        let outcome = receiver.recv().map_err(|_| {
                            "The agent command stopped before it finished.".to_owned()
                        })?;
                        if let Some(error) = crate::commands::command_outcome_message(&outcome) {
                            return Err(crate::error_catalog::ErrorNotice::failed(failed, error)
                                .to_string());
                        }
                    }
                    Ok::<(), String>(())
                })
                .await;
            _ = weak.update_in(cx, |this, _, cx| {
                this.pending.retain(|pending| *pending != key);
                this.error = result.err();
                cx.notify();
            });
        })
        .detach();
    }
}
crate::gpui_dock::tool_panel!(AgentsPanel, "bootty.agents", "Agents", padding = true);
impl Focusable for AgentsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl AgentsPanel {
    fn render_filters(&self, attention: usize, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("agents-all")
                    .label(format!("All {}", self.entries.len()))
                    .ghost()
                    .small()
                    .selected(!self.attention_only)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.attention_only = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("agents-attention")
                    .label(format!("Needs attention {attention}"))
                    .ghost()
                    .small()
                    .selected(self.attention_only)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.attention_only = true;
                        cx.notify();
                    })),
            )
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.is_visible()
            && !cx.reduce_motion()
            && self.entries.iter().any(|entry| agent_group(entry).0 == 1)
        {
            crate::motion::request_frame(cx.entity_id(), cx);
        }
        let query = self.query.read(cx).value().to_lowercase();
        let attention = self
            .entries
            .iter()
            .filter(|entry| needs_attention(entry))
            .count();
        let mut visible = self
            .entries
            .iter()
            .filter(|entry| {
                (!self.attention_only || needs_attention(entry))
                    && [
                        entry.title.as_str(),
                        entry.host.as_str(),
                        entry.cwd.as_deref().unwrap_or(""),
                        entry.status.as_str(),
                        entry.provider.module(),
                    ]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&query))
            })
            .collect::<Vec<_>>();
        visible.sort_by_key(|entry| agent_group(entry).0);
        let no_matches = visible.is_empty() && !self.entries.is_empty();
        let rows = visible
            .chunk_by(|a, b| agent_group(a).0 == agent_group(b).0)
            .flat_map(|group| {
                group.iter().enumerate().map(|(index, entry)| {
                    let heading =
                        (index == 0).then(|| format!("{} · {}", agent_group(entry).1, group.len()));
                    self.render_agent(entry, heading, cx)
                })
            });
        div()
            .id("agents-panel")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .child(
                Input::new(&self.query)
                    .context_menu(crate::gpui::text_menu(&self.query))
                    .small(),
            )
            .child(self.render_filters(attention, cx))
            .when(no_matches, |body| {
                body.child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No matching agents"),
                )
            })
            .child(
                Button::new("next-agent")
                    .label("Next unread")
                    .ghost()
                    .small()
                    .disabled(
                        self.pending.contains(&None)
                            || !self.entries.iter().any(|entry| entry.unread),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.invoke(None, "next", window, cx)),
                    ),
            )
            .children(self.error.clone().map(|error| {
                gpui_kit::component::alert::Alert::error("agent-command-error", error).on_close(
                    cx.listener(|this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    }),
                )
            }))
            .when(self.entries.is_empty(), |body| {
                body.child(
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child("No active agents"),
                )
            })
            .children(rows)
            .children(self.chrome.read(cx).dock_codexbar())
    }
}

pub fn needs_attention(entry: &AgentOverview) -> bool {
    entry.unread || matches!(entry.status.as_str(), "waiting" | "error")
}

fn agent_group(entry: &AgentOverview) -> (u8, &'static str) {
    match entry.status.as_str() {
        "waiting" | "error" => (0, "Needs attention"),
        "working" => (1, "Working"),
        "complete" => (2, "Ready to review"),
        status if status.starts_with("tool:") => (1, "Working"),
        _ if entry.unread => (2, "Ready to review"),
        _ => (3, "Idle"),
    }
}

/// Status words shared with the sidebar badges.
pub fn status_label(entry: &AgentOverview) -> &str {
    match entry.status.as_str() {
        "waiting" => "Waiting",
        "error" => "Failed",
        "working" => "Working",
        "complete" => "Finished",
        "idle" => "Idle",
        "stopped" => "Stopped",
        other => other.strip_prefix("tool:").unwrap_or(other),
    }
}

impl AgentsPanel {
    fn actions_button(
        &self,
        entry: &AgentOverview,
        id: &str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.weak_entity();
        let menu_entry = entry.clone();
        let pending = self.pending.contains(&Some(entry.target.clone()));
        Button::new(SharedString::from(format!("{id}:actions")))
            .icon(IconName::Ellipsis)
            .ghost()
            .small()
            .accessibility_label("Agent actions")
            .element_tooltip("Agent actions")
            .dropdown_menu(move |mut menu, _, _| {
                for (action, label, enabled) in [
                    ("resume", "Resume", menu_entry.can_resume),
                    ("fork", "Fork", menu_entry.can_resume),
                    ("acknowledge", "Mark read", menu_entry.unread),
                ] {
                    let owner = owner.clone();
                    let entry = menu_entry.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .disabled(pending || !enabled)
                            .on_click(move |_, window, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.invoke(Some(entry.clone()), action, window, cx);
                                });
                            }),
                    );
                }
                menu
            })
    }
    fn render_agent(
        &self,
        entry: &AgentOverview,
        heading: Option<String>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (rank, _) = agent_group(entry);
        let status_color = match rank {
            0 => cx.theme().warning,
            1 => cx.theme().primary,
            2 => cx.theme().success,
            _ => cx.theme().muted_foreground,
        };
        let id = format!("agent:{}:{}:{}", entry.provider, entry.scope, entry.pane);
        let focus_entry = entry.clone();
        let pending = self.pending.contains(&Some(entry.target.clone()));
        div()
            .id(SharedString::from(id.clone()))
            .flex()
            .flex_col()
            .min_w_0()
            .gap_1()
            .py_2()
            .when_some(heading, |row, heading| {
                row.child(
                    div()
                        .pb_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(heading),
                )
            })
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("{id}:focus")))
                            .ghost()
                            .small()
                            .flex_1()
                            .min_w_0()
                            .selected(self.selected.as_ref() == Some(&entry.target))
                            .accessibility_label(entry.title.clone())
                            .child(div().flex_1().min_w_0().truncate().child(format!(
                                "{}{}",
                                if entry.unread { "● " } else { "" },
                                entry.title
                            )))
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.invoke(Some(focus_entry.clone()), "focus", window, cx);
                            })),
                    )
                    .child(self.actions_button(entry, &id, cx)),
            )
            .child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(status_color)
                    .when(rank == 1 && !cx.reduce_motion(), |status| {
                        status.opacity(crate::motion::pulse())
                    })
                    .child(format!(
                        "{} · {}",
                        entry.provider.display_name(),
                        status_label(entry)
                    )),
            )
            .child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(format!(
                        "{} · {}",
                        entry.host,
                        entry.cwd.as_deref().unwrap_or(&entry.pane)
                    )),
            )
    }
}
