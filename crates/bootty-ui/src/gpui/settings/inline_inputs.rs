//! Retained component inputs for dynamic settings rows.

use gpui_kit::component::{
    Sizable as _, Size,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::{
    AnyElement, Context, Entity, IntoElement, SharedString, Styled, Subscription, Window, div,
    prelude::*, rems,
};

use super::{
    model::{SettingsIntent, StatusSegmentIntent},
    window::GpuiSettings,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum InlineInputTarget {
    StringListItem { list: String, index: usize },
    StatusSegmentIcon { setting: String, index: usize },
    EnvironmentName { id: String, index: usize },
    EnvironmentValue { id: String, index: usize },
}

impl InlineInputTarget {
    fn editor_id(&self) -> String {
        match self {
            Self::StringListItem { list, index } => format!("{list}:{index}"),
            Self::StatusSegmentIcon { setting, index } => format!("{setting}:{index}:icon"),
            Self::EnvironmentName { id, index } => format!("{id}:{index}:name"),
            Self::EnvironmentValue { id, index } => format!("{id}:{index}:value"),
        }
    }

    fn value_intent(&self, value: String) -> SettingsIntent {
        match self {
            Self::StringListItem { list, index } => SettingsIntent::SetStringListItem {
                id: list.clone(),
                index: *index,
                value,
            },
            Self::StatusSegmentIcon { setting, index } => SettingsIntent::EditStatusSegments {
                id: setting.clone(),
                edit: StatusSegmentIntent::SetIcon {
                    index: *index,
                    icon: (!value.is_empty()).then_some(value),
                },
            },
            Self::EnvironmentName { index, .. } => SettingsIntent::SetEnvironmentName {
                index: *index,
                value,
            },
            Self::EnvironmentValue { index, .. } => SettingsIntent::SetEnvironmentValue {
                index: *index,
                value,
            },
        }
    }
}

pub(super) struct InlineInputOptions {
    pub(super) selector: String,
    pub(super) target: InlineInputTarget,
    pub(super) value: String,
    pub(super) placeholder: String,
    pub(super) aria_label: String,
    pub(super) enabled: bool,
    pub(super) width_rems: f32,
    pub(super) size: Size,
}

struct InlineInputState {
    owner: Entity<GpuiSettings>,
    target: InlineInputTarget,
    input: Entity<InputState>,
    external_value: String,
    _subscription: Subscription,
}

impl InlineInputState {
    fn on_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let value = input.read(cx).value().to_string();
                let intent = self.target.value_intent(value);
                self.owner
                    .update(cx, |settings, cx| settings.emit(intent, cx));
            }
            InputEvent::PressEnter { .. } => {
                self.owner
                    .update(cx, |settings, cx| settings.finish_inline_input(window, cx));
            }
            InputEvent::Focus => {
                let editor = self.target.editor_id();
                self.owner.update(cx, |settings, cx| {
                    settings.focus_editor(Some(editor), cx);
                });
            }
            InputEvent::Blur => {
                self.owner.update(cx, |settings, cx| {
                    settings.focus_editor(None, cx);
                });
            }
        }
    }

    fn sync_external_value(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_value == value {
            return;
        }
        value.clone_into(&mut self.external_value);
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl GpuiSettings {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_inline_input(
        selector: String,
        target: InlineInputTarget,
        value: &str,
        placeholder: &str,
        aria_label: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Self::render_inline_input_with_options(
            InlineInputOptions {
                selector,
                target,
                value: value.to_owned(),
                placeholder: placeholder.to_owned(),
                aria_label: aria_label.to_owned(),
                enabled,
                width_rems: 22.5,
                size: Size::Medium,
            },
            window,
            cx,
        )
    }

    pub(super) fn render_inline_input_with_options(
        options: InlineInputOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let InlineInputOptions {
            selector,
            target,
            value,
            placeholder,
            aria_label,
            enabled,
            width_rems,
            size,
        } = options;
        let state_key = SharedString::from(format!("settings-inline-input-state-{selector}"));
        let initial = value.clone();
        let initial_placeholder = placeholder;
        let owner = cx.entity();
        let state = window.use_keyed_state(state_key, cx, move |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(initial.clone())
                    .placeholder(initial_placeholder.clone())
            });
            let subscription = cx.subscribe_in(&input, window, InlineInputState::on_input_event);
            InlineInputState {
                owner,
                target,
                input,
                external_value: initial,
                _subscription: subscription,
            }
        });
        state.update(cx, |state, cx| {
            state.sync_external_value(&value, window, cx);
        });
        let input = state.read(cx).input.clone();
        let escape_owner = cx.entity();
        let debug_selector = selector.clone();
        div()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .w(rems(width_rems))
            .max_w_full()
            .on_key_down(move |event, window, app| {
                if event.keystroke.key != "escape" {
                    return;
                }
                app.stop_propagation();
                escape_owner.update(app, |settings, cx| {
                    settings.finish_inline_input(window, cx);
                });
            })
            .child(crate::gpui::focus_input(
                &input,
                Input::new(&input)
                    .aria_label(aria_label)
                    .with_size(size)
                    .disabled(!enabled)
                    .w_full(),
            ))
            .into_any_element()
    }

    pub(super) fn finish_inline_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.content_focus.focus(window, cx);
        self.focus_editor(None, cx);
    }
}
