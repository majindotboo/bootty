#![cfg(test)]

use bootty_config::{color::Color, config::ColorConfig};
use bootty_ui::ansi_palette::{standard_16_palette, xterm_256_palette};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn color(value: &str) -> Color {
    Color::from_hex(value).expect("valid test color")
}

#[rstest]
fn standard_preset_keeps_overrides_and_fills_from_the_resolved_theme() {
    let resolved = ColorConfig {
        palette: (0..16)
            .map(|index| Color {
                r: index,
                g: index.saturating_add(16),
                b: index.saturating_add(32),
                a: u8::MAX,
            })
            .collect(),
        ..ColorConfig::default()
    };

    let palette = standard_16_palette(&[color("#abcdef")], &resolved);

    assert_eq!(palette.len(), 16);
    assert_eq!(palette[0], color("#abcdef"));
    assert_eq!(palette[1], resolved.palette[1]);
    assert_eq!(palette[15], resolved.palette[15]);
}

#[rstest]
fn xterm_preset_preserves_standard_colors_and_resolves_all_256_indices() {
    let resolved = ColorConfig {
        palette: (0..16)
            .map(|index| Color {
                r: index,
                g: index.saturating_add(16),
                b: index.saturating_add(32),
                a: u8::MAX,
            })
            .collect(),
        background: Some(color("#101820")),
        foreground: Some(color("#d0d8e0")),
        palette_harmonious: true,
        ..ColorConfig::default()
    };

    let palette = xterm_256_palette(&[color("#abcdef")], &resolved);

    assert_eq!(palette.len(), 256);
    assert_eq!(palette[0], color("#abcdef"));
    assert_eq!(palette[1..16], resolved.palette[1..16]);
    assert_ne!(palette[16], palette[231]);
    assert_ne!(palette[232], palette[255]);
}
