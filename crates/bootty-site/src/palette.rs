//! Renderer color and palette demonstration content.

use tuirealm::ratatui::style::{Color, Modifier, Style};
use tuirealm::ratatui::text::{Line, Span};

use crate::content::Section;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DemoRgb {
    r: u8,
    g: u8,
    b: u8,
}

impl DemoRgb {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Clone, Copy)]
struct DemoLab {
    l: f32,
    a: f32,
    b: f32,
}

impl DemoLab {
    fn from_rgb(rgb: DemoRgb) -> Self {
        let r = srgb_to_linear(f32::from(rgb.r) / 255.0);
        let g = srgb_to_linear(f32::from(rgb.g) / 255.0);
        let b = srgb_to_linear(f32::from(rgb.b) / 255.0);
        let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47;
        let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
        let z = (0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b) / 1.088_83;
        let fx = xyz_to_lab_curve(x);
        let fy = xyz_to_lab_curve(y);
        let fz = xyz_to_lab_curve(z);
        Self {
            l: 116.0 * fy - 16.0,
            a: 500.0 * (fx - fy),
            b: 200.0 * (fy - fz),
        }
    }

    fn to_rgb(self) -> DemoRgb {
        let fy = (self.l + 16.0) / 116.0;
        let fx = self.a / 500.0 + fy;
        let fz = fy - self.b / 200.0;
        let x = 0.950_47 * lab_to_xyz_curve(fx, fx.powi(3));
        let y = lab_to_xyz_curve(fy, fy.powi(3));
        let z = 1.088_83 * lab_to_xyz_curve(fz, fz.powi(3));
        let r = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
        let g = -0.969_266 * x + 1.876_010_8 * y + 0.041_556 * z;
        let b = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;
        DemoRgb::new(
            linear_to_srgb_byte(r),
            linear_to_srgb_byte(g),
            linear_to_srgb_byte(b),
        )
    }

    fn mix(self, other: Self, t: f32) -> Self {
        Self {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }
}

pub(crate) fn palette_demo_lines(section: Section) -> Vec<Line<'static>> {
    let palette = harmonious_demo_palette();
    vec![
        Line::from(Span::styled(
            "Renderer color checks",
            Style::default()
                .fg(section.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        labeled_swatch_row("ANSI 0-7       ", &palette, 0..8),
        labeled_swatch_row("ANSI 8-15      ", &palette, 8..16),
        labeled_indices_row(
            "256-color cube ",
            &palette,
            &[
                16, 21, 51, 46, 82, 118, 154, 190, 226, 220, 214, 208, 202, 196,
            ],
        ),
        labeled_indices_row(
            "grayscale ramp ",
            &palette,
            &[232, 234, 236, 238, 240, 242, 244, 246, 248, 250, 252, 254],
        ),
        contrast_row(&palette),
    ]
}

fn labeled_swatch_row(
    label: &'static str,
    palette: &[DemoRgb; 256],
    range: std::ops::Range<usize>,
) -> Line<'static> {
    labeled_indices_row(label, palette, &(range.collect::<Vec<_>>()))
}

fn labeled_indices_row(
    label: &'static str,
    palette: &[DemoRgb; 256],
    indices: &[usize],
) -> Line<'static> {
    let mut spans = Vec::with_capacity(indices.len() + 1);
    spans.push(Span::styled(label, Style::default().fg(Color::DarkGray)));
    for &index in indices {
        spans.push(swatch(index, palette[index]));
    }
    Line::from(spans)
}

fn contrast_row(palette: &[DemoRgb; 256]) -> Line<'static> {
    Line::from(vec![
        Span::styled("foreground     ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " readable light text ",
            Style::default()
                .fg(rgb_color(DEMO_FOREGROUND))
                .bg(rgb_color(DEMO_BACKGROUND)),
        ),
        Span::raw("  "),
        Span::styled(
            " selected range ",
            Style::default()
                .fg(readable_palette_text(palette[99]))
                .bg(rgb_color(palette[99])),
        ),
    ])
}

fn swatch(index: usize, color: DemoRgb) -> Span<'static> {
    Span::styled(
        format!(" {index:02x} "),
        Style::default()
            .fg(readable_palette_text(color))
            .bg(rgb_color(color)),
    )
}

fn readable_palette_text(color: DemoRgb) -> Color {
    if color_luma(color) > 145_000 {
        Color::Black
    } else {
        Color::White
    }
}

fn color_luma(color: DemoRgb) -> u32 {
    u32::from(color.r) * 299 + u32::from(color.g) * 587 + u32::from(color.b) * 114
}

fn rgb_color(color: DemoRgb) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

fn harmonious_demo_palette() -> [DemoRgb; 256] {
    let base = demo_base16();
    let mut palette = [DEMO_FOREGROUND; 256];
    palette[..16].copy_from_slice(&base);

    let low = DemoLab::from_rgb(base[0]);
    let high = DemoLab::from_rgb(base[7]);
    for red in 0..6 {
        for green in 0..6 {
            for blue in 0..6 {
                let index = 16 + 36 * red + 6 * green + blue;
                let weighted = (red + green + blue) as f32 / 15.0;
                let anchor = low.mix(high, weighted);
                let accent = DemoLab::from_rgb(base[8 + ((red * 3 + green * 5 + blue * 7) % 8)]);
                palette[index] = anchor.mix(accent, 0.32).to_rgb();
            }
        }
    }

    for step in 0..24 {
        let t = step as f32 / 23.0;
        palette[232 + step] = low.mix(high, t).to_rgb();
    }
    palette
}

const DEMO_BACKGROUND: DemoRgb = DemoRgb::new(17, 18, 26);
const DEMO_FOREGROUND: DemoRgb = DemoRgb::new(192, 202, 245);

fn demo_base16() -> [DemoRgb; 16] {
    [
        DEMO_BACKGROUND,
        DemoRgb::new(247, 118, 142),
        DemoRgb::new(158, 206, 106),
        DemoRgb::new(224, 175, 104),
        DemoRgb::new(122, 162, 247),
        DemoRgb::new(187, 154, 247),
        DemoRgb::new(125, 207, 255),
        DEMO_FOREGROUND,
        DemoRgb::new(86, 95, 137),
        DemoRgb::new(255, 92, 170),
        DemoRgb::new(184, 242, 124),
        DemoRgb::new(255, 203, 107),
        DemoRgb::new(141, 185, 255),
        DemoRgb::new(203, 166, 247),
        DemoRgb::new(137, 220, 235),
        DemoRgb::new(220, 226, 255),
    ]
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn xyz_to_lab_curve(value: f32) -> f32 {
    if value > 0.008_856 {
        value.cbrt()
    } else {
        7.787 * value + 16.0 / 116.0
    }
}

fn lab_to_xyz_curve(value: f32, cubed: f32) -> f32 {
    if cubed > 0.008_856 {
        cubed
    } else {
        (value - 16.0 / 116.0) / 7.787
    }
}

fn linear_to_srgb_byte(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let srgb = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round() as u8
}
