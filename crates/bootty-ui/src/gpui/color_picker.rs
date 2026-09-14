//! Shared color picking for settings and theme authoring.

use num_traits::ToPrimitive as _;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{
    Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
};

use std::rc::Rc;

use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ColorPickerUpdate {
    Set(String),
    Reset,
}

type UpdateHandler = Rc<dyn Fn(ColorPickerUpdate, &mut App)>;

pub(super) struct ColorPickerParams<'a> {
    pub(super) selector: String,
    pub(super) label: &'a str,
    pub(super) value: &'a str,
    pub(super) default_label: &'a str,
    pub(super) enabled: bool,
    pub(super) resettable: bool,
}

struct ColorPickerViewState {
    picker: Entity<ColorPickerState>,
    external_value: String,
    on_update: UpdateHandler,
    subscription: Subscription,
}

impl ColorPickerViewState {
    fn new(
        value: String,
        on_update: UpdateHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (picker, subscription) = Self::build_picker(&value, &on_update, window, cx);
        Self {
            picker,
            external_value: value,
            on_update,
            subscription,
        }
    }

    fn sync_external(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.external_value == value {
            return;
        }

        // Owner acknowledgement of the picker's own edit must preserve its open popover.
        if self.picker.read(cx).value() == parse_hex_color(value) {
            value.clone_into(&mut self.external_value);
            return;
        }
        let (picker, subscription) = Self::build_picker(value, &self.on_update, window, cx);
        self.picker = picker;
        value.clone_into(&mut self.external_value);
        self.subscription = subscription;
        cx.notify();
    }

    fn build_picker(
        value: &str,
        on_update: &UpdateHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<ColorPickerState>, Subscription) {
        let color = parse_hex_color(value);
        let picker = cx.new(|cx| {
            let picker = ColorPickerState::new(window, cx);
            if let Some(color) = color {
                picker.default_value(color)
            } else {
                picker
            }
        });
        let on_update = on_update.clone();
        let subscription = cx.subscribe(&picker, move |_, _, event, cx| match event {
            ColorPickerEvent::Change(Some(color)) => {
                on_update(ColorPickerUpdate::Set(serialize_color(*color)), cx);
            }
            ColorPickerEvent::Change(None) => on_update(ColorPickerUpdate::Reset, cx),
        });
        (picker, subscription)
    }
}

pub(super) fn render_color_picker(
    params: ColorPickerParams<'_>,
    on_update: UpdateHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let ColorPickerParams {
        selector,
        label,
        value,
        default_label,
        enabled,
        resettable,
    } = params;
    let state_key = SharedString::from(format!("{selector}-state"));
    let initial_value = value.to_owned();
    let initial_update = on_update.clone();
    let state = window.use_keyed_state(state_key, cx, move |window, cx| {
        ColorPickerViewState::new(initial_value, initial_update, window, cx)
    });
    state.update(cx, |state, cx| state.sync_external(value, window, cx));
    let picker = state.read(cx).picker.clone();

    let display_value = if value.is_empty() {
        default_label.to_owned()
    } else {
        value.to_owned()
    };
    let debug_selector = selector.clone();
    let component_selector = format!("{selector}-component");
    let component_debug_selector = component_selector.clone();
    let reset_selector = format!("{selector}-reset");
    let reset_debug_selector = reset_selector.clone();
    let reset_button_id = format!("{reset_selector}-button");
    let on_reset = on_update;
    let component: AnyElement = if enabled {
        ColorPicker::new(&picker)
            .label(display_value)
            .accessibility_label(label.to_owned())
            .anchor(Anchor::TopRight)
            .into_any_element()
    } else {
        Button::new(SharedString::from(format!("{component_selector}-disabled")))
            .label(display_value)
            .outline()
            .small()
            .disabled(true)
            .accessibility_label(format!("{label} (disabled)"))
            .into_any_element()
    };

    gpui_kit::div()
        .flex()
        .items_center()
        .id(SharedString::from(selector))
        .debug_selector(move || debug_selector)
        .gap_1()
        .relative()
        .child(
            div()
                .id(SharedString::from(component_selector))
                .debug_selector(move || component_debug_selector)
                .child(component),
        )
        .when(enabled && resettable && !value.is_empty(), |this| {
            this.child(
                div()
                    .id(SharedString::from(reset_selector))
                    .debug_selector(move || reset_debug_selector)
                    .child(
                        Button::new(SharedString::from(reset_button_id))
                            .ghost()
                            .small()
                            .icon(IconName::Undo)
                            .tab_stop(false)
                            .text_color(cx.theme().muted_foreground)
                            .accessibility_label(format!("Reset {label} to Default"))
                            .tooltip("Reset to Default")
                            .on_click(move |_, _, cx| {
                                on_reset(ColorPickerUpdate::Reset, cx);
                            }),
                    ),
            )
        })
        .into_any_element()
}

fn serialize_color(color: gpui_kit::Hsla) -> String {
    let color = gpui_kit::Rgba::from(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round().to_u8().unwrap_or(0);
    let red = channel(color.r);
    let green = channel(color.g);
    let blue = channel(color.b);
    let alpha = channel(color.a);

    if alpha < u8::MAX {
        format!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}")
    } else {
        format!("#{red:02X}{green:02X}{blue:02X}")
    }
}

/// Uses GPUI's pinned parser semantics: `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa`.
pub(super) fn parse_hex_color(value: &str) -> Option<gpui_kit::Hsla> {
    gpui_kit::Rgba::try_from(value).ok().map(Into::into)
}
