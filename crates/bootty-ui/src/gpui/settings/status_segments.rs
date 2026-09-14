use crate::gpui::color_picker::{
    ColorPickerParams, ColorPickerUpdate, parse_hex_color, render_color_picker,
};
use gpui_kit::component::{Disableable as _, Sizable as _, button::Button, label::Label};
use std::rc::Rc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*,
    rems,
};

use super::{
    components::{
        OrderedRowMovement, debug_wrapper, ordered_collection_scope, ordered_row_drag_handle,
        ordered_row_remove_button,
    },
    inline_inputs::InlineInputTarget,
    model::{
        SettingsChoice, SettingsIntent, StatusSegmentAlignment, StatusSegmentColor,
        StatusSegmentEditorRow, StatusSegmentIntent, StatusSegmentsSnapshot,
    },
    window::GpuiSettings,
};

impl GpuiSettings {
    pub(super) fn render_status_segments(
        editor: &StatusSegmentsSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = editor.segments.len();
        let drag_scope = ordered_collection_scope(
            &format!("status-segments:{}", editor.id),
            editor
                .segments
                .iter()
                .map(|segment| {
                    (
                        &segment.module,
                        format!(
                            "{:?}{:?}{:?}{:?}",
                            segment.alignment, segment.foreground, segment.background, segment.icon
                        ),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let mut content = gpui_kit::div()
            .flex()
            .flex_col()
            .w(rems(35.0))
            .max_w_full()
            .gap_2()
            .child(Self::render_status_preview(editor, cx));

        for (index, segment) in editor.segments.iter().enumerate() {
            content = content.child(Self::render_status_segment_row(
                &editor.id,
                index,
                count,
                segment,
                &editor.modules,
                &drag_scope,
                window,
                cx,
            ));
        }

        let entity = cx.entity();
        let id = editor.id.clone();
        let added_module = editor.modules.first().map(|module| module.token.clone());
        let can_add = added_module.is_some();
        let add_selector = format!("settings-status-segments-{}-add", editor.id);
        content
            .when(editor.segments.is_empty(), |this| {
                this.child(
                    Label::new("No modules in this bar").text_color(cx.theme().muted_foreground),
                )
            })
            .child(debug_wrapper(
                add_selector.clone(),
                Button::new(SharedString::from(format!("kit-{add_selector}")))
                    .label(editor.add_label.clone())
                    .outline()
                    .xsmall()
                    .disabled(!can_add)
                    .on_click(move |_, _, app| {
                        let Some(module) = added_module.clone() else {
                            return;
                        };
                        entity.update(app, |this, cx| {
                            this.emit(
                                SettingsIntent::EditStatusSegments {
                                    id: id.clone(),
                                    edit: StatusSegmentIntent::Add { module },
                                },
                                cx,
                            );
                        });
                    }),
            ))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_status_segment_row(
        id: &str,
        index: usize,
        count: usize,
        segment: &StatusSegmentEditorRow,
        modules: &[SettingsChoice],
        drag_scope: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Status segment DTOs currently have no durable identity; index is the authoritative
        // mutation coordinate. Keep it in selectors until the settings model grows segment IDs,
        // and avoid retaining row-local state by index beyond the inline control's host key.
        let module = Self::status_segment_module(id, index, &segment.module, modules, window, cx);
        let alignment_id = id.to_owned();
        let alignment = Self::render_dropdown(
            format!("settings-status-segment-{id}-{index}-alignment"),
            "Alignment",
            "Position this module on the left, center, or right of the bar.",
            alignment_token(segment.alignment),
            &alignment_choices(),
            true,
            Rc::new(move |token| SettingsIntent::EditStatusSegments {
                id: alignment_id.clone(),
                edit: StatusSegmentIntent::SetAlignment {
                    index,
                    alignment: alignment_from_token(&token),
                },
            }),
            window,
            cx,
        );
        let row_selector = format!("settings-status-segment-{id}-{index}");
        let actions_entity = cx.entity();
        let actions_id = id.to_owned();
        let drag_scope = drag_scope.to_owned();
        let row_label = segment.module.clone();
        let focus_border = cx.theme().ring;

        let row_debug_selector = row_selector.clone();
        let mut row = gpui_kit::div()
            .flex()
            .flex_col()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_debug_selector)
            .w_full()
            .gap_2()
            .p_2()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().input)
            .focusable()
            .tab_index(0_isize)
            .focus_visible(move |style| style.border_1().border_color(focus_border))
            .child(
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .w_full()
                    .flex_wrap()
                    .gap_2()
                    .child(ordered_row_drag_handle(
                        row_selector,
                        drag_scope.clone(),
                        index.to_string(),
                        row_label.clone(),
                        cx.theme().muted_foreground,
                    ))
                    .child(module)
                    .child(alignment)
                    .child(div().flex_1())
                    .child(ordered_row_remove_button(
                        format!("status-segment-{id}-{index}"),
                        row_label,
                        true,
                        move |app| {
                            actions_entity.update(app, |this, cx| {
                                this.emit(
                                    SettingsIntent::EditStatusSegments {
                                        id: actions_id.clone(),
                                        edit: StatusSegmentIntent::Remove { index },
                                    },
                                    cx,
                                );
                            });
                        },
                    )),
            )
            .child(Self::status_segment_appearance(
                id, index, segment, window, cx,
            ));

        let movement_id = id.to_owned();
        row = OrderedRowMovement {
            selector: format!("status-segment-{id}-{index}"),
            scope: drag_scope,
            index,
            count,
            enabled: true,
        }
        .apply(
            row,
            move |index, offset| SettingsIntent::EditStatusSegments {
                id: movement_id.clone(),
                edit: StatusSegmentIntent::Move { index, offset },
            },
            cx,
        );
        row.into_any_element()
    }

    fn status_segment_module(
        id: &str,
        index: usize,
        module: &str,
        modules: &[SettingsChoice],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let module_id = id.to_owned();
        Self::render_dropdown(
            format!("settings-status-segment-{id}-{index}-module"),
            "Module",
            "Status module rendered in this position.",
            module,
            modules,
            true,
            Rc::new(move |module| SettingsIntent::EditStatusSegments {
                id: module_id.clone(),
                edit: StatusSegmentIntent::SetModule { index, module },
            }),
            window,
            cx,
        )
    }

    fn status_segment_appearance(
        id: &str,
        index: usize,
        segment: &StatusSegmentEditorRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let foreground = Self::render_status_color_control(
            id,
            index,
            StatusSegmentColor::Foreground,
            segment.foreground.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        let background = Self::render_status_color_control(
            id,
            index,
            StatusSegmentColor::Background,
            segment.background.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        let icon_target = InlineInputTarget::StatusSegmentIcon {
            setting: id.to_owned(),
            index,
        };
        let icon = Self::render_inline_input(
            format!("settings-status-segment-{id}-{index}-icon"),
            icon_target,
            segment.icon.as_deref().unwrap_or_default(),
            "Optional icon",
            "Optional status icon",
            true,
            window,
            cx,
        );

        gpui_kit::div()
            .flex()
            .items_center()
            .w_full()
            .flex_wrap()
            .gap_2()
            .child(field("Foreground", foreground, cx))
            .child(field("Background", background, cx))
            .child(field("Icon", icon, cx))
            .into_any_element()
    }

    fn render_status_preview(editor: &StatusSegmentsSnapshot, cx: &Context<Self>) -> AnyElement {
        let aligned = |alignment| {
            editor
                .segments
                .iter()
                .filter(move |segment| segment.alignment == alignment)
                .map(|segment| preview_segment(segment, cx))
        };
        let selector = format!("settings-status-segments-{}-preview", editor.id);
        let debug_selector = selector.clone();
        gpui_kit::div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Label::new("Preview")
                    .text_sm()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                gpui_kit::div()
                    .flex()
                    .items_center()
                    .id(SharedString::from(selector))
                    .debug_selector(move || debug_selector)
                    .w_full()
                    .min_h(rems(1.875))
                    .px_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().status_bar)
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .gap_1()
                            .children(aligned(StatusSegmentAlignment::Left)),
                    )
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .justify_center()
                            .gap_1()
                            .children(aligned(StatusSegmentAlignment::Center)),
                    )
                    .child(
                        gpui_kit::div()
                            .flex()
                            .items_center()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .justify_end()
                            .gap_1()
                            .children(aligned(StatusSegmentAlignment::Right)),
                    ),
            )
            .into_any_element()
    }

    fn render_status_color_control(
        setting: &str,
        index: usize,
        field: StatusSegmentColor,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let field_name = color_field_name(field);
        let entity = cx.entity();
        let setting = setting.to_owned();
        render_color_picker(
            ColorPickerParams {
                selector: format!("settings-status-segment-{setting}-{index}-{field_name}"),
                label: field_name,
                value,
                default_label: "Default",
                enabled: true,
                resettable: true,
            },
            Rc::new(move |update, app| {
                entity.update(app, |this, cx| {
                    let value = match update {
                        ColorPickerUpdate::Set(value) => Some(value),
                        ColorPickerUpdate::Reset => None,
                    };
                    this.emit(status_color_intent(&setting, index, field, value), cx);
                });
            }),
            window,
            cx,
        )
    }
}

fn field(label: &'static str, control: AnyElement, cx: &gpui_kit::App) -> AnyElement {
    gpui_kit::div()
        .flex()
        .flex_col()
        .min_w(rems(9.375))
        .gap_0p5()
        .child(
            Label::new(label)
                .text_sm()
                .text_color(cx.theme().muted_foreground),
        )
        .child(control)
        .into_any_element()
}

fn preview_segment(segment: &StatusSegmentEditorRow, cx: &gpui_kit::App) -> AnyElement {
    let foreground = segment
        .foreground
        .as_deref()
        .and_then(parse_hex_color)
        .unwrap_or(cx.theme().foreground);
    gpui_kit::div()
        .flex()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .gap_0p5()
        .px_1()
        .rounded_sm()
        .text_color(foreground)
        .when_some(
            segment.background.as_deref().and_then(parse_hex_color),
            gpui_kit::Styled::bg,
        )
        .when_some(segment.icon.clone(), |this, icon| {
            this.child(Label::new(icon))
        })
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(Label::new(segment.module.clone()).text_sm()),
        )
        .into_any_element()
}

fn alignment_choices() -> Vec<SettingsChoice> {
    [("left", "Left"), ("center", "Center"), ("right", "Right")]
        .into_iter()
        .map(|(token, label)| SettingsChoice {
            token: token.to_owned(),
            label: label.to_owned(),
            description: None,
        })
        .collect()
}

const fn alignment_token(alignment: StatusSegmentAlignment) -> &'static str {
    match alignment {
        StatusSegmentAlignment::Left => "left",
        StatusSegmentAlignment::Center => "center",
        StatusSegmentAlignment::Right => "right",
    }
}

fn alignment_from_token(token: &str) -> StatusSegmentAlignment {
    match token {
        "center" => StatusSegmentAlignment::Center,
        "right" => StatusSegmentAlignment::Right,
        _ => StatusSegmentAlignment::Left,
    }
}

const fn color_field_name(field: StatusSegmentColor) -> &'static str {
    match field {
        StatusSegmentColor::Foreground => "Foreground",
        StatusSegmentColor::Background => "Background",
    }
}

fn status_color_intent(
    id: &str,
    index: usize,
    field: StatusSegmentColor,
    value: Option<String>,
) -> SettingsIntent {
    SettingsIntent::EditStatusSegments {
        id: id.to_owned(),
        edit: StatusSegmentIntent::SetColor {
            index,
            field,
            value,
        },
    }
}
