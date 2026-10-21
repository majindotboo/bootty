//! Project navigation and tab lifecycle in the settings window.
use super::*;
use crate::gpui_workspace::project_settings::ProjectSettingsEvent;
use bootty_control::{Caller, CommandInvocation, CommandTarget, ResourceKind};
use bootty_mux::{controller::SpaceId, workspace::SpaceSummary};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::{AnyElement, SharedString, rems};

/// One Space as the picker shows it.
fn space_choice(icon: &str, name: &str, cx: &gpui_kit::App) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(crate::gpui::sized_icon(
            icon,
            crate::gpui::IconSize::Small,
            cx.theme().foreground,
        ))
        .child(name.to_owned())
}

/// The binding target of one Space, resolved the same way as the active Space's.
fn space_binding_target(state: &crate::state::AppState, scope: SpaceId) -> Option<CommandTarget> {
    let mux = state.workspace.binding(scope)?.mux();
    let handle = state.binding_target_handle(scope, mux.binding_generation());
    crate::commands::ExactMuxTarget::Binding(scope).command_target(
        ResourceKind::Binding,
        mux,
        &handle,
    )
}

impl GpuiSettingsWindow {
    pub(super) fn render_projects(&self, cx: &Context<Self>) -> AnyElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return div()
                .child("The workspace is unavailable.")
                .into_any_element();
        };
        let state = &workspace.read(cx).state;
        let spaces = state.space_summaries();
        let scope = self.project_space.unwrap_or_else(|| state.mux_scope());
        let space = spaces.iter().find(|space| space.id == scope);
        let target = space.and_then(|_| space_binding_target(state, scope));
        let unavailable = match space {
            None => Some("This Space was removed. Choose another Space.".to_owned()),
            Some(space) if target.is_none() => Some(
                space
                    .error
                    .clone()
                    .unwrap_or_else(|| "This Space's host is unavailable.".to_owned()),
            ),
            Some(_) => None,
        };
        let projects: Vec<_> = state
            .workspace
            .registered_projects(scope)
            .cloned()
            .collect();
        let add_target = target.clone();
        let mut page = div()
            .w_full()
            .max_w(rems(48.0))
            .mx_auto()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Projects"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Self::project_space_picker(&spaces, space, cx))
                            .child(
                                Button::new("settings-add-project")
                                    .outline()
                                    .label("Add project…")
                                    .disabled(target.is_none())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(target) = &add_target {
                                            _ = this.workspace.update(cx, |_, cx| {
                                                GpuiWorkspace::choose_project_directory(
                                                    target.clone(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    })),
                            ),
                    ),
            );
        if let Some(message) = unavailable {
            page = page.child(
                div()
                    .py_6()
                    .text_color(cx.theme().muted_foreground)
                    .child(message),
            );
        } else if projects.is_empty() {
            page = page.child(div().py_6().child("No projects"));
        }
        for project in projects {
            page = page.child(Self::project_row(&project, target.as_ref(), cx));
        }
        page = page.when_some(self.project_error.as_ref(), |page, error| {
            page.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().semantic_tokens().colors.destructive)
                    .child(error.clone()),
            )
        });
        div()
            .id("settings-projects-page")
            .size_full()
            .overflow_y_scroll()
            .child(page)
            .into_any_element()
    }

    /// Chooses the Space whose projects the page browses. It never switches the workspace.
    fn project_space_picker(
        spaces: &[SpaceSummary],
        selected: Option<&SpaceSummary>,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let (icon, name) = selected.map_or(("", "Unavailable Space"), |space| {
            (space.icon.as_str(), space.name.as_str())
        });
        let choices: Vec<_> = spaces
            .iter()
            .map(|space| (space.id, space.icon.clone(), space.name.clone()))
            .collect();
        let selected = selected.map(|space| space.id);
        let owner = cx.weak_entity();
        Button::new("settings-projects-space")
            .outline()
            .accessibility_label(format!("Space: {name}"))
            .child(space_choice(icon, name, cx))
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for (id, icon, name) in &choices {
                    let (id, icon, name) = (*id, icon.clone(), name.clone());
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            div()
                                .id(SharedString::from(format!(
                                    "settings-projects-space-{}",
                                    id.persistence_value()
                                )))
                                .role(gpui_kit::Role::MenuItem)
                                .aria_label(name.clone())
                                .child(space_choice(&icon, &name, cx))
                        })
                        .checked(selected == Some(id))
                        .on_click(move |_, _, cx| {
                            _ = owner.update(cx, |this, cx| {
                                this.project_space = Some(id);
                                this.project_error = None;
                                cx.notify();
                            });
                        }),
                    );
                }
                menu
            })
    }

    fn project_row(
        project: &bootty_mux::repository::RegisteredProject,
        target: Option<&CommandTarget>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let name = if project.settings.name.is_empty() {
            project
                .cwd
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&project.cwd)
                .to_owned()
        } else {
            project.settings.name.clone()
        };
        let icon = if project.settings.icon.is_empty() {
            "folder"
        } else {
            &project.settings.icon
        };
        let cwd = project.cwd.clone();
        let edit_cwd = cwd.clone();
        let remove_cwd = cwd.clone();
        let edit_target = target.cloned();
        let remove_target = target.cloned();
        let row = div()
            .id(SharedString::from(format!("settings-project-{cwd}")))
            .flex()
            .items_center()
            .gap_3()
            .py_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(crate::gpui::sized_icon(
                icon,
                crate::gpui::IconSize::Medium,
                cx.theme().muted_foreground,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(bootty_git::project::display_path(
                                &cwd,
                                crate::strings::home_dir().as_deref(),
                            )),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!("edit-project-{cwd}")))
                    .outline()
                    .small()
                    .label("Settings")
                    .accessibility_label(format!("Settings for {name}"))
                    .disabled(target.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(target) = &edit_target {
                            this.project_command("project.edit", &edit_cwd, target, cx);
                        }
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("remove-project-{cwd}")))
                    .ghost()
                    .small()
                    .label("Remove")
                    .accessibility_label(format!("Remove {name}"))
                    .disabled(target.is_none())
                    .on_click(cx.listener(move |_, _, window, cx| {
                        if let Some(target) = &remove_target {
                            Self::request_remove_project(
                                remove_cwd.clone(),
                                target.clone(),
                                window,
                                cx,
                            );
                        }
                    })),
            );
        row.into_any_element()
    }

    fn project_command(
        &mut self,
        command: &str,
        cwd: &str,
        target: &CommandTarget,
        cx: &mut Context<Self>,
    ) {
        if command == "project.remove"
            && self.projects.iter().any(|tab| {
                tab.view.read(cx).cwd() == cwd
                    && tab.view.read(cx).target() == target
                    && tab.view.read(cx).saving()
            })
        {
            self.project_error = Some("Wait for project settings to finish saving.".into());
            cx.notify();
            return;
        }
        let mut invocation =
            CommandInvocation::new(command, vec![cwd.to_owned()], Caller::Internal);
        invocation.target = Some(target.clone());
        let result = self.workspace.update(cx, |workspace, cx| {
            let mut effects = Vec::new();
            let result = workspace.state.dispatch_command(
                invocation,
                crate::state::ViewportSnapshot::default(),
                &mut effects,
            );
            workspace.pending_effects.extend(effects);
            cx.notify();
            result
        });
        if command == "project.remove"
            && matches!(&result, Ok(bootty_control::CommandOutcome::Success { .. }))
        {
            self.projects.retain(|tab| {
                tab.view.read(cx).cwd() != cwd || tab.view.read(cx).target() != target
            });
            if matches!(self.active_tab, SettingsWindowTab::Project(id) if !self.projects.iter().any(|tab| tab.view.entity_id() == id))
            {
                self.active_tab = SettingsWindowTab::Projects;
            }
        }
        self.project_error = result.map_or_else(
            |_| Some("The workspace is unavailable. Reopen Settings to try again.".into()),
            |outcome| {
                crate::commands::command_outcome_message(&outcome)
                    .map(|message| bootty_agents::native_error_message(&message))
            },
        );
        // A removal asked for from a project tab reports there, where the user is looking.
        if command == "project.remove"
            && let Some(error) = self.project_error.clone()
        {
            for tab in &self.projects {
                if tab.view.read(cx).cwd() == cwd && tab.view.read(cx).target() == target {
                    let error = error.clone();
                    tab.view.update(cx, |view, cx| view.set_error(error, cx));
                }
            }
        }
        cx.notify();
    }

    /// A project tab asks to remove its project from the Space the tab was opened for.
    pub(super) fn on_project_settings_event(
        view: &Entity<crate::gpui_workspace::project_settings::ProjectSettingsView>,
        event: &ProjectSettingsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ProjectSettingsEvent::Remove => {
                let (cwd, target) = {
                    let view = view.read(cx);
                    (view.cwd().to_owned(), view.target().clone())
                };
                Self::request_remove_project(cwd, target, window, cx);
            }
        }
    }

    fn request_remove_project(
        cwd: String,
        target: CommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_cwd =
            bootty_git::project::display_path(&cwd, crate::strings::home_dir().as_deref());
        let answer = crate::gpui::prompt(
            "Remove project?",
            Some(&format!(
                "Remove {display_cwd} from registered projects? Files and existing sessions are kept. Any open settings draft for this project will close."
            )),
            &["Remove".into(), "Cancel".into()],
            window,
            cx,
        );
        cx.spawn_in(window, async move |owner, cx| {
            if answer.await == Ok(0) {
                _ = owner.update_in(cx, |this, _, cx| {
                    this.project_command("project.remove", &cwd, &target, cx);
                });
            }
        })
        .detach();
    }

    pub(super) fn render_project_tab(tab: &ProjectSettingsTab, cx: &Context<Self>) -> Tab {
        let id = tab.view.entity_id();
        let project = tab.view.read(cx);
        let name = project.name().to_owned();
        let label = if project.is_dirty(cx) {
            format!("{name} ●")
        } else {
            name.clone()
        };
        Tab::new()
            .label(label)
            .aria_label(format!("Project settings: {name}"))
            .suffix(
                Button::new(SharedString::from(format!(
                    "close-project-{}",
                    project.cwd()
                )))
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .accessibility_label(format!("Close {name} settings"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.request_close_project_tab(id, window, cx);
                })),
            )
    }

    pub(super) fn request_close_project_tab(
        &mut self,
        id: gpui_kit::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_project_close(id, false, window, cx);
    }

    pub(super) fn confirm_project_close(
        &mut self,
        id: gpui_kit::EntityId,
        close_window: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.projects.iter().find(|tab| tab.view.entity_id() == id) else {
            return;
        };
        let view = tab.view.read(cx);
        if view.saving() || self.close_prompt_pending {
            return;
        }
        if !view.is_dirty(cx) {
            self.close_project_tab(id, window, cx);
            if close_window {
                self.request_close(window, cx);
            }
            return;
        }
        let answer = crate::gpui::prompt(
            &format!("Discard changes to {}?", view.name()),
            Some("Your unsaved project settings will be lost."),
            &["Discard".into(), "Cancel".into()],
            window,
            cx,
        );
        self.close_prompt_pending = true;
        cx.spawn_in(window, async move |owner, cx| {
            let discard = matches!(answer.await, Ok(0));
            _ = owner.update_in(cx, |this, window, cx| {
                this.close_prompt_pending = false;
                if discard {
                    this.close_project_tab(id, window, cx);
                    if close_window {
                        this.request_close(window, cx);
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn close_project_tab(
        &mut self,
        id: gpui_kit::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.projects.retain(|tab| tab.view.entity_id() != id);
        if self.active_tab == SettingsWindowTab::Project(id) {
            self.active_tab = SettingsWindowTab::Projects;
            activate_settings_window(self, window, cx);
        }
        cx.notify();
    }
}
