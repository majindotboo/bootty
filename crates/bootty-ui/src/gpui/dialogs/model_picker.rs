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
            self.control_focus
                .insert("provider", self.provider_focus.clone());
            return;
        };
        let Some(provider_field) = spec.fields.iter().find(|field| field.id == "provider") else {
            self.model_picker = None;
            self.control_focus
                .insert("provider", self.provider_focus.clone());
            return;
        };
        let provider = provider_field.value.clone();
        let Some(kind) = provider_kind(&provider) else {
            self.model_picker = None;
            self.control_focus
                .insert("provider", self.provider_focus.clone());
            return;
        };
        let providers = match &provider_field.kind {
            DialogFieldKind::Choice(providers) => providers.clone(),
            DialogFieldKind::Text | DialogFieldKind::Color => vec![provider],
        };
        let selected = spec.selected_model.clone().or_else(|| {
            spec.models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.id.clone())
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
        let subscription = cx.subscribe_in(
            &state,
            window,
            |this, _, event: &ModelPickerEvent, _, cx| {
                let Some(spec) = &this.spec else {
                    return;
                };
                let (field, value) = match event {
                    ModelPickerEvent::Select(id) => ("model", id),
                    ModelPickerEvent::SelectProvider(provider) => ("provider", provider),
                    ModelPickerEvent::Favorite(id) => ("model-favorite", id),
                };
                cx.emit(DialogIntent::FieldChanged {
                    dialog: spec.id.clone(),
                    field: field.into(),
                    value: value.clone(),
                });
            },
        );
        self.control_focus
            .insert("provider", state.focus_handle(cx));
        self.model_picker = Some(NewModelPicker {
            state,
            _subscription: subscription,
        });
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

    pub(super) fn render_new_model_picker(
        &self,
        _: &DialogSpec,
        _: &DialogField,
        _: &Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        Some(self.model_picker.as_ref()?.state.clone().into_any_element())
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
