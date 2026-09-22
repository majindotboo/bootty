//! Settings and remote-profile text input lifecycle.

use super::{model::SettingsIntent, window::GpuiSettings};
use gpui_kit::Role;
use gpui_kit::component::{
    Sizable as _, Size,
    input::{
        Input as ComponentInput, InputEvent as ComponentInputEvent,
        InputState as ComponentInputState,
    },
};
use gpui_kit::{
    AnyElement, Context, Entity, IntoElement, ParentElement, SharedString, Styled, Subscription,
    Window, div, prelude::*, rems,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum TextControlTarget {
    Setting(String),
    Remote {
        profile_id: String,
        field_id: String,
    },
}

struct TextControlState {
    target: TextControlTarget,
    owner: Entity<GpuiSettings>,
    input: Entity<ComponentInputState>,
    external_value: String,
    current_value: String,
    _subscription: Subscription,
}

impl TextControlState {
    fn input(
        target: TextControlTarget,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<GpuiSettings>,
    ) -> Entity<ComponentInputState> {
        let key = match &target {
            TextControlTarget::Setting(id) => format!("settings-text-state-{id}"),
            TextControlTarget::Remote {
                profile_id,
                field_id,
            } => format!("settings-remote-input-state-{profile_id}-{field_id}"),
        };
        let owner = cx.entity();
        let initial_value = value.to_owned();
        let placeholder = placeholder.to_owned();
        let state = window.use_keyed_state(SharedString::from(key), cx, move |window, cx| {
            let input = cx.new(|cx| {
                ComponentInputState::new(window, cx)
                    .default_value(initial_value.clone())
                    .placeholder(placeholder)
            });
            let subscription = cx.subscribe_in(&input, window, Self::on_input_event);
            Self {
                target,
                owner,
                input,
                external_value: initial_value.clone(),
                current_value: initial_value,
                _subscription: subscription,
            }
        });
        state.update(cx, |state, cx| state.sync_external_value(value, window, cx));
        state.read(cx).input.clone()
    }

    fn on_input_event(
        &mut self,
        input: &Entity<ComponentInputState>,
        event: &ComponentInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComponentInputEvent::Change => {
                let value = input.read(cx).value().to_string();
                if self.current_value == value {
                    return;
                }
                self.current_value.clone_from(&value);
                let intent = match &self.target {
                    TextControlTarget::Setting(id) => SettingsIntent::SetText {
                        id: id.clone(),
                        value,
                    },
                    TextControlTarget::Remote {
                        profile_id,
                        field_id,
                    } => SettingsIntent::SetRemoteField {
                        profile_id: profile_id.clone(),
                        field_id: field_id.clone(),
                        value,
                    },
                };
                self.owner
                    .update(cx, |settings, cx| settings.emit(intent, cx));
            }
            ComponentInputEvent::PressEnter { .. } => {
                if matches!(&self.target, TextControlTarget::Remote { .. }) {
                    self.owner.update(cx, |settings, cx| {
                        settings.finish_inline_input(window, cx);
                    });
                }
            }
            ComponentInputEvent::Focus => {
                if let TextControlTarget::Remote {
                    profile_id,
                    field_id,
                } = &self.target
                {
                    let editor = format!("remote-field:{profile_id}:{field_id}");
                    self.owner.update(cx, |settings, cx| {
                        settings.focus_editor(Some(editor), cx);
                    });
                }
            }
            ComponentInputEvent::Blur => {
                if matches!(&self.target, TextControlTarget::Remote { .. }) {
                    self.owner.update(cx, |settings, cx| {
                        settings.focus_editor(None, cx);
                    });
                }
            }
        }
    }

    fn sync_external_value(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_value == value {
            return;
        }
        self.external_value.clear();
        self.external_value.push_str(value);
        if self.current_value == value {
            return;
        }
        self.current_value.clear();
        self.current_value.push_str(value);
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl GpuiSettings {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_setting_text_control(
        id: &str,
        label: &str,
        help: &str,
        value: &str,
        placeholder: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = TextControlState::input(
            TextControlTarget::Setting(id.to_owned()),
            value,
            placeholder,
            window,
            cx,
        );
        let selector = format!("settings-input-{id}");
        let debug_selector = selector.clone();
        div()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .w(rems(22.5))
            .max_w_full()
            .child(crate::gpui::focus_input(
                &input,
                ComponentInput::new(&input)
                    // gpui-component exposes the editor's accessible name but not a separate
                    // description for this control. Keep the setting help in the real input's
                    // announced name until the component forwards aria_description.
                    .aria_label(format!("{label}. {help}"))
                    .disabled(!enabled)
                    .with_size(Size::Medium)
                    .w_full(),
            ))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_remote_text_field(
        profile_id: String,
        field_id: String,
        selector: String,
        aria_label: String,
        value: &str,
        placeholder: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = TextControlState::input(
            TextControlTarget::Remote {
                profile_id,
                field_id,
            },
            value,
            placeholder,
            window,
            cx,
        );
        let debug_selector = selector.clone();
        let escape_owner = cx.entity();
        div()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .w(rems(22.5))
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
                ComponentInput::new(&input)
                    .aria_label(aria_label)
                    .role(Role::TextInput)
                    .disabled(!enabled)
                    .with_size(Size::Medium)
                    .w_full(),
            ))
            .into_any_element()
    }
}
