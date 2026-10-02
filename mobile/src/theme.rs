use gpui_kit::{
    App,
    component::{Theme, ThemeMode},
    px, rgb,
};

/// Bootty's charcoal surfaces and purple accent; `UIKit` uses the same shell colors.
pub(crate) fn apply(text_size: f32, cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(14. * text_size / 17.);
        theme.mono_font_size = px(12. * text_size / 17.);
        theme.radius = px(4.);
        theme.radius_lg = px(6.);
        let colors = &mut theme.colors;
        colors.background = rgb(0x001b_1d21).into();
        colors.foreground = rgb(0x00ed_f0f2).into();
        colors.title_bar = rgb(0x0013_1518).into();
        colors.sidebar = rgb(0x0024_272c).into();
        colors.muted = rgb(0x0024_272c).into();
        colors.muted_foreground = rgb(0x0093_9aa3).into();
        colors.border = rgb(0x0037_3b43).into();
        colors.input = colors.border;
        colors.primary = rgb(0x00b4_a1e8).into();
        colors.primary_foreground = colors.background;
        colors.button_primary = colors.primary;
        colors.button_primary_foreground = colors.background;
        colors.button_primary_hover = rgb(0x00c6_b7ef).into();
        colors.button_primary_active = rgb(0x009e_88d6).into();
        colors.ring = colors.primary;
        colors.caret = colors.primary;
        colors.selection = rgb(0x0040_364f).into();
        colors.secondary = colors.sidebar;
        colors.secondary_foreground = colors.foreground;
        colors.secondary_hover = rgb(0x0037_3b43).into();
        colors.secondary_active = rgb(0x0042_4750).into();
        colors.accent = colors.secondary_hover;
        colors.accent_foreground = colors.foreground;
        colors.tab_bar = colors.title_bar;
        colors.tab = colors.background;
        colors.tab_foreground = colors.muted_foreground;
        colors.tab_active = colors.background;
        colors.tab_active_foreground = colors.foreground;
    });
}
