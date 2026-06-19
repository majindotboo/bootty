use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face, Feature, UnicodeBuffer};
use smallvec::SmallVec;

use crate::terminal_text::FontFeature;

/// One glyph produced by shaping a text run. Glyph ids index the same font face
/// that ab_glyph loads from the identical bytes, so they can be rasterized
/// directly via [`ab_glyph::GlyphId`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShapedGlyph {
    pub glyph_id: u16,
    /// Byte offset into the shaped text of the source cluster this glyph covers.
    /// HarfBuzz assigns ligatures the cluster of their first source character.
    pub cluster: u32,
    /// Horizontal advance in pixels at the requested font size.
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// A contiguous source-text span produced by shaping, expressed as a byte range
/// plus the glyphs that render it. A ligature span covers several source
/// characters with fewer glyphs; a decomposition covers one character with
/// several glyphs.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ShapedSpan {
    pub start: usize,
    pub end: usize,
    pub glyphs: SmallVec<[ShapedGlyph; 2]>,
}

/// Shapes a run of text against a font's GSUB/GPOS tables via HarfBuzz
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
    let units_per_em = face.units_per_em() as f32;
    if units_per_em <= 0.0 {
        return None;
    }
    let scale = font_size / units_per_em;

    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();

    let shaped = rustybuzz::shape(&face, features, buffer);
    let infos = shaped.glyph_infos();
    let positions = shaped.glyph_positions();

    Some(
        infos
            .iter()
            .zip(positions)
            .map(|(info, position)| ShapedGlyph {
                glyph_id: info.glyph_id as u16,
                cluster: info.cluster,
                x_advance: position.x_advance as f32 * scale,
                x_offset: position.x_offset as f32 * scale,
                y_offset: position.y_offset as f32 * scale,
            })
            .collect(),
    )
}

/// Translates the user's [`FontFeature`] list into HarfBuzz features. The
/// `liga` setting acts as the single "ligatures" knob: disabling it also
/// disables the contextual/common ligature features that HarfBuzz would
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

/// Partitions shaped glyphs into source spans using HarfBuzz cluster values.
/// Glyphs sharing a cluster belong to one span, which covers source bytes
/// `[cluster, next_distinct_cluster)`. Assumes left-to-right, monotonically
/// non-decreasing clusters (HarfBuzz's default for terminal text).
pub(super) fn shaped_spans(text: &str, glyphs: &[ShapedGlyph]) -> Vec<ShapedSpan> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < glyphs.len() {
        let start = glyphs[index].cluster as usize;
        let mut next = index + 1;
        while next < glyphs.len() && glyphs[next].cluster as usize == start {
            next += 1;
        }
        let end = glyphs
            .get(next)
            .map_or(text.len(), |glyph| glyph.cluster as usize);
        spans.push(ShapedSpan {
            start,
            end,
            glyphs: glyphs[index..next].iter().copied().collect(),
        });
        index = next;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_text::default_font_features;

    fn maple_mono() -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/fonts/MapleMono-wght.ttf");
        std::fs::read(path).expect("fixture font reads")
    }

    fn glyph(glyph_id: u16, cluster: u32) -> ShapedGlyph {
        ShapedGlyph {
            glyph_id,
            cluster,
            x_advance: 9.6,
            x_offset: 0.0,
            y_offset: 0.0,
        }
    }

    #[test]
    fn shapes_one_glyph_per_plain_character_with_byte_clusters() {
        let data = maple_mono();
        let features = harfbuzz_features(&default_font_features());
        let glyphs = shape_run(&data, 0, "abc", 16.0, &features).unwrap();

        assert_eq!(glyphs.len(), 3);
        assert_eq!(
            glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(
            glyphs.iter().all(|g| g.glyph_id != 0 && g.x_advance > 0.0),
            "every plain glyph resolves with a positive advance: {glyphs:?}"
        );
    }

    #[test]
    fn font_without_fi_ligature_keeps_two_glyphs() {
        // The bundled Maple Mono build has no "fi" ligature. A real shaper must
        // surface that as two glyphs instead of forcing a merge (the bug).
        let data = maple_mono();
        let features = harfbuzz_features(&default_font_features());
        let glyphs = shape_run(&data, 0, "fi", 16.0, &features).unwrap();
        assert_eq!(glyphs.len(), 2, "no ligature in the font: {glyphs:?}");
        assert_eq!(glyphs[0].cluster, 0);
        assert_eq!(glyphs[1].cluster, 1);
    }

    #[test]
    fn disabling_liga_also_disables_contextual_ligatures() {
        let off = harfbuzz_features(&[FontFeature::new(*b"liga", 0)]);
        let tags: Vec<[u8; 4]> = off
            .iter()
            .map(|f| f.tag.to_bytes())
            .filter(|tag| matches!(tag, b"calt" | b"clig" | b"rlig" | b"dlig"))
            .collect();
        assert!(
            tags.contains(b"calt") && tags.contains(b"clig"),
            "liga=0 forces calt/clig off too: {tags:?}"
        );
    }

    #[test]
    fn ligature_span_covers_all_source_bytes_of_one_glyph() {
        // Synthetic: a single ligature glyph covering the two chars of "fi".
        let spans = shaped_spans("fi", &[glyph(900, 0)]);
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start, spans[0].end), (0, 2));
        assert_eq!(spans[0].glyphs.len(), 1);
    }

    #[test]
    fn plain_glyphs_split_into_one_span_per_character() {
        let spans = shaped_spans("ab", &[glyph(10, 0), glyph(11, 1)]);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].end), (0, 1));
        assert_eq!((spans[1].start, spans[1].end), (1, 2));
    }

    #[test]
    fn decomposition_groups_multiple_glyphs_into_one_span() {
        // One source char shaped into two glyphs (e.g. base + mark): both share
        // the cluster and stay in a single span.
        let spans = shaped_spans("x", &[glyph(20, 0), glyph(21, 0)]);
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start, spans[0].end), (0, 1));
        assert_eq!(spans[0].glyphs.len(), 2);
    }
}
