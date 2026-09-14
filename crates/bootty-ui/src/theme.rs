use crate::gpui::{UiPalette, UiTheme, chrome::Rgba};
use bootty_config::{
    color::Color,
    config::{AppearanceVariant, BoottyConfig, ColorConfig},
};
use gpui_kit::WindowAppearance;

#[must_use]
pub const fn appearance_variant(appearance: WindowAppearance) -> AppearanceVariant {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => AppearanceVariant::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => AppearanceVariant::Dark,
    }
}

#[must_use]
pub fn theme_from_config(config: &BoottyConfig, variant: AppearanceVariant) -> UiTheme {
    let colors = config.colors_for_appearance(variant);
    UiTheme {
        palette: theme_palette_from_colors(colors),
    }
}

pub fn theme_palette_from_colors(colors: &ColorConfig) -> UiPalette {
    let mut terminal = [None; 16];
    for (slot, color) in terminal.iter_mut().zip(colors.palette.iter()) {
        *slot = Some(config_rgba(*color));
    }
    UiPalette::from_terminal_colors(
        colors.background.map(config_rgba),
        colors.foreground.map(config_rgba),
        terminal,
    )
}

pub(crate) const fn config_rgba(color: Color) -> Rgba {
    Rgba {
        red: color.r,
        green: color.g,
        blue: color.b,
        alpha: color.a,
    }
}

/// Named theme colors exposed to Luau extensions as `bootty.theme.*`.
#[must_use]
pub fn theme_tokens(config: &BoottyConfig, variant: AppearanceVariant) -> Vec<(String, String)> {
    let palette = theme_from_config(config, variant).palette;
    let hex = |color: Rgba| format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue);
    [
        ("base", palette.base),
        ("mantle", palette.mantle),
        ("pane", palette.pane),
        ("surface", palette.surface),
        ("hover", palette.hover),
        ("border", palette.border),
        ("text", palette.text),
        ("subtext", palette.subtext),
        ("muted", palette.muted),
        ("primary", palette.primary),
        ("accent", palette.accent),
        ("warning", palette.warning),
        ("success", palette.success),
        ("destructive", palette.destructive),
    ]
    .into_iter()
    .map(|(name, color)| (name.to_owned(), hex(color)))
    .collect()
}
