//! Live provider choices for the next conversation turn.
use super::*;
use crate::gpui::{ModelPickerEvent, ModelPickerView};
use bootty_agents::NativeModelSelection;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};

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
    pub(super) fn load_models(&mut self, window: &Window, cx: &Context<Self>) {
        if !self.provider_enabled()
            || self.record.snapshot.status != NativeSessionStatus::Idle
            || self.models.is_some()
        {
            return;
        }
        self.models = Some(Vec::new());
        self.model_command("models", Vec::new(), window, cx);
    }

    fn model_command(
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
                    Ok(CommandOutcome::Success { value, .. })
                        if matches!(operation.as_str(), "models" | "favorite") =>
                    {
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

    fn select_model(
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
            && self.record.snapshot.status == NativeSessionStatus::Idle;
        if let Some(picker) = &self.model_picker {
            picker.update(cx, |picker, cx| {
                picker.set_models(models, selected.as_deref(), enabled, window, cx);
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
        let subscription = cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &ModelPickerEvent, window, cx| match event {
                ModelPickerEvent::Favorite(id) => {
                    this.model_command("favorite", vec![id.clone()], window, cx);
                }
                ModelPickerEvent::Select(id) => {
                    if let Some(model) = this.models.iter().flatten().find(|model| &model.id == id)
                    {
                        let effort = this
                            .record
                            .config
                            .reasoning_effort
                            .as_ref()
                            .filter(|effort| model.reasoning_efforts.contains(effort))
                            .cloned()
                            .or_else(|| model.default_reasoning_effort.clone());
                        this.select_model(
                            &NativeModelSelection {
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
            },
        );
        self.model_picker_subscription = Some(subscription);
        self.model_picker = Some(picker);
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
            || self.record.snapshot.status != NativeSessionStatus::Idle;
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
                        .disabled(self.pending.contains("models") || busy)
                        .loading(self.pending.contains("models"))
                        .accessibility_label(if self.pending.contains("models") {
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
            .when_some(
                current.filter(|model| !model.reasoning_efforts.is_empty()),
                |row, model| {
                    row.child(div().w_px().h_4().bg(cx.theme().border)).child(
                        div()
                            .track_focus(&self.effort_focus)
                            .child(self.render_reasoning_control(model, busy, cx)),
                    )
                },
            )
    }

    pub(super) fn render_permission_control(&self, cx: &Context<Self>) -> impl IntoElement {
        let mode = self.record.config.permissions;
        let provider = self.record.config.provider;
        let label = if mode == bootty_agents::NativePermissionMode::ProviderDefault {
            "Configured permissions"
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
            .tooltip("Permissions for the next turn. Existing approval requests keep their captured permissions.")
            .loading(self.pending.contains("permissions") || self.resuming())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(crate::gpui::sized_icon(
                        mode.icon(),
                        crate::gpui::IconSize::Small,
                        cx.theme().foreground,
                    ))
                    .child(label),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for choice in
                    bootty_agents::NativePermissionMode::ALL
                        .into_iter()
                        .filter(|choice| {
                            *choice != bootty_agents::NativePermissionMode::ProviderDefault
                                && choice.supports(provider)
                        })
                {
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(choice.label())
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
    fn render_reasoning_control(
        &self,
        model: &bootty_agents::NativeModelOption,
        busy: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let model_id = model.id.clone();
        let efforts = model.reasoning_efforts.clone();
        let selected = self
            .record
            .config
            .reasoning_effort
            .clone()
            .or_else(|| model.default_reasoning_effort.clone());
        let owner = cx.weak_entity();
        let default_effort = model.default_reasoning_effort.clone();
        Button::new("native-reasoning")
            .ghost()
            .small()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(crate::gpui::sized_icon(
                        "brain",
                        crate::gpui::IconSize::Small,
                        cx.theme().foreground,
                    ))
                    .child(
                        selected
                            .as_deref()
                            .map_or_else(|| "Provider setting".into(), reasoning_label),
                    ),
            )
            .dropdown_caret(true)
            .accessibility_label("Reasoning for next message")
            .disabled(busy)
            .dropdown_menu(move |mut menu, window, _| {
                menu = menu
                    .min_w(gpui_kit::rems(12.).to_pixels(window.rem_size()))
                    .label("Reasoning");
                for effort in &efforts {
                    let selection = NativeModelSelection {
                        model: model_id.clone(),
                        reasoning_effort: Some(effort.clone()),
                    };
                    let owner = owner.clone();
                    let label = reasoning_label(effort);
                    let is_default = default_effort.as_ref() == Some(effort);
                    menu = menu.item(
                        PopupMenuItem::element(move |_, cx| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(label.clone())
                                .when(is_default, |row| {
                                    row.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().foreground.opacity(0.65))
                                            .child("Default"),
                                    )
                                })
                        })
                        .checked(selected.as_ref() == Some(effort))
                        .on_click(move |_, window, cx| {
                            _ = owner.update(cx, |this, cx| {
                                this.select_model(&selection, window, cx);
                            });
                        }),
                    );
                }
                menu
            })
    }
}
