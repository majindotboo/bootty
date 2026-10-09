//! Both Agent composers use the same retained catalog picker.
use super::*;
use crate::gpui::{ModelPickerEvent, ModelPickerView};

pub(super) struct NewModelPicker {
    pub(super) state: Entity<ModelPickerView>,
    provider: String,
    _subscription: Subscription,
}

impl DialogView {
    pub(super) fn sync_new_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = &self.spec else {
            return;
        };
        if !spec.fields.iter().any(|field| field.id == "model") {
            self.model_picker = None;
            return;
        }
        let provider = spec
            .fields
            .iter()
            .find(|field| field.id == "provider")
            .map_or_else(String::new, |field| field.value.clone());
        let kind = match provider.as_str() {
            "Codex" => bootty_agents::AgentKind::Codex,
            "Claude" => bootty_agents::AgentKind::Claude,
            "Pi" => bootty_agents::AgentKind::Pi,
            _ => return,
        };
        let selected = spec.selected_model.clone().or_else(|| {
            spec.models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.id.clone())
        });
        if let Some(picker) = &self.model_picker
            && picker.provider == provider
        {
            picker.state.update(cx, |picker, cx| {
                picker.set_models(&spec.models, selected.as_deref(), !spec.busy, window, cx);
            });
            return;
        }
        let state = cx.new(|cx| {
            ModelPickerView::new(kind, spec.models.clone(), selected.as_deref(), window, cx)
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
                    ModelPickerEvent::Favorite(id) => ("model-favorite", id),
                };
                cx.emit(DialogIntent::FieldChanged {
                    dialog: spec.id.clone(),
                    field: field.into(),
                    value: value.clone(),
                });
            },
        );
        self.model_picker = Some(NewModelPicker {
            state,
            provider,
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
