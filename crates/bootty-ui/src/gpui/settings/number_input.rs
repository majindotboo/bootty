//! Numeric settings input, slider previews, and accepted-value synchronization.

use super::model::NumberControl;
use super::{
    model::{ScalarValue, SettingsIntent},
    window::GpuiSettings,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    input::{InputEvent as ComponentInputEvent, InputState as ComponentInputState},
};
use gpui_kit::component::{
    button::Button,
    input::{MaskPattern, NumberInput},
    label::Label,
    slider::{Slider as ComponentSlider, SliderEvent, SliderState},
};
use gpui_kit::{
    AnyElement, Context, Entity, IntoElement, ParentElement, SharedString, Styled, Subscription,
    Window, div, prelude::*, rems,
};

/// Width of a number editor, including its two stepper buttons, in the UI's zoom-aware unit.
///
/// The component reserves two `2rem` buttons before laying out the editable text region. Keep
/// enough rems for the value and suffix at the largest supported UI font size instead of letting
/// flexbox squeeze the text into a clipped sliver.
const SETTINGS_NUMBER_INPUT_WIDTH_REMS: f32 = 10.0;

struct NumberControlConfig {
    id: String,
    range: std::ops::RangeInclusive<f32>,
    precision: usize,
    display_scale: f32,
    optional: bool,
}

struct NumberControlState {
    config: NumberControlConfig,
    owner: Entity<GpuiSettings>,
    input: Entity<ComponentInputState>,
    slider: Option<Entity<SliderState>>,
    external_value: f32,
    current_value: f32,
    slider_preview: Option<f32>,
    _subscriptions: Vec<Subscription>,
}

impl NumberControlState {
    #[expect(
        clippy::float_cmp,
        reason = "Exact accepted values deduplicate slider notifications."
    )]
    fn new(
        config: NumberControlConfig,
        control: NumberControl,
        value: f32,
        owner: Entity<GpuiSettings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let precision = config.precision;
        let display_scale = config.display_scale;
        let display_step = precision_step(precision);
        let value_step = (display_step / display_scale).max(f32::EPSILON);
        let range_start = *config.range.start();
        let range_end = *config.range.end();
        let input = cx.new(|cx| {
            ComponentInputState::new(window, cx)
                .default_value(format_number(value * display_scale, precision))
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(precision),
                })
                .step(f64::from(display_step))
                .min(f64::from(range_start * display_scale))
                .max(f64::from(range_end * display_scale))
        });
        let slider = (control == NumberControl::Slider).then(|| {
            cx.new(|_| {
                SliderState::new()
                    .max(range_end)
                    // Set max before min: SliderState starts with max=100, and updating
                    // thumb position while min is above that default panics for ranges such
                    // as sidebar width (120..=600).
                    .min(range_start)
                    .step(value_step)
                    .default_value(value)
            })
        });
        let mut subscriptions = vec![cx.subscribe_in(&input, window, Self::on_input_event)];
        if let Some(slider) = &slider {
            subscriptions.push(cx.subscribe_in(slider, window, Self::on_slider_event));
            // The pinned Slider's accessibility actions notify without Change/Release.
            // Observe those discrete changes; drag events already mark a local preview.
            subscriptions.push(cx.observe_in(slider, window, |this, slider, window, cx| {
                let value = slider.read(cx).value().start();
                if this.slider_preview.is_none() && value != this.current_value {
                    this.sync_input(value, window, cx);
                    this.emit_value(value, cx);
                }
            }));
        }
        Self {
            config,
            owner,
            input,
            slider,
            external_value: value,
            current_value: value,
            slider_preview: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_input_event(
        &mut self,
        input: &Entity<ComponentInputState>,
        event: &ComponentInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, ComponentInputEvent::Change) && self.slider_preview.is_some() {
            return;
        }
        let commit = matches!(
            event,
            ComponentInputEvent::Blur | ComponentInputEvent::PressEnter { .. }
        );
        if !commit && !matches!(event, ComponentInputEvent::Change) {
            return;
        }

        let text = input.read(cx).value();
        if text.trim().is_empty() {
            if commit && self.config.optional {
                self.owner.update(cx, |settings, cx| {
                    settings.emit(SettingsIntent::RemoveValue(self.config.id.clone()), cx);
                });
            } else if commit {
                self.sync_input(self.current_value, window, cx);
            }
            return;
        }

        let Some(raw_value) = parse_number_input(&text, self.config.display_scale) else {
            if commit {
                self.sync_input(self.current_value, window, cx);
            }
            return;
        };
        if !commit && !self.config.range.contains(&raw_value) {
            return;
        }
        let Some(value) = normalize_number_input(
            raw_value,
            &self.config.range,
            self.config.precision,
            self.config.display_scale,
        ) else {
            return;
        };

        if commit {
            self.sync_input(value, window, cx);
        }
        if let Some(slider) = &self.slider {
            slider.update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
        self.emit_value(value, cx);
    }

    fn on_slider_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (value, commit) = match event {
            SliderEvent::Change(value) => (value, false),
            SliderEvent::Release(value) => (value, true),
        };
        let Some(value) = normalize_number_input(
            value.start(),
            &self.config.range,
            self.config.precision,
            self.config.display_scale,
        ) else {
            return;
        };
        // Dragging previews the control locally; persist and rebuild the terminal once on release.
        self.slider_preview = Some(value);
        self.sync_input(value, window, cx);
        if commit {
            self.emit_value(value, cx);
            self.slider_preview = None;
        }
    }

    #[expect(
        clippy::float_cmp,
        reason = "Exact accepted values deduplicate control events and prevent feedback loops."
    )]
    fn emit_value(&mut self, value: f32, cx: &mut Context<Self>) {
        if self.current_value == value {
            return;
        }
        self.current_value = value;
        self.owner.update(cx, |settings, cx| {
            settings.emit(
                SettingsIntent::SetValue {
                    id: self.config.id.clone(),
                    value: ScalarValue::Number(value),
                },
                cx,
            );
        });
    }

    #[expect(
        clippy::float_cmp,
        reason = "Exact accepted values deduplicate control events and prevent feedback loops."
    )]
    fn sync_external_value(&mut self, value: f32, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_value == value {
            return;
        }
        self.external_value = value;
        self.slider_preview = None;
        self.current_value = value;
        self.sync_input(value, window, cx);
        if let Some(slider) = &self.slider {
            slider.update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
    }

    fn sync_input(&self, value: f32, window: &mut Window, cx: &mut Context<Self>) {
        let value = format_number(value * self.config.display_scale, self.config.precision);
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

impl GpuiSettings {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_number_control(
        &self,
        id: &str,
        label: &str,
        value: f32,
        range: std::ops::RangeInclusive<f32>,
        control: NumberControl,
        precision: usize,
        suffix: &str,
        display_scale: f32,
        optional: bool,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let display_scale = valid_display_scale(display_scale);
        let range_start = *range.start();
        let range_end = *range.end();
        let state_key = SharedString::from(format!(
            "settings-number-state-{id}-{control:?}-{range_start}-{range_end}-{precision}-{display_scale}-{suffix}"
        ));
        let owner = cx.entity();
        let config = NumberControlConfig {
            id: id.to_owned(),
            range,
            precision,
            display_scale,
            optional,
        };
        let state = window.use_keyed_state(state_key, cx, move |window, cx| {
            NumberControlState::new(config, control, value, owner, window, cx)
        });

        state.update(cx, |state, cx| {
            state.sync_external_value(value, window, cx);
        });
        let input = state.read(cx).input.clone();
        let slider = state.read(cx).slider.clone();
        let selector = format!("settings-number-{id}");
        let debug_selector = selector.clone();
        let input = Self::number_input(id, &input, suffix, enabled, cx);

        let mut content = gpui_kit::div()
            .flex()
            .items_center()
            .id(SharedString::from(selector))
            .debug_selector(move || debug_selector)
            .items_center()
            .gap_2();
        if let Some(slider) = slider {
            let slider_selector = format!("settings-slider-{id}");
            let slider_debug_selector = slider_selector.clone();
            content = content.child(
                div()
                    .id(SharedString::from(slider_selector))
                    .debug_selector(move || slider_debug_selector)
                    .w_32()
                    .child(ComponentSlider::new(&slider).disabled(!enabled)),
            );
        }
        content = content.child(input);
        if optional && !self.draft.is_default(id) {
            content = content.child(Self::number_auto_button(id, label, enabled, cx));
        }
        content.into_any_element()
    }

    fn number_auto_button(id: &str, label: &str, enabled: bool, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let intent_id = id.to_owned();
        let auto_selector = format!("settings-number-auto-{id}");
        let auto_debug_selector = auto_selector.clone();
        div()
            .id(SharedString::from(auto_selector.clone()))
            .debug_selector(move || auto_debug_selector)
            .child(
                Button::new(SharedString::from(format!("kit-{auto_selector}")))
                    .label("Auto")
                    .outline()
                    .small()
                    .tab_index(0_isize)
                    .tab_stop(false)
                    .accessibility_label(format!("Use automatic {label}"))
                    .disabled(!enabled)
                    .on_click(move |_, _, app| {
                        entity.update(app, |settings, cx| {
                            settings.emit(SettingsIntent::RemoveValue(intent_id.clone()), cx);
                        });
                    }),
            )
            .into_any_element()
    }

    fn number_input(
        id: &str,
        input: &Entity<ComponentInputState>,
        suffix: &str,
        enabled: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let input_selector = format!("settings-number-input-{id}");
        let input_debug_selector = input_selector.clone();
        // NumberInput owns its semantic spinbutton root, but gpui-component does not currently
        // expose an aria-label/aria-description builder for that root. Do not put a misleading
        // name on this layout wrapper; this needs an upstream component API addition.
        div()
            .id(SharedString::from(input_selector))
            .debug_selector(move || input_debug_selector)
            .w(rems(SETTINGS_NUMBER_INPUT_WIDTH_REMS))
            .flex_shrink_0()
            .child(crate::gpui::focus_input(
                input,
                NumberInput::new(input)
                    .disabled(!enabled)
                    .when(!suffix.is_empty(), |input| {
                        input.suffix(
                            Label::new(suffix.to_owned())
                                .text_sm()
                                .text_color(cx.theme().muted_foreground),
                        )
                    }),
            ))
    }
}
fn quantize_number(value: f32, precision: usize) -> f32 {
    let places = i32::try_from(precision).unwrap_or(8).min(8);
    let scale = 10_f32.powi(places);
    (value * scale).round() / scale
}

fn precision_step(precision: usize) -> f32 {
    10_f32.powi(
        i32::try_from(precision)
            .unwrap_or(8)
            .min(8)
            .saturating_neg(),
    )
}

fn valid_display_scale(display_scale: f32) -> f32 {
    if display_scale.is_finite() && display_scale > 0.0 {
        display_scale
    } else {
        1.0
    }
}

fn parse_number_input(text: &str, display_scale: f32) -> Option<f32> {
    let value = text.trim().parse::<f32>().ok()? / display_scale;
    value.is_finite().then_some(value)
}

fn normalize_number_input(
    value: f32,
    range: &std::ops::RangeInclusive<f32>,
    precision: usize,
    display_scale: f32,
) -> Option<f32> {
    let start = *range.start();
    let end = *range.end();
    if !value.is_finite() || !start.is_finite() || !end.is_finite() || start > end {
        return None;
    }
    let value = value.clamp(start, end);
    Some((quantize_number(value * display_scale, precision) / display_scale).clamp(start, end))
}

fn format_number(value: f32, precision: usize) -> String {
    format!("{value:.precision$}")
}
