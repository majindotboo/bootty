//! Structured name/value editor for session environment variables.

use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, button::Button};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Size;
use gpui_kit::{AnyElement, Context, SharedString, Window, prelude::*};

use super::{
    components::{
        OrderedRowMovement, debug_wrapper, ordered_collection_scope, ordered_row_drag_handle,
        ordered_row_remove_button,
    },
    inline_inputs::{InlineInputOptions, InlineInputTarget},
    model::{EnvironmentVariable, SettingsIntent},
    window::GpuiSettings,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnvironmentField {
    Name,
    Value,
}

impl GpuiSettings {
    pub(super) fn render_environment(
        id: &str,
        items: &[EnvironmentVariable],
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = items.len();
        let drag_scope = ordered_collection_scope(
            &format!("environment:{id}"),
            items
                .iter()
                .map(|item| (&item.name, &item.value))
                .collect::<Vec<_>>(),
        );
        let collection = EnvironmentRows {
            id,
            count,
            drag_scope,
            enabled,
        };
        let rows = items
            .iter()
            .enumerate()
            .map(|(index, item)| collection.render_row(index, item, window, cx))
            .collect::<Vec<_>>();
        let add_entity = cx.entity();
        gpui_kit::div()
            .flex()
            .flex_col()
            .w_full()
            .max_w_128()
            .gap_1()
            .children(rows)
            .child(debug_wrapper(
                format!("settings-environment-{id}-add"),
                Button::new(SharedString::from(format!(
                    "kit-settings-environment-{id}-add"
                )))
                .label("Add variable")
                .icon(Icon::new(IconName::Plus).small())
                .outline()
                .small()
                .disabled(!enabled)
                .on_click(move |_, _, app| {
                    add_entity.update(app, |this, cx| {
                        this.emit(SettingsIntent::AddEnvironmentVariable, cx);
                    });
                }),
            ))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_environment_field(
        id: &str,
        selector: String,
        index: usize,
        field: EnvironmentField,
        value: &str,
        placeholder: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Environment rows are still addressed by index in the typed persistence contract. Keep
        // that complete domain coordinate in the retained-state key until the model gains row IDs.
        let target = match field {
            EnvironmentField::Name => InlineInputTarget::EnvironmentName {
                id: id.to_owned(),
                index,
            },
            EnvironmentField::Value => InlineInputTarget::EnvironmentValue {
                id: id.to_owned(),
                index,
            },
        };
        Self::render_inline_input_with_options(
            InlineInputOptions {
                selector,
                target,
                value: value.to_owned(),
                placeholder: placeholder.to_owned(),
                aria_label: match field {
                    EnvironmentField::Name => "Environment variable name".to_owned(),
                    EnvironmentField::Value => "Environment variable value".to_owned(),
                },
                enabled,
                width_rems: if matches!(field, EnvironmentField::Name) {
                    8.0
                } else {
                    12.0
                },
                size: Size::Small,
            },
            window,
            cx,
        )
    }
}

struct EnvironmentRows<'a> {
    id: &'a str,
    count: usize,
    drag_scope: String,
    enabled: bool,
}

impl EnvironmentRows<'_> {
    fn render_row(
        &self,
        index: usize,
        item: &EnvironmentVariable,
        window: &mut Window,
        cx: &mut Context<GpuiSettings>,
    ) -> AnyElement {
        let id = self.id;
        let enabled = self.enabled;
        let count = self.count;
        let entity = cx.entity();
        let row_selector = format!("settings-environment-{id}-{index}");
        let row_label = item.name.clone();
        let focus_border = cx.theme().ring;
        let mut row = gpui_kit::div()
            .flex()
            .items_center()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector({
                let selector = row_selector.clone();
                move || selector
            })
            .w_full()
            .flex_wrap()
            .gap_0p5()
            .focusable()
            .tab_index(0_isize)
            .focus_visible(move |style| style.border_1().border_color(focus_border));
        if enabled {
            row = row.child(ordered_row_drag_handle(
                row_selector.clone(),
                self.drag_scope.clone(),
                index.to_string(),
                row_label.clone(),
                cx.theme().muted_foreground,
            ));
        }
        row = row
            .child(GpuiSettings::render_environment_field(
                id,
                format!("settings-environment-{id}-{index}-name"),
                index,
                EnvironmentField::Name,
                &item.name,
                "NAME",
                enabled,
                window,
                cx,
            ))
            .child(GpuiSettings::render_environment_field(
                id,
                format!("settings-environment-{id}-{index}-value"),
                index,
                EnvironmentField::Value,
                &item.value,
                "Value",
                enabled,
                window,
                cx,
            ));
        row = row.child(ordered_row_remove_button(
            row_selector.clone(),
            row_label,
            enabled,
            move |app| {
                entity.update(app, |this, cx| {
                    this.emit(SettingsIntent::RemoveEnvironmentVariable(index), cx);
                });
            },
        ));
        OrderedRowMovement {
            selector: row_selector,
            scope: self.drag_scope.clone(),
            index,
            count,
            enabled,
        }
        .apply(
            row,
            |index, offset| SettingsIntent::MoveEnvironmentVariable { index, offset },
            cx,
        )
        .into_any_element()
    }
}
