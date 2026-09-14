use num_traits::ToPrimitive as _;

use gpui_kit::{Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};

use super::{
    ChromeIntent, ChromePalette, ContextMenu, GpuiChrome, MenuRow, Rgba, SpaceSnapshot,
    SpaceTransition, color, sidebar::DraggedSidebarRow,
};
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::{
    Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
};

const BUTTON_SIZE: f32 = 28.0;
const BUTTON_GAP: f32 = 4.0;

pub(super) fn render(
    spaces: &[SpaceSnapshot],
    transition: Option<SpaceTransition>,
    height: f32,
    background: Rgba,
    colors: ChromePalette,
    cx: &Context<GpuiChrome>,
) -> gpui_kit::AnyElement {
    let controls_min_width = px((spaces.len().to_f32().unwrap_or(f32::MAX)).mul_add(
        BUTTON_GAP,
        (spaces.len().to_f32().unwrap_or(f32::MAX) + 1.0) * BUTTON_SIZE,
    ) + 8.0);
    let buttons = spaces
        .iter()
        .map(|space| space_button(space, transition, colors, cx));
    let create_owner = cx.weak_entity();
    div()
        .id("bootty-gpui-space-switcher")
        .debug_selector(|| "bootty-gpui-space-switcher".to_owned())
        .flex_none()
        .h(px(height))
        .w_full()
        .p_1()
        .overflow_x_scroll()
        .border_t_1()
        .border_color(color(colors.border))
        .child(
            div()
                .w_full()
                .min_w(controls_min_width)
                .flex()
                .items_center()
                .justify_center()
                .gap(px(BUTTON_GAP))
                .children(buttons)
                .child(super::button::activated_button(
                    div()
                        .debug_selector(|| "space-create".to_owned())
                        .size(px(BUTTON_SIZE))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center(),
                    Button::new("space-create")
                        .icon(
                            Icon::new(IconName::Plus)
                                .small()
                                .text_color(color(colors.subtext)),
                        )
                        .ghost()
                        .accessibility_label("New Space")
                        .tooltip("New Space"),
                    move |_, app| {
                        _ = create_owner.update(app, |_, cx| cx.emit(ChromeIntent::CreateSpace));
                    },
                )),
        )
        .bg(color(background))
        .into_any_element()
}

fn space_button(
    space: &SpaceSnapshot,
    transition: Option<SpaceTransition>,
    colors: ChromePalette,
    cx: &Context<GpuiChrome>,
) -> impl IntoElement {
    let activate = space.key;
    let click_space = space.clone();
    let menu_space = space.clone();
    let tooltip_name = space.name.clone();
    let tooltip_error = space.error.clone();
    let can_drop_sessions = space.accepts_moves;
    let drop_space = space.clone();
    let selected = transition.map_or(if space.active { 1.0 } else { 0.0 }, |transition| {
        if space.key == transition.from {
            1.0 - transition.progress.clamp(0.0, 1.0)
        } else if space.key == transition.to {
            transition.progress.clamp(0.0, 1.0)
        } else {
            0.0
        }
    });
    let button_id = SharedString::from(format!("space-{}", space.key.0));
    let visual = div()
        .id(button_id)
        .debug_selector({
            let key = space.key.0;
            move || format!("space-{key}")
        })
        .relative()
        .size(px(BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .text_color(if space.error.is_some() {
            color(colors.muted)
        } else if selected > 0.0 {
            color(space.color)
        } else {
            color(colors.subtext)
        })
        .when(selected > 0.0, |element| {
            element.bg(color(with_opacity(colors.surface, selected)))
        })
        .hover(move |style| style.bg(color(colors.hover)))
        .can_drop(move |value, _, _| can_drop_sessions && value.is::<DraggedSidebarRow>())
        .on_drop(cx.listener(move |_, dragged: &DraggedSidebarRow, _, cx| {
            if drop_space.accepts_moves && !dragged.sessions.is_empty() {
                cx.emit(ChromeIntent::MoveSessionsToSpace {
                    sessions: dragged.sessions.clone(),
                    to: drop_space.key,
                });
            }
        }))
        .child(crate::gpui::icon(
            &space.icon,
            15.0,
            color(if space.error.is_some() {
                colors.muted
            } else if selected > 0.0 {
                space.color
            } else {
                colors.subtext
            }),
        ));
    let owner = cx.weak_entity();
    let visual = visual.context_menu(move |menu, _, _| {
        super::popup_menu(menu, &ContextMenu::Space(menu_space.clone()), &owner)
    });
    let owner = cx.weak_entity();
    let button = Button::new(SharedString::from(format!("space-button-{}", space.key.0)))
        .ghost()
        .p_0()
        .w(px(BUTTON_SIZE))
        .h(px(BUTTON_SIZE))
        .tab_index(0_isize)
        .accessibility_label(space.name.clone())
        .tooltip(tooltip_error.map_or_else(
            || tooltip_name.clone(),
            |detail| format!("{tooltip_name}\n{detail}"),
        ))
        .child(visual);
    super::button::activated_button(div().size(px(BUTTON_SIZE)), button, move |_, app| {
        if !click_space.active {
            _ = owner.update(app, |_, cx| cx.emit(ChromeIntent::ActivateSpace(activate)));
        }
    })
}

fn with_opacity(mut color: Rgba, opacity: f32) -> Rgba {
    color.alpha = (f32::from(color.alpha) * opacity.clamp(0.0, 1.0))
        .round()
        .to_u8()
        .unwrap_or(0);
    color
}

pub(super) fn space_menu(space: &SpaceSnapshot) -> Vec<MenuRow> {
    let row = |label: &str, enabled: bool, destructive: bool, starts_group: bool, intent| MenuRow {
        label: label.to_owned(),
        enabled,
        destructive,
        starts_group,
        intent,
    };
    let mut rows = Vec::new();
    if space.error.is_some() {
        rows.push(row(
            "Reconnect",
            true,
            false,
            false,
            ChromeIntent::ReconnectSpace(space.key),
        ));
    }
    rows.push(row(
        "Edit Space",
        true,
        false,
        !rows.is_empty(),
        ChromeIntent::EditSpace(space.key),
    ));
    rows.push(row(
        "Close",
        space.can_close,
        true,
        true,
        ChromeIntent::CloseSpace(space.key),
    ));
    rows
}
