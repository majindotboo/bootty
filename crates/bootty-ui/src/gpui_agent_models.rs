//! Live provider choices for the next conversation turn.
use super::*;
use crate::gpui::{ModelPickerEvent, ModelPickerView};
use bootty_agents::NativeModelSelection;
use gpui_kit::component::menu::PopupMenuItem;

pub(super) type ModelPickerState = ModelPickerView;

pub fn reasoning_label(value: &str) -> String {
    match value {
        "off" | "none" => "Off".into(),
        "minimal" => "Minimal".into(),
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" => "Extra High".into(),
        "max" => "Max".into(),
        "ultra" => "Ultra".into(),
        _ => value.into(),
    }
}

impl NativeAgentSessionView {
    pub(crate) fn set_available_efforts(&mut self, efforts: &[String], cx: &mut Context<Self>) {
        if self.available_efforts != efforts {
            self.available_efforts = efforts.to_vec();
            if let Some(picker) = &self.model_picker {
                picker.update(cx, |picker, cx| picker.set_available_efforts(efforts, cx));
            }
        }
    }
    pub(crate) fn set_cached_catalog(
        &mut self,
        catalog: bootty_agents::NativeProviderCatalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.models = Some(catalog.models);
        self.provider_permissions = catalog.permissions;
        self.sync_model_picker(window, cx);
        if self.provider_enabled()
            && matches!(
                self.record.snapshot.status,
                NativeSessionStatus::Idle
                    | NativeSessionStatus::Working
                    | NativeSessionStatus::Waiting
            )
        {
            self.model_command("models-info", Vec::new(), window, cx);
        }
    }

    pub(super) fn load_models(&mut self, window: &Window, cx: &Context<Self>) {
        if !self.provider_enabled()
            || !matches!(
                self.record.snapshot.status,
                NativeSessionStatus::Idle
                    | NativeSessionStatus::Working
                    | NativeSessionStatus::Waiting
            )
            || self.models.is_some()
        {
            return;
        }
        self.models = Some(Vec::new());
        self.model_command("models-info", Vec::new(), window, cx);
    }

    pub(super) fn model_command(
        &mut self,
        operation: &str,
        arguments: Vec<String>,
        window: &Window,
        cx: &Context<Self>,
    ) {
        if self.pending.contains(operation) {
            return;
        }
        let target = self.record.target();
        let mut invocation = CommandInvocation::new(
            format!("agents.native.{operation}"),
            vec![self.record.id.clone(), self.record.generation.to_string()],
            Caller::Internal,
        );
        invocation.target = Some(target.clone());
        invocation.arguments.extend(arguments);
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.error = Some(match error {
                    bootty_control::AppCommandSendError::Overloaded => {
                        "The application command queue is full".into()
                    }
                    bootty_control::AppCommandSendError::Shutdown => {
                        "The application is shutting down".into()
                    }
                });
                return;
            }
        };
        let operation = operation.to_owned();
        self.pending.insert(operation.clone());
        cx.spawn_in(window, async move |owner, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = owner.update_in(cx, |this, window, cx| {
                this.pending.remove(&operation);
                if this.record.target() != target {
                    return;
                }
                match result {
                    Ok(CommandOutcome::Success { value, .. }) if operation == "models-info" => {
                        match serde_json::from_value::<bootty_agents::NativeProviderCatalog>(value)
                        {
                            Ok(catalog) => {
                                this.models = Some(catalog.models);
                                this.provider_permissions = catalog.permissions;
                                this.error = None;
                            }
                            Err(error) => {
                                this.error = Some(format!("Invalid provider catalog: {error}"));
                            }
                        }
                    }
                    Ok(CommandOutcome::Success { value, .. }) if operation == "favorite" => {
                        match serde_json::from_value::<Vec<bootty_agents::NativeModelOption>>(value)
                        {
                            Ok(models) if !models.is_empty() => {
                                this.models = Some(models);
                                this.error = None;
                            }
                            Ok(_) => {
                                this.error = Some(
                                    "No models are available for this provider account".into(),
                                );
                            }
                            Err(error) => {
                                this.error =
                                    Some(format!("Invalid provider model catalog: {error}"));
                            }
                        }
                    }
                    Ok(CommandOutcome::Success { .. }) if operation == "goal" => this.error = None,
                    Ok(CommandOutcome::Success { value, .. }) => {
                        if operation == "permissions" {
                            if let Ok(record) = serde_json::from_value(value) {
                                this.record = record;
                                this.error = None;
                            }
                        } else if let Ok(config) = serde_json::from_value(value) {
                            this.record.config = config;
                            this.error = None;
                        }
                    }
                    Ok(outcome) => {
                        // Provider catalogs can change while a saved picker is open.
                        if matches!(operation.as_str(), "configure" | "favorite") {
                            this.models = None;
                        }
                        this.error = crate::commands::command_outcome_message(&outcome);
                    }
                    Err(_) => this.error = Some("Model command stopped".to_owned()),
                }
                this.sync_model_picker(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_model(
        &mut self,
        selection: &NativeModelSelection,
        window: &Window,
        cx: &Context<Self>,
    ) {
        if let Ok(encoded) = serde_json::to_string(selection) {
            self.model_command("configure", vec![encoded], window, cx);
        }
    }

    pub(super) fn sync_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(models) = &self.models else {
            return;
        };
        if models.is_empty() {
            return;
        }
        let selected = models
            .iter()
            .find(|model| self.record.config.model.as_deref() == Some(&model.id))
            .or_else(|| models.iter().find(|model| model.is_default))
            .map(|model| model.id.clone());
        let enabled = !self.pending.contains("configure")
            && !self.pending.contains("favorite")
            && matches!(
                self.record.snapshot.status,
                NativeSessionStatus::Idle
                    | NativeSessionStatus::Working
                    | NativeSessionStatus::Waiting
            );
        if let Some(picker) = &self.model_picker {
            picker.update(cx, |picker, cx| {
                picker.set_models(models, selected.as_deref(), enabled, window, cx);
                picker.set_reasoning_effort(self.record.config.reasoning_effort.as_deref(), cx);
                picker.set_fast_mode(self.record.config.fast_mode, cx);
                picker.set_available_efforts(&self.available_efforts, cx);
            });
            return;
        }
        let picker = cx.new(|cx| {
            ModelPickerView::new(
                self.record.config.provider,
                models.clone(),
                selected.as_deref(),
                window,
                cx,
            )
        });
        picker.update(cx, |picker, cx| {
            picker.set_reasoning_effort(self.record.config.reasoning_effort.as_deref(), cx);
            picker.set_fast_mode(self.record.config.fast_mode, cx);
            picker.set_available_efforts(&self.available_efforts, cx);
        });
        let subscription = cx.subscribe_in(&picker, window, |this, _, event, window, cx| {
            this.model_picker_changed(event, window, cx);
        });
        self.model_picker_subscription = Some(subscription);
        self.model_picker = Some(picker);
    }

    fn model_picker_changed(
        &mut self,
        event: &ModelPickerEvent,
        window: &Window,
        cx: &Context<Self>,
    ) {
        match event {
            ModelPickerEvent::Reasoning(selection) => self.select_model(selection, window, cx),
            ModelPickerEvent::FastMode(fast) => {
                if let Some(model) = self.models.iter().flatten().find(|model| {
                    self.record
                        .config
                        .model
                        .as_ref()
                        .map_or(model.is_default, |selected| selected == &model.id)
                }) {
                    self.select_model(
                        &NativeModelSelection {
                            plan_mode: None,
                            model: model.id.clone(),
                            reasoning_effort: self
                                .record
                                .config
                                .reasoning_effort
                                .clone()
                                .or_else(|| model.default_reasoning_effort.clone()),
                            fast_mode: Some(*fast),
                        },
                        window,
                        cx,
                    );
                }
            }
            ModelPickerEvent::Favorite(id) => {
                self.model_command("favorite", vec![id.clone()], window, cx);
            }
            ModelPickerEvent::Select(id) => {
                if let Some(model) = self.models.iter().flatten().find(|model| &model.id == id) {
                    let effort = self
                        .record
                        .config
                        .reasoning_effort
                        .as_ref()
                        .filter(|effort| {
                            model.reasoning_efforts.contains(effort)
                                && (self.available_efforts.is_empty()
                                    || self.available_efforts.contains(effort))
                        })
                        .cloned()
                        .or_else(|| {
                            model.default_reasoning_effort.clone().filter(|effort| {
                                self.available_efforts.is_empty()
                                    || self.available_efforts.contains(effort)
                            })
                        })
                        .or_else(|| {
                            model
                                .reasoning_efforts
                                .iter()
                                .find(|effort| {
                                    self.available_efforts.is_empty()
                                        || self.available_efforts.contains(effort)
                                })
                                .cloned()
                        });
                    self.select_model(
                        &NativeModelSelection {
                            plan_mode: None,
                            fast_mode: None,
                            model: id.clone(),
                            reasoning_effort: effort,
                        },
                        window,
                        cx,
                    );
                }
            }
            // Existing conversations keep their provider fixed; only the new-session
            // composer supplies provider choices to the shared picker.
            ModelPickerEvent::SelectProvider(_) => {}
        }
    }

    pub(super) fn render_model_controls(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let current = self
            .models
            .iter()
            .flatten()
            .find(|model| self.record.config.model.as_deref() == Some(&model.id))
            .or_else(|| self.models.iter().flatten().find(|model| model.is_default));
        let label = current.map_or_else(
            || {
                self.record
                    .config
                    .model
                    .clone()
                    .unwrap_or_else(|| provider_name(self.record.config.provider).to_owned())
            },
            |model| model.display_name.clone(),
        );
        let provider = self.record.config.provider;
        let busy = self.pending.contains("configure")
            || !matches!(
                self.record.snapshot.status,
                NativeSessionStatus::Idle
                    | NativeSessionStatus::Working
                    | NativeSessionStatus::Waiting
            );
        let trigger = move |cx: &App| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(crate::gpui::sized_icon(
                    provider.icon(),
                    crate::gpui::IconSize::Small,
                    crate::gpui::provider_color(&provider.to_string(), cx),
                ))
                .child(div().min_w_0().text_ellipsis().child(label.clone()))
        };
        div()
            .flex()
            .flex_shrink_0()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(self.model_picker.as_ref().map_or_else(
                || {
                    Button::new("native-model")
                        .ghost()
                        .small()
                        .disabled(self.pending.contains("models-info") || busy)
                        .loading(self.pending.contains("models-info"))
                        .accessibility_label(if self.pending.contains("models-info") {
                            "Loading models"
                        } else if self.error.is_some() {
                            "Retry loading models"
                        } else {
                            "Model for next message"
                        })
                        .child(trigger(cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.models = None;
                            this.error = None;
                            cx.notify();
                        }))
                        .into_any_element()
                },
                |picker| picker.clone().into_any_element(),
            ))
    }

    pub(super) fn render_permission_control(&self, cx: &Context<Self>) -> impl IntoElement {
        let mode = if self.record.config.permissions
            == bootty_agents::NativePermissionMode::ProviderDefault
        {
            self.provider_permissions
                .unwrap_or(bootty_agents::NativePermissionMode::ProviderDefault)
        } else {
            self.record.config.permissions
        };
        let provider = self.record.config.provider;
        let label = if mode == bootty_agents::NativePermissionMode::ProviderDefault {
            "Policy unavailable"
        } else {
            mode.label()
        };
        let owner = cx.entity().downgrade();
        Button::new("native-permissions")
            .ghost()
            .small()
            .dropdown_caret(true)
            .accessibility_label(format!("Permissions: {label}"))
            .disabled(
                self.pending.contains("permissions") || self.resuming() || !self.provider_enabled(),
            )
            .loading(self.pending.contains("permissions") || self.resuming())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(crate::gpui::sized_icon(
                        mode.icon(),
                        crate::gpui::IconSize::Small,
                        if mode == bootty_agents::NativePermissionMode::FullAccess {
                            cx.theme().warning
                        } else {
                            cx.theme().foreground
                        },
                    ))
                    .child(
                        div()
                            .text_color(
                                if mode == bootty_agents::NativePermissionMode::FullAccess {
                                    cx.theme().warning
                                } else {
                                    cx.theme().foreground
                                },
                            )
                            .child(label),
                    ),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for &choice in bootty_agents::NativePermissionMode::choices(provider) {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| permission_choice_content(choice, cx))
                            .checked(choice == mode)
                            .on_click(move |_, window, cx| {
                                _ = owner.update(cx, |this, cx| {
                                    this.model_command(
                                        "permissions",
                                        vec![choice.id().to_owned()],
                                        window,
                                        cx,
                                    );
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
    }
}

/// Both composers present the same provider permission choice.
pub fn permission_choice_content(
    choice: bootty_agents::NativePermissionMode,
    cx: &App,
) -> gpui_kit::AnyElement {
    let color = if choice == bootty_agents::NativePermissionMode::FullAccess {
        cx.theme().warning
    } else {
        cx.theme().foreground
    };
    div()
        .id(gpui_kit::SharedString::from(format!(
            "permission-choice:{}",
            choice.id()
        )))
        .role(gpui_kit::Role::MenuItem)
        .aria_label(format!("{}: {}", choice.label(), choice.description()))
        .flex()
        .items_start()
        .gap_2()
        .child(crate::gpui::sized_icon(
            choice.icon(),
            crate::gpui::IconSize::Small,
            color,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(color).child(choice.label()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(choice.description()),
                ),
        )
        .into_any_element()
}
