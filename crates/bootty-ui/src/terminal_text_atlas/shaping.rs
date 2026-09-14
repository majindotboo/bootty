use rustybuzz::ttf_parser::Tag;
use rustybuzz::{BufferClusterLevel, Direction, Face, Feature, UnicodeBuffer};

use ab_glyph::{Font, FontArc, GlyphId};
use num_traits::ToPrimitive as _;

use super::clusters::{
    ShapedCluster, ShapedGlyph, is_combining_mark, is_default_emoji_presentation, is_private_use,
    is_symbol_like, is_variation_selector, with_shaped_cluster,
};
use crate::terminal_text::{FontFeature, terminal_char_width};

/// Shapes a run of text against a font's GSUB/GPOS tables via `HarfBuzz`
/// (`rustybuzz`). Ligatures and contextual alternates only form when the font
/// actually contains them, so a font without an "fi" ligature yields two
/// separate glyphs rather than a forced merge.
///
/// Returns `None` when the bytes do not parse as a usable face.
pub(super) fn shape_run(
    font_data: &[u8],
    face_index: u32,
    text: &str,
    font_size: f32,
    features: &[Feature],
) -> Option<Vec<ShapedGlyph>> {
    let face = Face::from_slice(font_data, face_index)?;
    let units_per_em = face.units_per_em().to_f32()?;
    if units_per_em <= 0.0 {
        return None;
    }
    let scale = font_size / units_per_em;

    let mut source = Vec::new();
    let mut buffer = UnicodeBuffer::new();
    buffer.set_cluster_level(BufferClusterLevel::Characters);
    buffer.set_direction(Direction::LeftToRight);
    u32::try_from(text.len()).ok()?;
    let mut source_index = 0_u32;
    crate::terminal_text::for_terminal_text_cells(text, |cell, cluster| {
        for (index, ch) in cluster.chars().enumerate() {
            buffer.add(ch, source_index);
            source_index = source_index.saturating_add(1);
            source.push(SourceCodepoint {
                cell,
                starts_cell: index == 0,
            });
        }
    });
    buffer.guess_segment_properties();

    let shaped = rustybuzz::shape(&face, features, buffer);
    let infos = shaped.glyph_infos();
    let positions = shaped.glyph_positions();
    if infos.len() != positions.len() {
        return None;
    }

    let mut run_offset_x = 0.0_f32;
    let mut run_offset_y = 0.0_f32;
    let mut run_offset_cell = 0_u16;
    let mut cell_offset_cell = 0_u16;
    let mut cell_offset_x = 0.0_f32;
    let mut glyphs = Vec::with_capacity(infos.len());

    for (info, position) in infos.iter().zip(positions) {
        let source_index = usize::try_from(info.cluster).ok()?;
        let codepoint = source.get(source_index)?;
        let glyph_cell = codepoint.cell;
        if cell_offset_cell != glyph_cell {
            let is_after_glyph_from_current_or_next_clusters = glyph_cell <= run_offset_cell;
            if codepoint.starts_cell && !is_after_glyph_from_current_or_next_clusters {
                cell_offset_cell = glyph_cell;
                cell_offset_x = run_offset_x;
            }
        }

        glyphs.push(ShapedGlyph {
            glyph_id: u16::try_from(info.glyph_id).ok()?,
            cluster: u32::from(cell_offset_cell),
            x_offset: position
                .x_offset
                .to_f32()?
                .mul_add(scale, run_offset_x - cell_offset_x),
            y_offset: position.y_offset.to_f32()?.mul_add(scale, run_offset_y),
        });

        run_offset_x = position.x_advance.to_f32()?.mul_add(scale, run_offset_x);
        run_offset_y = position.y_advance.to_f32()?.mul_add(scale, run_offset_y);
        run_offset_cell = run_offset_cell.max(glyph_cell);
    }

    Some(glyphs)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceCodepoint {
    cell: u16,
    starts_cell: bool,
}

/// Translates the user's [`FontFeature`] list into `HarfBuzz` features. The
/// `liga` setting acts as the single "ligatures" knob: disabling it also
/// disables the contextual/common ligature features that `HarfBuzz` would
/// otherwise apply by default, so `liga=0` turns ligatures off as a user
/// expects.
pub(super) fn harfbuzz_features(features: &[FontFeature]) -> Vec<Feature> {
    let mut out: Vec<Feature> = features
        .iter()
        .map(|feature| Feature::new(Tag::from_bytes(&feature.tag()), feature.value(), ..))
        .collect();
    if !ligatures_enabled(features) {
        for tag in [b"calt", b"clig", b"liga", b"rlig", b"dlig"] {
            out.push(Feature::new(Tag::from_bytes(tag), 0, ..));
        }
    }
    out
}

/// Whether the font's GSUB table advertises any feature that can substitute or
/// merge glyphs in horizontal text. Fonts without these (e.g. Menlo, SF Mono)
/// keep the cheaper per-character render paths.
pub(super) fn font_has_ligature_features(font_data: &[u8], face_index: u32) -> bool {
    let Some(face) = Face::from_slice(font_data, face_index) else {
        return false;
    };
    let Some(gsub) = face.tables().gsub else {
        return false;
    };
    gsub.features.into_iter().any(|feature| {
        matches!(
            &feature.tag.to_bytes(),
            b"liga" | b"clig" | b"calt" | b"rlig" | b"dlig"
        )
    })
}

fn ligatures_enabled(features: &[FontFeature]) -> bool {
    features
        .iter()
        .rev()
        .find(|feature| feature.tag() == *b"liga")
        .is_none_or(|feature| feature.value() != 0)
}

/// Shapes `text` with the primary font and emits cell-aligned clusters,
/// attaching shaped glyph ids to ligature/contextual clusters. Returns
/// `None` when the font has no ligature features, so the caller can keep the
/// cheaper per-character paths.
pub(super) fn shape_clusters(
    database: &fontdb::Database,
    id: fontdb::ID,
    font: &FontArc,
    text: &str,
    font_size: f32,
    features: &[FontFeature],
    clusters: &mut Vec<ShapedCluster>,
) -> Option<(u16, usize)> {
    let hb_features = harfbuzz_features(features);
    let glyphs = database
        .with_face_data(id, |data, index| {
            shape_run(data, index, text, font_size, &hb_features)
        })
        .flatten()?;
    let source_ranges = text_byte_ranges_by_cell(text);
    let total_cells = u16::try_from(source_ranges.len()).unwrap_or(u16::MAX);
    let mut cluster_index = 0_usize;
    let mut glyph_index = 0;
    while glyph_index < glyphs.len() {
        let group = shaped_glyph_group(
            text,
            &source_ranges,
            &glyphs,
            glyph_index,
            total_cells,
            font,
        )?;
        let mut glyph_end = group.end;
        let mut cells = group.cells;
        let mut source_end = group.source_end;
        let draw_by_glyph = group.draw_by_glyph;

        if draw_by_glyph {
            while glyph_end < glyphs.len() {
                let next_group = shaped_glyph_group(
                    text,
                    &source_ranges,
                    &glyphs,
                    glyph_end,
                    total_cells,
                    font,
                )?;
                if !next_group.draw_by_glyph || next_group.cell != group.cell.saturating_add(cells)
                {
                    break;
                }
                glyph_end = next_group.end;
                cells = cells.saturating_add(next_group.cells);
                source_end = next_group.source_end;
            }
        }

        let slice = text.get(group.source_start..source_end)?;
        let shaped = glyphs.get(glyph_index..glyph_end)?;
        with_shaped_cluster(clusters, cluster_index, |cluster| {
            cluster.text.clear();
            cluster.glyphs.clear();
            cluster.text.push_str(slice);
            cluster.cell = group.cell;
            cluster.is_whitespace = slice.chars().all(char::is_whitespace);
            cluster.cells = cells;
            if draw_by_glyph {
                cluster.glyphs.extend(shaped.iter().copied());
            }
        });
        cluster_index = cluster_index.checked_add(1)?;
        glyph_index = glyph_end;
    }
    Some((total_cells.max(1), cluster_index))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShapedGlyphGroup {
    cell: u16,
    cells: u16,
    source_start: usize,
    source_end: usize,
    end: usize,
    draw_by_glyph: bool,
}

fn shaped_glyph_group(
    text: &str,
    source_ranges: &[std::ops::Range<usize>],
    glyphs: &[ShapedGlyph],
    start: usize,
    total_cells: u16,
    font: &FontArc,
) -> Option<ShapedGlyphGroup> {
    let cell = glyphs.get(start)?.cluster;
    let mut end = start.checked_add(1)?;
    while glyphs.get(end).is_some_and(|glyph| glyph.cluster == cell) {
        end = end.checked_add(1)?;
    }

    let cell = u16::try_from(cell).unwrap_or(u16::MAX);
    let next_cell = glyphs.get(end).map_or(total_cells, |glyph| {
        u16::try_from(glyph.cluster).unwrap_or(u16::MAX)
    });
    let cells = next_cell.saturating_sub(cell).max(1);
    let source_start = source_ranges
        .get(usize::from(cell))
        .map_or(0, |range| range.start);
    let source_end = source_ranges
        .get(usize::from(cell.saturating_add(cells).saturating_sub(1)))
        .map_or(text.len(), |range| range.end);
    let slice = text.get(source_start..source_end)?;

    Some(ShapedGlyphGroup {
        cell,
        cells,
        source_start,
        source_end,
        end,
        draw_by_glyph: draw_span_by_glyph(slice, glyphs.get(start..end)?, font),
    })
}

// Build each cell's source range once. Glyph grouping then takes constant time
// instead of rescanning the entire UTF-8 run for every ordinary character.
fn text_byte_ranges_by_cell(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut previous = 0..0;
    let mut byte_start = 0_usize;
    let cells = crate::terminal_text::for_terminal_text_cells(text, |cell, cluster| {
        ranges.resize(usize::from(cell), previous.clone());
        let byte_end = byte_start.saturating_add(cluster.len());
        previous = byte_start..byte_end;
        byte_start = byte_end;
    });
    ranges.resize(usize::from(cells), previous);
    ranges
}

/// Whether a shaped span should be drawn directly from its glyph ids rather than
/// the per-character path. True only for genuine font output the per-character
/// path cannot reproduce: a ligature (several source cells shaped together) or a
/// single character the font swapped for a contextual alternate. Everything with
/// dedicated handling (whitespace, private-use icons, symbols/box-drawing, emoji,
/// or any uncovered `.notdef` glyph) stays on the legacy path.
fn draw_span_by_glyph(slice: &str, glyphs: &[ShapedGlyph], font: &FontArc) -> bool {
    if glyphs.is_empty() || glyphs.iter().any(|glyph| glyph.glyph_id == 0) {
        return false;
    }
    if slice.chars().any(|ch| {
        ch.is_whitespace()
            || is_private_use(ch)
            || is_symbol_like(ch)
            || is_variation_selector(ch)
            || ch == '\u{fe0f}'
            || is_default_emoji_presentation(ch)
    }) {
        return false;
    }
    let width_chars = slice
        .chars()
        .filter(|ch| terminal_char_width(*ch) >= 1)
        .count();
    if width_chars >= 2 || slice.chars().any(is_combining_mark) {
        return true;
    }
    let [glyph] = glyphs else {
        return false;
    };
    width_chars == 1
        && slice
            .chars()
            .next()
            .is_some_and(|ch| GlyphId(glyph.glyph_id) != font.glyph_id(ch))
}
