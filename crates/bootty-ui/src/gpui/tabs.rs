//! Shared tab appearance and close-affordance layout for dock and mux tabs.
use bootty_config::config::{TabAppearance, TabCloseButton, TabClosePosition, TabConfig};
use gpui_kit::component::{
    Size,
    tab::{TabBar, TabVariant},
};
use gpui_kit::{
    AnyElement, Div, Hsla, MouseButton, ParentElement, SharedString, Styled, div, prelude::*, px,
};

pub const fn variant(appearance: TabAppearance) -> TabVariant {
    match appearance {
        TabAppearance::Classic => TabVariant::Tab,
        TabAppearance::Underline => TabVariant::Underline,
        TabAppearance::Pill => TabVariant::Pill,
        TabAppearance::Outline => TabVariant::Outline,
        TabAppearance::Segmented => TabVariant::Segmented,
    }
}

/// Cover Kit's bar separator before it paints the tabs, preserving the active
/// underline indicator and each variant's own tab background.
pub fn blend_bar(bar: TabBar, appearance: TabAppearance, background: Hsla) -> TabBar {
    bar.when(appearance != TabAppearance::Classic, |bar| {
        bar.bg(background)
            .prefix(div().absolute().inset_0().bg(background))
    })
}

pub fn size(appearance: TabAppearance) -> Size {
    // Kit's medium underline is 36px; the workspace tab row is 32px.
    if appearance == TabAppearance::Underline {
        Size::Small
    } else {
        Size::Medium
    }
}

pub fn content(
    content: AnyElement,
    close: Option<AnyElement>,
    hover_group: SharedString,
    config: TabConfig,
    selected_background: Option<Hsla>,
) -> Div {
    let close = close.filter(|_| config.close_button != TabCloseButton::Hidden);
    let underline = config.appearance == TabAppearance::Underline;
    div()
        .h_full()
        .relative()
        .flex()
        .items_center()
        .min_w_0()
        // Own one padding box, including the close target. Kit already adds 12px
        // except for underline; cancel that inset rather than adding a close column.
        .when(!underline, |row| row.mx(px(-12.)))
        .px_3()
        .when(close.is_some(), gpui_kit::Styled::px_4)
        .when_some(selected_background, |row, background| {
            row.h_full().bg(background).rounded_sm()
        })
        .child(content)
        .when_some(close, |row, close| {
            row.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(config.close_position == TabClosePosition::Left, |button| {
                        button.left_0()
                    })
                    .when(config.close_position == TabClosePosition::Right, |button| {
                        button.right_0()
                    })
                    .when(config.close_button == TabCloseButton::Hover, |button| {
                        button
                            .invisible()
                            .group_hover(hover_group, gpui_kit::Styled::visible)
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(close),
            )
        })
}
