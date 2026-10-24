//! Shared editor and toolbar geometry for new and established conversations.
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{App, Div, Length, Styled};

pub fn surface(cx: &App, focused: bool, invalid: bool) -> Div {
    v_flex()
        .relative()
        .w_full()
        .min_w_0()
        .gap_2()
        .p_3()
        .pb_2()
        .rounded_xl()
        .bg(cx.theme().muted.opacity(0.35))
        .border_1()
        .border_color(if invalid {
            cx.theme().danger
        } else if focused {
            cx.theme().ring
        } else {
            cx.theme().border
        })
}

pub fn controls() -> Div {
    h_flex().w_full().min_w_0().items_center().gap_2()
}

pub fn column_width(width: bootty_config::config::TranscriptWidth) -> Length {
    width.rems().map_or_else(
        || gpui_kit::relative(1.).into(),
        |width| gpui_kit::rems(width).into(),
    )
}
