//! ANSI palette values offered by the native settings editor.

use bootty_config::{color::Color, config::ColorConfig};
use libghostty_vt::style::RgbColor;

/// Resolve the active standard 16 colors: explicit entries win, then the selected theme, then
/// Bootty's built-in terminal defaults.
#[must_use]
pub fn standard_16_palette(overrides: &[Color], resolved: &ColorConfig) -> Vec<Color> {
    let defaults = bootty_terminal::terminal_palette::default_base16();
    defaults
        .into_iter()
        .enumerate()
        .map(|(index, default)| {
            overrides.get(index).copied().unwrap_or_else(|| {
                resolved
                    .palette
                    .get(index)
                    .copied()
                    .unwrap_or_else(|| rgb_to_color(default))
            })
        })
        .collect()
}

/// Resolve the legacy xterm-256 preset with the same generator used by live terminals.
#[must_use]
pub fn xterm_256_palette(overrides: &[Color], resolved: &ColorConfig) -> Vec<Color> {
    let base16 = standard_16_palette(overrides, resolved);
    let base: [RgbColor; 256] = std::array::from_fn(|index| {
        base16
            .get(index)
            .copied()
            .map_or(RgbColor { r: 0, g: 0, b: 0 }, color_to_rgb)
    });
    // Preserve standard colors verbatim and generate only the cube and grayscale ramp.
    let skip: [bool; 256] = std::array::from_fn(|index| index < 16);
    let defaults = bootty_terminal::terminal_palette::default_base16();
    let background = resolved.background.map_or(defaults[0], color_to_rgb);
    let foreground = resolved.foreground.map_or(defaults[15], color_to_rgb);
    bootty_terminal::terminal_palette::generate_256_palette(
        &base,
        &skip,
        background,
        foreground,
        resolved.palette_harmonious,
    )
    .into_iter()
    .map(rgb_to_color)
    .collect()
}

const fn color_to_rgb(color: Color) -> RgbColor {
    RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

const fn rgb_to_color(color: RgbColor) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: u8::MAX,
    }
}
