//! Full-page history uses the existing searchable list and command owner.
use super::*;

impl DialogView {
    pub(super) fn history_panel(
        &mut self,
        spec: &DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let mut list = spec.clone();
        list.rows.retain(|row| row.id.0 != "scope");
        let scope = spec.rows.iter().find(|row| row.id.0 == "scope").cloned();
        let mut filters = h_flex().gap_2().items_center();
        for field in &spec.fields {
            let DialogFieldKind::Choice(options) = &field.kind else {
                continue;
            };
            let owner = cx.weak_entity();
            let field = field.clone();
            let dialog = spec.id.clone();
            let options = options.clone();
            filters = filters.child(
                Button::new(SharedString::from(format!("history-{}", field.id)))
                    .outline()
                    .small()
                    .label(field.value.clone())
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, _, _| {
                        options.iter().fold(menu, |menu, option| {
                            let owner = owner.clone();
                            let dialog = dialog.clone();
                            let field = field.id.clone();
                            let value = option.clone();
                            menu.item(
                                gpui_kit::component::menu::PopupMenuItem::new(value.clone())
                                    .on_click(move |_, _, cx| {
                                        _ = owner.update(cx, |_, cx| {
                                            cx.emit(DialogIntent::FieldChanged {
                                                dialog: dialog.clone(),
                                                field: field.clone(),
                                                value: value.clone(),
                                            });
                                        });
                                    }),
                            )
                        })
                    }),
            );
        }
        if let Some(scope) = scope {
            let dialog = spec.id.clone();
            filters = filters.child(
                Button::new("history-project-scope")
                    .outline()
                    .small()
                    .label(scope.label.clone())
                    .disabled(!scope.enabled)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(DialogIntent::Activate {
                            dialog: dialog.clone(),
                            action: ActionId::new("toggle-scope"),
                            row: RowId::new("scope"),
                            payload: DialogPayload::None,
                        });
                    })),
            );
        }
        v_flex()
            .size_full()
            .min_h_0()
            .gap_4()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .child("History"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Continue an existing conversation."),
                            ),
                    )
                    .child(
                        Button::new("history-close")
                            .icon(gpui_kit::assets::IconName::X)
                            .ghost()
                            .small()
                            .accessibility_label("Close history")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    ),
            )
            .child(filters)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.command_panel(&list, window, cx)),
            )
            .into_any_element()
    }
}
