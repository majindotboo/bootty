use crate::{
    paint_plan::{TerminalPaintPlan, TextAttrs, TextRun},
    terminal_sprite::SpriteGlyph,
};
pub use bootty_config::FontFeature;
use bootty_config::FontStyleAssignment;
use bootty_terminal::geometry::{
    CellMetrics, DEFAULT_FONT_SIZE, TerminalPadding, fit_cell_height_to_available_space,
    fit_cell_width_to_available_space,
};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTextConfig {
    pub families: Vec<String>,
    pub style_bold: FontStyleAssignment,
    pub style_italic: FontStyleAssignment,
    pub style_bold_italic: FontStyleAssignment,
    pub font_features: Vec<FontFeature>,
    pub codepoint_overrides: CodepointFontMap,
    pub font_size: f32,
    pub cell_width: Option<f32>,
    pub cell_height: Option<f32>,
    pub fit_cell_height: bool,
    pub fit_cell_width: bool,
    pub baseline_adjustment: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
}

impl Default for TerminalTextConfig {
    fn default() -> Self {
        Self {
            families: vec!["monospace".to_owned()],
            style_bold: FontStyleAssignment::Automatic,
            style_italic: FontStyleAssignment::Automatic,
            style_bold_italic: FontStyleAssignment::Automatic,
            font_features: default_font_features(),
            codepoint_overrides: CodepointFontMap::default(),
            font_size: DEFAULT_FONT_SIZE,
            cell_width: None,
            cell_height: None,
            fit_cell_height: true,
            fit_cell_width: false,
            baseline_adjustment: 0.0,
            underline_position: 2.0,
            underline_thickness: 1.0,
        }
    }
}

impl TerminalTextConfig {
    #[must_use]
    pub fn with_cell_metrics(cell: CellMetrics) -> Self {
        Self {
            cell_width: Some(cell.width),
            cell_height: Some(cell.height),
            ..Self::default()
        }
    }
}

/// The terminal grid pitch and glyph ink box derived from one base font contract.
///
/// Fitting may distribute spare viewport space through the grid pitch, but it must not stretch the
/// Ghostty-derived cell used to rasterize and vertically position glyph ink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalTextGeometry {
    pub grid_cell: CellMetrics,
    pub ink_cell: CellMetrics,
}

impl TerminalTextGeometry {
    #[must_use]
    pub fn fitted(
        config: &TerminalTextConfig,
        available_width: f32,
        available_height: f32,
        base_cell: CellMetrics,
        padding: TerminalPadding,
    ) -> Self {
        let mut grid_cell = base_cell;
        if config.fit_cell_height {
            grid_cell = fit_cell_height_to_available_space(available_height, grid_cell, padding);
        }
        if config.fit_cell_width {
            grid_cell = fit_cell_width_to_available_space(available_width, grid_cell, padding);
        }
        Self {
            grid_cell,
            ink_cell: base_cell,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodepointFontMap {
    entries: Vec<CodepointFontEntry>,
}

impl CodepointFontMap {
    /// Add an override for a nonempty range. Empty ranges have no effect.
    pub fn add(&mut self, range: std::ops::RangeInclusive<char>, family: impl Into<String>) {
        let start = u32::from(*range.start());
        let end = u32::from(*range.end());
        if range.is_empty() {
            return;
        }
        self.entries.push(CodepointFontEntry {
            start,
            end,
            family: family.into(),
        });
    }

    #[must_use]
    pub fn family_for(&self, ch: char) -> Option<&str> {
        let codepoint = u32::from(ch);
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.start <= codepoint && codepoint <= entry.end)
            .map(|entry| entry.family.as_str())
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CodepointFontEntry {
    start: u32,
    end: u32,
    family: String,
}

#[must_use]
pub fn default_font_features() -> Vec<FontFeature> {
    vec![FontFeature::new(*b"liga", 1)]
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontResolver {
    config: TerminalTextConfig,
    default_faces: [Arc<ResolvedFontFace>; 4],
}

impl FontResolver {
    #[must_use]
    pub fn new(config: TerminalTextConfig) -> Self {
        let default_faces = std::array::from_fn(|index| {
            Arc::new(resolve_face_for_char_and_style(
                &config,
                None,
                FontStyle::from_index(index),
            ))
        });
        Self {
            config,
            default_faces,
        }
    }

    #[must_use]
    pub fn resolve_face(&self, attrs: &TextAttrs) -> ResolvedFontFace {
        self.resolve_face_handle(attrs, None).as_ref().clone()
    }

    #[must_use]
    pub fn resolve_face_handle_for_text(
        &self,
        attrs: &TextAttrs,
        text: &str,
    ) -> Arc<ResolvedFontFace> {
        self.resolve_face_handle(attrs, text.chars().find(|ch| terminal_char_width(*ch) > 0))
    }

    fn resolve_face_handle(&self, attrs: &TextAttrs, ch: Option<char>) -> Arc<ResolvedFontFace> {
        let style = FontStyle::from_attrs(attrs);
        if ch
            .and_then(|ch| self.config.codepoint_overrides.family_for(ch))
            .is_none()
        {
            let [regular, bold, italic, bold_italic] = &self.default_faces;
            return Arc::clone(match style {
                FontStyle::Regular => regular,
                FontStyle::Bold => bold,
                FontStyle::Italic => italic,
                FontStyle::BoldItalic => bold_italic,
            });
        }
        Arc::new(resolve_face_for_char_and_style(&self.config, ch, style))
    }
}

fn resolve_face_for_char_and_style(
    config: &TerminalTextConfig,
    ch: Option<char>,
    style: FontStyle,
) -> ResolvedFontFace {
    let mut families = config.families.iter();
    let default_family = families
        .next()
        .cloned()
        .unwrap_or_else(|| "monospace".to_owned());
    let override_family = ch.and_then(|ch| config.codepoint_overrides.family_for(ch));
    let (family, fallback_families) = match override_family {
        Some(family) => (
            family.to_owned(),
            std::iter::once(default_family)
                .chain(families.cloned())
                .collect(),
        ),
        None => (default_family, families.cloned().collect()),
    };
    ResolvedFontFace {
        family,
        fallback_families,
        style,
        assignment: match style {
            FontStyle::Regular => FontStyleAssignment::Automatic,
            FontStyle::Bold => config.style_bold.clone(),
            FontStyle::Italic => config.style_italic.clone(),
            FontStyle::BoldItalic => config.style_bold_italic.clone(),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResolvedFontFace {
    pub family: String,
    pub fallback_families: Vec<String>,
    pub style: FontStyle,
    pub assignment: FontStyleAssignment,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    const fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Regular,
            1 => Self::Bold,
            2 => Self::Italic,
            _ => Self::BoldItalic,
        }
    }

    const fn from_attrs(attrs: &TextAttrs) -> Self {
        match (attrs.bold, attrs.italic) {
            (true, true) => Self::BoldItalic,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (false, false) => Self::Regular,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Terminal sprite families are independent policy switches."
)]
pub struct NativeSymbolPolicy {
    blocks: bool,
    shades: bool,
    quadrants: bool,
    box_drawing: bool,
    powerline: bool,
    progress_indicators: bool,
    separators: bool,
    braille: bool,
    legacy: bool,
    special: bool,
}

impl NativeSymbolPolicy {
    const fn all(enabled: bool) -> Self {
        Self {
            blocks: enabled,
            shades: enabled,
            quadrants: enabled,
            box_drawing: enabled,
            powerline: enabled,
            progress_indicators: enabled,
            separators: enabled,
            braille: enabled,
            legacy: enabled,
            special: enabled,
        }
    }

    #[must_use]
    pub const fn font_only() -> Self {
        Self::all(false)
    }

    #[must_use]
    pub const fn terminal_glyph_primitives() -> Self {
        Self::all(true)
    }
    #[must_use]
    pub const fn classify(self, ch: char) -> Option<NativeSymbolClass> {
        let class = match ch {
            '▀'..='▐' | '▔' | '▕' if self.blocks => NativeSymbolClass::Block,
            '░' | '▒' | '▓' if self.shades => NativeSymbolClass::Shade,
            '▖'..='▟' if self.quadrants => NativeSymbolClass::Quadrant,
            '─'..='╿' if self.box_drawing => NativeSymbolClass::BoxDrawing,
            '\u{E0B0}'..='\u{E0D7}' if self.powerline => NativeSymbolClass::Powerline,
            '\u{EE00}'..='\u{EE0B}' if self.progress_indicators => {
                NativeSymbolClass::ProgressIndicator
            }
            '❯' | '❮' | '' | '' if self.separators => NativeSymbolClass::Separator,
            '\u{2800}'..='\u{28FF}' if self.braille => NativeSymbolClass::Braille,
            '\u{1FB00}'..='\u{1FBFF}' if self.legacy => NativeSymbolClass::LegacyComputing,
            '\u{1CC00}'..='\u{1CEBF}' if self.legacy => {
                NativeSymbolClass::LegacyComputingSupplement
            }
            '\u{F5D0}'..='\u{F60D}' if self.special => NativeSymbolClass::Special,
            _ => return None,
        };
        Some(class)
    }
}

impl Default for NativeSymbolPolicy {
    fn default() -> Self {
        Self::terminal_glyph_primitives()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeSymbolClass {
    Block,
    Shade,
    Quadrant,
    BoxDrawing,
    Powerline,
    ProgressIndicator,
    Separator,
    Braille,
    LegacyComputing,
    LegacyComputingSupplement,
    Special,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTextContract {
    pub(crate) config: TerminalTextConfig,
    pub(crate) font_features: Arc<[FontFeature]>,
    pub(crate) resolver: FontResolver,
    pub(crate) native_symbol_policy: NativeSymbolPolicy,
}

impl TerminalTextContract {
    #[must_use]
    pub fn new(config: TerminalTextConfig, native_symbol_policy: NativeSymbolPolicy) -> Self {
        let resolver = FontResolver::new(config.clone());
        let font_features = Arc::from(config.font_features.clone());
        Self {
            config,
            font_features,
            resolver,
            native_symbol_policy,
        }
    }

    #[must_use]
    pub fn for_terminal_paint_plan(
        plan: &TerminalPaintPlan,
        base_config: &TerminalTextConfig,
    ) -> Self {
        Self::new(
            terminal_text_config_for_plan(plan, base_config),
            NativeSymbolPolicy::terminal_glyph_primitives(),
        )
    }

    #[must_use]
    pub fn resolve_face_handle_for_run(&self, run: &TextRun) -> Arc<ResolvedFontFace> {
        self.resolver
            .resolve_face_handle_for_text(&run.attrs, &run.text)
    }

    #[must_use]
    pub fn has_native_symbol_fragments(&self, text: &str) -> bool {
        if text.is_ascii() {
            return false;
        }
        text.chars()
            .any(|ch| self.native_symbol_glyph(ch).is_some())
    }

    #[must_use]
    pub fn native_symbol_glyph(&self, ch: char) -> Option<SpriteGlyph> {
        self.native_symbol_policy.classify(ch)?;
        SpriteGlyph::from_char(ch)
    }

    #[must_use]
    pub const fn baseline_adjustment(&self) -> f32 {
        self.config.baseline_adjustment
    }
}

#[must_use]
pub fn terminal_text_config_for_plan(
    plan: &TerminalPaintPlan,
    base_config: &TerminalTextConfig,
) -> TerminalTextConfig {
    plan.text_runs.first().map_or_else(
        || base_config.clone(),
        |run| TerminalTextConfig {
            cell_width: Some(run.rect.width() / f32::from(run.cells.max(1))),
            cell_height: Some(run.rect.height()),
            ..base_config.clone()
        },
    )
}

#[must_use]
pub fn terminal_char_width(ch: char) -> u16 {
    u16::try_from(UnicodeWidthChar::width(ch).unwrap_or(0)).unwrap_or(u16::MAX)
}

/// Cells occupied by one grapheme payload from the terminal grid. Measure the whole
/// sequence so joined emoji, modifiers, and presentation selectors keep their width.
#[must_use]
pub fn terminal_grapheme_cells(chars: &[char]) -> u16 {
    match chars {
        [] => 1,
        [ch] => terminal_char_width(*ch).max(1),
        _ => {
            // Ordinary cells take the scalar path; short graphemes fit in the stack buffer.
            let mut bytes = smallvec::SmallVec::<[u8; 32]>::new();
            for ch in chars {
                bytes.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            }
            std::str::from_utf8(&bytes).map_or(1, |text| {
                u16::try_from(text.width()).unwrap_or(u16::MAX).max(1)
            })
        }
    }
}

/// Visit terminal graphemes with their starting cells and return the total width.
pub fn for_terminal_text_cells(text: &str, mut emit: impl FnMut(u16, &str)) -> u16 {
    let mut cell = 0_u16;
    for grapheme in text.graphemes(true) {
        emit(cell, grapheme);
        cell = cell.saturating_add(u16::try_from(grapheme.width()).unwrap_or(u16::MAX).max(1));
    }
    cell
}
