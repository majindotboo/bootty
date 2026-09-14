use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use ab_glyph::{Font, FontArc, ScaleFont};
use bootty_terminal::geometry::{
    CellMetrics, DEFAULT_CELL_WIDTH, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT,
};

use crate::{
    font_database::{font_pixel_scale, load_matching_font, system_font_database},
    paint_plan::{PlanColor, TextAttrs},
    terminal_text::{ResolvedFontFace, TerminalTextConfig},
};

const GHOSTTY_CONFIG_CELL_HEIGHT_ADJUSTMENT: f32 = 1.45;

#[must_use]
pub fn terminal_text_cell_metrics(config: &TerminalTextConfig, display_scale: f32) -> CellMetrics {
    let display_scale = if display_scale.is_finite() {
        display_scale.max(1.0)
    } else {
        1.0
    };
    let face = crate::terminal_text::FontResolver::new(config.clone()).resolve_face(&TextAttrs {
        fg: PlanColor {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    });
    let ratio = config.font_size.max(1.0) / DEFAULT_FONT_SIZE;
    let mut cell = terminal_font(&face).map_or_else(
        || CellMetrics::new(DEFAULT_CELL_WIDTH * ratio, DEFAULT_LINE_HEIGHT * ratio),
        |font| {
            let physical = ghostty_cell_metrics_from_font(&font, config.font_size * display_scale);
            CellMetrics::new(
                physical.width / display_scale,
                physical.height / display_scale,
            )
        },
    );

    if let Some(width) = config.cell_width {
        cell.width = width.max(1.0);
    }
    if let Some(height) = config.cell_height {
        cell.height = height.max(1.0);
    }
    cell
}

pub(crate) fn terminal_underline_offset(
    config: &TerminalTextConfig,
    cell_height: f32,
    display_scale: f32,
) -> f32 {
    let face = crate::terminal_text::FontResolver::new(config.clone()).resolve_face(&TextAttrs {
        fg: PlanColor {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    });
    let display_scale = display_scale.max(1.0);
    let baseline = terminal_font(&face).map_or(config.font_size * display_scale, |font| {
        let scaled = font.as_scaled(font_pixel_scale(&font, config.font_size * display_scale));
        (((cell_height * display_scale).ceil() - scaled.height()) * 0.5).max(0.0) + scaled.ascent()
    });
    (baseline - (config.baseline_adjustment * display_scale).round()) / display_scale
        + config.underline_position
}

fn terminal_font(face: &ResolvedFontFace) -> Option<FontArc> {
    static FONT_CACHE: OnceLock<Mutex<TerminalFontCache>> = OnceLock::new();
    FONT_CACHE
        .get_or_init(|| Mutex::new(TerminalFontCache::new()))
        .lock()
        .ok()?
        .font_for_face(face)
}

fn ghostty_cell_metrics_from_font(font: &FontArc, font_size: f32) -> CellMetrics {
    let scale = font_pixel_scale(font, font_size);
    let scaled = font.as_scaled(scale);
    let face_width = (' '..='~')
        .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
        .fold(0.0_f32, f32::max);
    let face_height = scaled.height() + scaled.line_gap();
    CellMetrics::new(
        face_width.round().max(1.0),
        (face_height.round() * GHOSTTY_CONFIG_CELL_HEIGHT_ADJUSTMENT)
            .round()
            .max(1.0),
    )
}

struct TerminalFontCache {
    database: &'static fontdb::Database,
    fonts: HashMap<ResolvedFontFace, Option<FontArc>>,
}

impl TerminalFontCache {
    fn new() -> Self {
        Self {
            database: system_font_database(),
            fonts: HashMap::new(),
        }
    }

    fn font_for_face(&mut self, face: &ResolvedFontFace) -> Option<FontArc> {
        let database = self.database;
        self.fonts
            .entry(face.clone())
            .or_insert_with(|| load_terminal_font(database, face))
            .clone()
    }
}

fn load_terminal_font(database: &fontdb::Database, face: &ResolvedFontFace) -> Option<FontArc> {
    for family in terminal_font_family_priority(face) {
        if family == "monospace" {
            if let Some(font) =
                load_matching_font(database, &[fontdb::Family::Monospace], face.style)
            {
                return Some(font);
            }
        } else if let Some(font) =
            load_matching_font(database, &[fontdb::Family::Name(&family)], face.style)
        {
            return Some(font);
        }
    }
    load_matching_font(database, &[fontdb::Family::Monospace], face.style)
}

fn terminal_font_family_priority(face: &ResolvedFontFace) -> Vec<String> {
    let mut families = Vec::new();
    push_family(&mut families, &face.family);
    for family in &face.fallback_families {
        push_family(&mut families, family);
    }
    for family in [
        "JetBrains Mono",
        "JetBrainsMono Nerd Font Mono",
        "JetBrainsMono Nerd Font",
        "Symbols Nerd Font Mono",
    ] {
        push_family(&mut families, family);
    }
    push_family(&mut families, "monospace");
    families
}

fn push_family(families: &mut Vec<String>, family: &str) {
    if !families.iter().any(|existing| existing == family) {
        families.push(family.to_owned());
    }
}
