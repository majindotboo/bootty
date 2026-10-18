//! Both Agent composers use the same retained catalog picker.
use super::*;
use crate::gpui::{ModelPickerEvent, ModelPickerView};

pub(super) struct NewModelPicker {
    pub(super) state: Entity<ModelPickerView>,
    _subscription: Subscription,
}

impl DialogView {
    pub(super) fn sync_new_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else {
            return;
        };
        let Some(provider_field) = spec.fields.iter().find(|field| field.id == "provider") else {
            self.model_picker = None;
            self.control_focus
                .insert("provider", self.provider_focus.clone());
            return;
        };
        let Some(kind) = provider_kind(&provider_field.value) else {
            self.model_picker = None;
            self.control_focus
                .insert("provider", self.provider_focus.clone());
            return;
        };
        let providers = match &provider_field.kind {
            DialogFieldKind::Choice(providers) => providers.clone(),
            DialogFieldKind::Text | DialogFieldKind::Color => vec![provider_field.value.clone()],
        };
        let selected = spec.selected_model.clone().or_else(|| {
            spec.models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.id.clone())
        });
        let effort = spec
            .fields
            .iter()
            .find(|field| field.id == "reasoning")
            .map(|field| field.value.as_str());
        let fast = spec
            .fields
            .iter()
            .any(|field| field.id == "fast-mode" && field.value == "true");
        // The models of this provider the prompt also goes to, when the form offers that.
        let fan_out = spec
            .fields
            .iter()
            .find(|field| field.id == "fan-out")
            .map(|field| {
                field
                    .value
                    .lines()
                    .filter_map(crate::presentation::new_session_form::FanOutModel::decode)
                    .filter(|chosen| chosen.provider == kind.to_string())
                    .map(|chosen| chosen.model)
                    .collect::<Vec<_>>()
            });
        if let Some(picker) = &self.model_picker {
            picker.state.update(cx, |picker, cx| {
                picker.set_provider_models(
                    crate::gpui_model_picker::ProviderModelCatalog {
                        provider: kind,
                        models: &spec.models,
                        current: selected.as_deref(),
                        providers: &providers,
                        enabled: !spec.busy,
                    },
                    window,
                    cx,
                );
                picker.set_reasoning_effort(effort, cx);
                picker.set_fast_mode(fast, cx);
                picker.set_available_efforts(&spec.available_efforts, cx);
                picker.set_fan_out(fan_out, cx);
            });
            self.control_focus
                .insert("provider", picker.state.focus_handle(cx));
            return;
        }
        let state = cx.new(|cx| {
            ModelPickerView::new_with_providers(
                kind,
                spec.models.clone(),
                selected.as_deref(),
                providers,
                window,
                cx,
            )
        });
        state.update(cx, |picker, cx| {
            picker.set_reasoning_effort(effort, cx);
            picker.set_fast_mode(fast, cx);
            picker.set_available_efforts(&spec.available_efforts, cx);
            picker.set_fan_out(fan_out, cx);
        });
        let subscription = cx.subscribe_in(&state, window, |this, _, event, window, cx| {
            this.new_model_picker_changed(event, window, cx);
        });
        self.control_focus
            .insert("provider", state.focus_handle(cx));
        self.model_picker = Some(NewModelPicker {
            state,
            _subscription: subscription,
        });
    }

    fn new_model_picker_changed(
        &self,
        event: &ModelPickerEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(spec) = &self.spec else {
            return;
        };
        let (field, value) = match event {
            ModelPickerEvent::FastMode(fast) => {
                cx.emit(DialogIntent::FieldChanged {
                    dialog: spec.id.clone(),
                    field: "fast-mode".into(),
                    value: fast.to_string(),
                });
                return;
            }
            ModelPickerEvent::Reasoning(selection) => {
                if let Some(effort) = &selection.reasoning_effort {
                    cx.emit(DialogIntent::FieldChanged {
                        dialog: spec.id.clone(),
                        field: "reasoning".into(),
                        value: effort.clone(),
                    });
                }
                return;
            }
            ModelPickerEvent::FanOut(id) => {
                let provider = spec
                    .fields
                    .iter()
                    .find(|field| field.id == "provider")
                    .and_then(|field| provider_kind(&field.value))
                    .map_or_else(String::new, |kind| kind.to_string());
                let name = spec
                    .models
                    .iter()
                    .find(|model| &model.id == id)
                    .map_or_else(|| id.clone(), |model| model.display_name.clone());
                cx.emit(DialogIntent::FieldChanged {
                    dialog: spec.id.clone(),
                    field: "fan-out".into(),
                    value: crate::presentation::new_session_form::FanOutModel {
                        provider,
                        model: id.clone(),
                        name,
                    }
                    .encode(),
                });
                return;
            }
            ModelPickerEvent::Select(id) => ("model", id),
            ModelPickerEvent::SelectProvider(provider) => ("provider", provider),
            ModelPickerEvent::Favorite(id) => ("model-favorite", id),
        };
        cx.emit(DialogIntent::FieldChanged {
            dialog: spec.id.clone(),
            field: field.into(),
            value: value.clone(),
        });
        if matches!(event, ModelPickerEvent::Select(_)) {
            cx.defer_in(window, |this, window, cx| {
                this.prompt_textarea
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx);
            });
        }
    }

    pub(super) fn new_model_error(
        spec: &DialogSpec,
        cx: &Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let message = spec.model_error.clone()?;
        let dialog = spec.id.clone();
        Some(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
                .child(
                    Button::new("reload-models")
                        .debug_selector(|| "new-session-model-retry".to_owned())
                        .ghost()
                        .small()
                        .label("Retry")
                        .accessibility_label("Retry loading models")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(DialogIntent::Activate {
                                dialog: dialog.clone(),
                                row: RowId::new("reload-models"),
                                action: ActionId::new("reload-models"),
                                payload: DialogPayload::default(),
                            });
                        })),
                )
                .into_any_element(),
        )
    }

    /// The model picker, then a chip for each other model the prompt also goes to, which
    /// stops sending it there.
    pub(super) fn render_new_model_picker(
        &self,
        spec: &DialogSpec,
        _: &DialogField,
        cx: &Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let picker = self.model_picker.as_ref()?.state.clone();
        let chosen = spec
            .fields
            .iter()
            .find(|field| field.id == "fan-out")
            .map_or_else(Vec::new, |field| {
                field
                    .value
                    .lines()
                    .filter_map(crate::presentation::new_session_form::FanOutModel::decode)
                    .collect::<Vec<_>>()
            });
        if chosen.is_empty() {
            return Some(picker.into_any_element());
        }
        let dialog = spec.id.clone();
        Some(
            h_flex()
                .min_w_0()
                .gap_1()
                .child(picker)
                .children(chosen.into_iter().map(|model| {
                    let (owner, dialog) = (cx.weak_entity(), dialog.clone());
                    let label = format!("Don't send to {} too", model.name);
                    Button::new(SharedString::from(format!(
                        "fan-out-chip:{}:{}",
                        model.provider, model.model
                    )))
                    .debug_selector({
                        let id = format!("fan-out-chip:{}", model.model);
                        move || id
                    })
                    .ghost()
                    .xsmall()
                    .child(crate::gpui::sized_icon(
                        provider_kind(&model.provider)
                            .map_or("bot", bootty_agents::AgentKind::icon),
                        crate::gpui::IconSize::XSmall,
                        crate::gpui::provider_color(&model.provider, cx),
                    ))
                    .child(format!("+ {}", model.name))
                    .accessibility_label(label.clone())
                    .element_tooltip(label)
                    .on_click(move |_, _, cx| {
                        _ = owner.update(cx, |_, cx| {
                            cx.emit(DialogIntent::FieldChanged {
                                dialog: dialog.clone(),
                                field: "fan-out".into(),
                                value: model.encode(),
                            });
                        });
                    })
                }))
                .into_any_element(),
        )
    }
}

fn provider_kind(provider: &str) -> Option<bootty_agents::AgentKind> {
    match provider.to_ascii_lowercase().as_str() {
        "codex" => Some(bootty_agents::AgentKind::Codex),
        "claude" => Some(bootty_agents::AgentKind::Claude),
        "pi" => Some(bootty_agents::AgentKind::Pi),
        _ => None,
    }
}
