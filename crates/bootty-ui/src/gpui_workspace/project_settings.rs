//! The project page owns its controls; persistence stays on the shared command path.
use crate::presentation::project_editor::ProjectSettingsEditor;
use bootty_control::{BoundAppCommandSender, CommandCancellation, CommandOutcome};
use bootty_mux::repository::ProjectSettings;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, Focusable as _, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div, prelude::*, rems,
};
use std::time::{Duration, Instant};

pub(super) struct ProjectSettingsView {
    model: ProjectSettingsEditor,
    name: Entity<InputState>,
    branch_prefix: Entity<InputState>,
    start_ref: Entity<InputState>,
    sender: BoundAppCommandSender,
    saving: bool,
}

impl ProjectSettingsView {
    pub(super) fn new(
        model: ProjectSettingsEditor,
        sender: BoundAppCommandSender,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(model.settings.name.clone())
                .placeholder(model.name().to_owned())
        });
        let branch_prefix = cx.new(|cx| {
            InputState::new(window, cx).default_value(model.settings.branch_prefix.clone())
        });
        let start_ref = cx
            .new(|cx| InputState::new(window, cx).default_value(model.settings.start_ref.clone()));
        Self {
            model,
            name,
            branch_prefix,
            start_ref,
            sender,
            saving: false,
        }
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn save(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.model.settings.name = self.name.read(cx).value().to_string();
        self.model.settings.branch_prefix = self.branch_prefix.read(cx).value().to_string();
        self.model.settings.start_ref = self.start_ref.read(cx).value().to_string();
        let result = self.model.configure().and_then(|invocation| {
            let now = Instant::now();
            self.sender
                .submit(
                    invocation,
                    now.checked_add(Duration::from_secs(30)).unwrap_or(now),
                    CommandCancellation::new(),
                )
                .map_err(|error| format!("Could not save project settings: {error:?}"))
        });
        match result {
            Ok(receiver) => {
                self.saving = true;
                self.model.error = None;
                cx.spawn_in(window, async move |owner, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { receiver.recv() })
                        .await;
                    _ = owner.update_in(cx, |this, _, cx| {
                        this.saving = false;
                        this.model.error = match result {
                            Ok(CommandOutcome::Success { .. }) => None,
                            Ok(outcome) => crate::commands::command_outcome_message(&outcome),
                            Err(_) => Some("The application command owner stopped".into()),
                        };
                        cx.notify();
                    });
                })
                .detach();
            }
            Err(error) => self.model.error = Some(error),
        }
        cx.notify();
    }

    fn choice(
        &self,
        id: &'static str,
        options: &'static [(&'static str, &'static str)],
        selected: &str,
        apply: fn(&mut ProjectSettings, &str),
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let label = options
            .iter()
            .find(|(value, _)| *value == selected)
            .map_or(selected, |(_, label)| label)
            .to_owned();
        let content = Self::choice_content(id, selected, &label, cx);
        let field = match id {
            "project-icon" => "Icon",
            "project-provider" => "Default agent",
            "project-isolation" => "New sessions",
            _ => id,
        };
        let selected = selected.to_owned();
        let owner = cx.weak_entity();
        Button::new(id)
            .outline()
            .small()
            .accessibility_label(format!("{field}: {label}"))
            .child(content)
            .dropdown_caret(true)
            .disabled(self.saving)
            .dropdown_menu(move |mut menu, _, _| {
                for &(value, label) in options {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            div()
                                .id(gpui_kit::SharedString::from(format!(
                                    "project-choice-{id}-{value}"
                                )))
                                .role(gpui_kit::Role::MenuItem)
                                .aria_label(label)
                                .child(Self::choice_content(id, value, label, cx))
                        })
                        .checked(selected == value)
                        .on_click(move |_, _, cx| {
                            _ = owner.update(cx, |this, cx| {
                                apply(&mut this.model.settings, value);
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn choice_content(id: &str, value: &str, label: &str, cx: &App) -> gpui_kit::AnyElement {
        let icon = match id {
            "project-icon" => {
                if value.is_empty() {
                    "folder"
                } else {
                    value
                }
            }
            "project-provider" => match value {
                "codex" => bootty_agents::AgentKind::Codex.icon(),
                "claude" => bootty_agents::AgentKind::Claude.icon(),
                "pi" => bootty_agents::AgentKind::Pi.icon(),
                _ => "bot",
            },
            _ => {
                if value == "worktree" {
                    "git-branch"
                } else {
                    "folder"
                }
            }
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(crate::gpui::sized_icon(
                icon,
                crate::gpui::IconSize::Small,
                cx.theme().foreground,
            ))
            .child(label.to_owned())
            .into_any_element()
    }

    fn image_control(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .gap_2()
            .child(
                Button::new("project-image")
                    .outline()
                    .small()
                    .label("Choose image…")
                    .disabled(self.saving)
                    .on_click(cx.listener(|_, _, window, cx| {
                        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Choose image".into()),
                        });
                        cx.spawn_in(window, async move |owner, cx| {
                            if let Ok(Ok(Some(paths))) = paths.await
                                && let Some(path) = paths.first()
                            {
                                _ = owner.update_in(cx, |this, _, cx| {
                                    if !this.saving {
                                        this.model.settings.icon_path =
                                            Some(path.to_string_lossy().into_owned());
                                        cx.notify();
                                    }
                                });
                            }
                        })
                        .detach();
                    })),
            )
            .when_some(self.model.settings.icon_path.as_ref(), |row, path| {
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(path.clone()),
                )
                .child(
                    Button::new("project-clear-image")
                        .ghost()
                        .small()
                        .label("Clear")
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.settings.icon_path = None;
                            cx.notify();
                        })),
                )
            })
    }

    fn project_choices(&self, cx: &Context<Self>) -> [gpui_kit::AnyElement; 3] {
        let icon = self.choice(
            "project-icon",
            &[
                ("", "Automatic"),
                ("folder", "Folder"),
                ("code", "Code"),
                ("terminal", "Terminal"),
                ("bot", "Agent"),
                ("globe", "Globe"),
                ("book", "Book"),
                ("rocket", "Rocket"),
                ("server", "Server"),
                ("gamepad", "Game"),
            ],
            &self.model.settings.icon,
            |settings, value| {
                settings.icon = value.into();
                settings.icon_path = None;
            },
            cx,
        );
        let provider = self.choice(
            "project-provider",
            &[
                ("", "Use app settings"),
                ("codex", "Codex"),
                ("claude", "Claude"),
                ("pi", "Pi"),
            ],
            &self.model.settings.provider,
            |settings, value| settings.provider = value.into(),
            cx,
        );
        let isolation = self.choice(
            "project-isolation",
            &[
                ("current", "Current checkout"),
                ("worktree", "New worktree"),
            ],
            if self.model.settings.isolated {
                "worktree"
            } else {
                "current"
            },
            |settings, value| settings.isolated = value == "worktree",
            cx,
        );
        [icon, provider, isolation]
    }

    fn row(label: &'static str, control: impl IntoElement) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_4()
            .min_h(rems(2.5))
            .child(div().w(rems(10.0)).flex_shrink_0().text_sm().child(label))
            .child(div().flex_1().min_w_0().child(control))
    }
}

impl EventEmitter<gpui_kit::DismissEvent> for ProjectSettingsView {}
impl Render for ProjectSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let [icon, provider, isolation] = self.project_choices(cx);
        div()
            .id("project-settings-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(rems(44.0))
                    .mx_auto()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(div().text_sm().child("Project settings"))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(self.model.name().to_owned()),
                    )
                    .child(Self::row(
                        "Name",
                        Input::new(&self.name)
                            .disabled(self.saving)
                            .aria_label("Project name"),
                    ))
                    .child(Self::row("Icon", icon))
                    .child(Self::row("Custom image", self.image_control(cx)))
                    .child(Self::row("Default agent", provider))
                    .child(Self::row("New sessions", isolation))
                    .child(Self::row(
                        "Branch prefix",
                        Input::new(&self.branch_prefix)
                            .disabled(self.saving)
                            .aria_label("Branch prefix"),
                    ))
                    .child(Self::row(
                        "Starting ref",
                        Input::new(&self.start_ref)
                            .disabled(self.saving)
                            .aria_label("Starting ref"),
                    ))
                    .when_some(self.model.error.as_ref(), |page, error| {
                        page.child(
                            div()
                                .text_sm()
                                .text_color(
                                    gpui_kit::component::Theme::global(cx)
                                        .semantic_tokens()
                                        .colors
                                        .destructive,
                                )
                                .child(error.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("project-save")
                                    .primary()
                                    .label("Save")
                                    .loading(self.saving)
                                    .disabled(self.saving)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.save(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new("project-back")
                                    .ghost()
                                    .label("Back to settings")
                                    .disabled(self.saving)
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(gpui_kit::DismissEvent)),
                                    ),
                            ),
                    ),
            )
    }
}
