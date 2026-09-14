#![cfg(test)]

use std::sync::Arc;

use bootty_terminal::geometry::{DEFAULT_FONT_SIZE, SurfaceRect};
use bootty_ui::{
    paint_plan::PlanColor,
    terminal_render::{SpriteCommandBatch, TextCommand},
    terminal_sprite::SpriteGlyph,
    terminal_text::{FontFeature, FontStyle, ResolvedFontFace},
    terminal_text_atlas::{
        GlyphAtlas, GlyphAtlasError, GlyphAtlasFaceKey, GlyphAtlasFormat, GlyphAtlasKey,
        GlyphAtlasTextKey, ShapedCluster, TerminalTextShaper, TextAtlasBuilder, TexturedGlyphQuad,
    },
};
use num_traits::ToPrimitive as _;
use pretty_assertions::assert_eq;
use rstest::rstest;

fn cluster_layout(clusters: &[ShapedCluster]) -> Vec<(&str, u16, u16, bool)> {
    clusters
        .iter()
        .map(|cluster| {
            (
                cluster.text.as_str(),
                cluster.cell,
                cluster.cells,
                cluster.is_whitespace,
            )
        })
        .collect()
}

fn text_command(text: &str, width: f32) -> TextCommand {
    TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, width, 20.0),
        text: text.to_owned(),
        attrs: attrs(),
        face: Arc::new(face()),
        font_size: DEFAULT_FONT_SIZE,
        cell_width: width
            / text
                .chars()
                .count()
                .max(1)
                .to_f32()
                .expect("text length fits in f32"),
        font_features: Arc::from(vec![FontFeature::new(*b"liga", 1)]),
    }
}

#[test]
fn text_shaper_groups_combining_emoji_and_variation_clusters() {
    let shaper = TerminalTextShaper;

    let clusters = shaper.shape("fi e\u{301} 😀 \u{2764}\u{FE0F}", 1);

    // Plain adjacent letters stay one cluster each. Ligatures are decided later
    // from the font's own tables, not forced by this font-agnostic segmenter.
    assert!(clusters.iter().any(|cluster| cluster.text == "f"));
    assert!(clusters.iter().any(|cluster| cluster.text == "i"));
    assert!(clusters.iter().any(|cluster| cluster.text == "e\u{301}"));
    assert!(
        clusters
            .iter()
            .any(|cluster| cluster.text == "😀" && cluster.cells == 2)
    );
    // A VS16 emoji presentation sequence (❤️) is one grapheme spanning two cells, matching
    // libghostty's grid under grapheme-cluster mode. The selector must not split off.
    assert!(
        clusters
            .iter()
            .any(|cluster| cluster.text == "\u{2764}\u{FE0F}" && cluster.cells == 2)
    );
}

#[rstest]
#[case::accent("e\u{301}", 1)]
#[case::arabic_mark("م\u{064e}", 1)]
#[case::joined_family("👨‍👩‍👧‍👦", 2)]
#[case::skin_tone("👍🏽", 2)]
#[case::text_presentation("☔\u{fe0e}", 1)]
#[case::wide_emoji("😀\u{fe0f}", 2)]
#[case::repeated_selector("😀\u{fe0f}\u{fe0f}", 2)]
fn attached_marks_preserve_following_cell_positions(#[case] text: &str, #[case] cells: u16) {
    let shaper = TerminalTextShaper;
    let clusters = shaper.shape(&format!("{text}A"), 3);
    assert_eq!(
        cluster_layout(&clusters),
        vec![
            (text, 3, cells, false,),
            (
                "A",
                3_u16.checked_add(cells).expect("cell position fits"),
                1,
                false
            )
        ]
    );
}

#[test]
fn text_shaper_shape_into_replaces_previous_clusters() {
    let shaper = TerminalTextShaper;
    let mut clusters = shaper.shape("stale", 0);

    let total_cells = shaper.shape_into("A界e\u{301}", 4, &mut clusters);

    assert_eq!(total_cells, 4);
    assert_eq!(
        cluster_layout(&clusters),
        vec![
            ("A", 4, 1, false),
            ("界", 5, 2, false),
            ("e\u{301}", 7, 1, false),
        ]
    );

    let total_cells = shaper.shape_into("fi", 0, &mut clusters);

    assert_eq!(total_cells, 2);
    assert_eq!(clusters.len(), 2);
    assert_eq!(clusters[0].text, "f");
    assert_eq!(clusters[1].text, "i");
}

#[rstest]
#[case::arabic("مَرْحَبًا")]
#[case::devanagari("कर्म")]
#[case::tai_tham("ᨠᩣ")]
#[case::tibetan("བོད")]
#[case::javanese("ꦲꦤ")]
#[case::chakma("𑄇𑄧")]
#[case::bengali("কিরণ")]
fn complex_scripts_produce_ordered_nonzero_terminal_cells(#[case] text: &str) {
    let shaper = TerminalTextShaper;
    let clusters = shaper.shape(text, 0);

    assert!(!clusters.is_empty(), "{text:?} produced no clusters");
    assert_eq!(clusters[0].cell, 0, "{text:?} did not start at cell zero");
    assert!(
        clusters.iter().all(|cluster| cluster.cells >= 1),
        "{text:?} produced a zero-width terminal cluster"
    );
    assert_cells_are_monotonic(&clusters, text);
}

#[test]
fn text_shaper_preserves_spaces_emoji_variants_and_symbols() {
    let shaper = TerminalTextShaper;

    let clusters = shaper.shape("a  b", 3);
    assert_eq!(clusters[0].cell, 3);
    assert_eq!(clusters[1].text, " ");
    assert_eq!(clusters[2].text, " ");
    assert_eq!(clusters[3].cell, 6);
    assert_cells_are_monotonic(&clusters, "empty cells with background");

    let emoji = shaper.shape("🥸🥸", 0);
    assert_eq!(emoji.len(), 2);
    assert!(emoji.iter().all(|cluster| cluster.cells == 2));
    assert_eq!(emoji[1].cell, 2);

    let variants = shaper.shape("✊\u{fe0e} ✊\u{fe0f}", 0);
    assert!(variants.iter().any(|cluster| cluster.text == "✊\u{fe0e}"));
    assert!(variants.iter().any(|cluster| cluster.text == "✊\u{fe0f}"));

    let box_glyph = shaper.shape("a─b", 0);
    assert_eq!(box_glyph[1].text, "─");
    assert_eq!(box_glyph[1].cell, 1);

    let symbols = shaper.shape("a|b", 0);
    assert_eq!(symbols[0].text, "a");
    assert_eq!(symbols[1].text, "|");
    assert_eq!(symbols[2].text, "b");
    assert_cells_are_monotonic(&symbols, "symbols");
}

#[test]
fn atlas_reuses_cached_glyph_entries_for_same_key() {
    let mut atlas = GlyphAtlas::new(64, 64).expect("bounded test atlas");
    let key = GlyphAtlasKey {
        dilation: 0,
        face: GlyphAtlasFaceKey::new(face()),
        text: GlyphAtlasTextKey::new("A"),
        font_size_bits: DEFAULT_FONT_SIZE.to_bits(),
        pixels_per_point_bits: 1.0_f32.to_bits(),
        width: 8,
        height: 12,
    };

    let first = atlas.insert_or_get(key.clone(), 8, 12, vec![255; 8 * 12]);
    let second = atlas.insert_or_get(key, 8, 12, vec![0; 8 * 12]);

    assert_eq!(first, second);
    assert_eq!(atlas.len(), 1);
}

fn assert_cells_are_monotonic(
    clusters: &[bootty_ui::terminal_text_atlas::ShapedCluster],
    name: &str,
) {
    for pair in clusters.windows(2) {
        let previous_end = pair[0].cell.saturating_add(pair[0].cells);
        assert!(
            pair[1].cell >= previous_end,
            "{name} cluster {:?} overlaps {:?}",
            pair[1],
            pair[0]
        );
    }
}

#[test]
fn glyph_atlas_ports_ghostty_reserve_fit_edges() {
    let mut exact = GlyphAtlas::new(34, 34).expect("bounded test atlas");
    assert!(exact.reserve(32, 32).is_some());
    assert_eq!(exact.modified_count(), 0);
    assert!(exact.reserve(1, 1).is_none());

    let mut too_small = GlyphAtlas::new(32, 32).expect("bounded test atlas");
    assert!(too_small.reserve(32, 32).is_none());

    let mut multiple = GlyphAtlas::new(32, 32).expect("bounded test atlas");
    assert!(multiple.reserve(15, 30).is_some());
    assert!(multiple.reserve(15, 30).is_some());
    assert!(multiple.reserve(1, 1).is_none());
}

#[test]
fn glyph_atlas_ports_ghostty_write_and_crop_semantics() {
    let mut atlas = GlyphAtlas::new(32, 32).expect("bounded test atlas");
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    let old = atlas.modified_count();

    atlas.set(entry, &[1, 2, 3, 4]);

    assert!(atlas.modified_count() > old);
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
}

#[test]
fn glyph_atlas_ports_ghostty_larger_source_crop() {
    let mut atlas = GlyphAtlas::new(32, 32).expect("bounded test atlas");
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");

    atlas.set_from_larger(
        entry,
        &[
            8, 8, 8, 8, 8, //
            8, 8, 1, 2, 8, //
            8, 8, 3, 4, 8, //
            8, 8, 8, 8, 8,
        ],
        5,
        2,
        1,
    );

    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
    assert!(!atlas.pixels().contains(&8));
}

#[test]
fn glyph_atlas_ports_ghostty_grow_preserves_data_and_opens_space() {
    let mut atlas = GlyphAtlas::new(4, 4).expect("bounded test atlas");
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    assert!(atlas.reserve(1, 1).is_none());
    atlas.set(entry, &[1, 2, 3, 4]);

    let old_modified = atlas.modified_count();
    let old_resized = atlas.resized_count();
    atlas.grow(5, 5).expect("bounded atlas growth");

    assert!(atlas.modified_count() > old_modified);
    assert!(atlas.resized_count() > old_resized);
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
    assert!(atlas.reserve(1, 1).is_some());
}

#[test]
fn glyph_atlas_ports_ghostty_bgr_write_and_grow_semantics() {
    let mut atlas =
        GlyphAtlas::with_format(4, 4, GlyphAtlasFormat::Bgr).expect("bounded test atlas");
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    assert!(atlas.reserve(1, 1).is_none());

    atlas.set(
        entry,
        &[
            10, 11, 12, //
            13, 14, 15, //
            20, 21, 22, //
            23, 24, 25,
        ],
    );

    let expected = [10, 11, 12, 13, 14, 15, 0, 20, 25].map(Some);
    assert_eq!(bgr_samples(&atlas), expected);

    atlas.grow(5, 5).expect("bounded atlas growth");

    assert_eq!(bgr_samples(&atlas), expected);
    assert!(atlas.reserve(1, 3).is_some());
    assert!(atlas.reserve(2, 1).is_some());
    assert!(atlas.reserve(1, 1).is_none());
}

#[rstest::rstest]
#[case::byte_limit(5, 5, 4)]
#[case::width_overflow(u32::MAX, 5, usize::MAX)]
#[case::height_overflow(5, u32::MAX, usize::MAX)]
fn glyph_atlas_ports_ghostty_error_paths_without_partial_mutation(
    #[case] width: u32,
    #[case] height: u32,
    #[case] byte_limit: usize,
) {
    assert!(matches!(
        GlyphAtlas::try_with_format(32, 32, GlyphAtlasFormat::Alpha, 4),
        Err(GlyphAtlasError::CapacityExceeded)
    ));

    let mut atlas = GlyphAtlas::new(4, 4).expect("bounded test atlas");
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    atlas.set(entry, &[1, 2, 3, 4]);
    let old_modified = atlas.modified_count();
    let old_resized = atlas.resized_count();
    assert!(atlas.reserve(width, height).is_none());
    assert!(atlas.reserve(1, 1).is_none());
    assert_eq!(atlas.modified_count(), old_modified);
    assert_eq!(atlas.resized_count(), old_resized);
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);

    assert_eq!(
        atlas.try_grow_with_byte_limit(width, height, byte_limit),
        Err(GlyphAtlasError::CapacityExceeded)
    );
    assert_eq!(atlas.modified_count(), old_modified);
    assert_eq!(atlas.resized_count(), old_resized);
    assert_eq!(atlas.size(), (4, 4));
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
}

#[test]
fn glyph_atlas_saturation_memo_still_admits_smaller_glyphs() {
    // Two wide rows leave only a thin right-edge gap. A large reserve fails and records the
    // saturation footprint; the bug guarded here is the memo over-blocking — a smaller glyph
    // that genuinely fits the leftover gap must still reserve rather than fall to the 1x1
    // fallback (which silently drops the glyph).
    let mut atlas = GlyphAtlas::new(20, 20).expect("bounded test atlas");
    assert!(atlas.reserve(16, 4).is_some());
    assert!(atlas.reserve(16, 12).is_some());

    assert!(atlas.reserve(16, 4).is_none());
    assert!(atlas.reserve(2, 1).is_some());
}

#[test]
fn text_atlas_builder_appends_textured_quads_without_replacing_existing_batch() {
    let command = text_command("A界", 40.0);
    let sentinel = TexturedGlyphQuad {
        rect: SurfaceRect::from_min_size(99.0, 99.0, 1.0, 1.0),
        uv: SurfaceRect::from_min_size(0.0, 0.0, 1.0, 1.0),
        atlas_entry: bootty_ui::terminal_text_atlas::GlyphAtlasEntry {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        color: attrs().fg,
        snap_to_pixel_grid: false,
    };
    let mut builder = TextAtlasBuilder::new(128, 128).expect("bounded test atlas");
    let mut quads = vec![sentinel];

    builder.visit_text_command(&command, 1.0, |_, quad| quads.push(quad));

    assert_eq!(quads.len(), 3);
    assert_eq!(quads[0], sentinel);
    assert!(quads[1].uv.min_x < quads[1].uv.max_x);
    assert_eq!(quads[1].color, attrs().fg);
    assert!(!quads[1].snap_to_pixel_grid);
    assert_eq!(builder.atlas_len(), 2);
}

#[test]
fn atlas_keys_separate_same_glyph_at_different_pixel_scales() {
    let command = text_command("A", 10.0);
    let mut builder = TextAtlasBuilder::new(128, 128).expect("bounded test atlas");

    command_quads(&mut builder, &command, 1.0);
    command_quads(&mut builder, &command, 2.0);

    assert_eq!(builder.atlas_len(), 2);
}

#[rstest]
#[case("◆")]
#[case("→")]
#[case("\u{e0a0}")]
fn gpui_preserves_atlas_ink_beyond_a_style_run(#[case] text: &str) {
    use bootty_ui::{
        gpui::GpuiTerminalElement,
        terminal_render::{TerminalRenderCommand, TerminalRenderFrame},
    };
    let mut command = text_command(text, 9.0);
    command.face = Arc::new(ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: "Maple Mono NF".into(),
        fallback_families: vec![],
        style: FontStyle::Regular,
    });
    command.rect = SurfaceRect::from_min_size(30.0, 10.0, 9.0, 32.0);
    let mut atlas = TextAtlasBuilder::new_rgba(256, 256).expect("bounded test atlas");
    let quads = command_quads(&mut atlas, &command, 1.0);
    assert_ne!(quads.as_slice(), &[]);
    let expected: Vec<_> = quads
        .iter()
        .map(|quad| {
            SurfaceRect::from_min_size(
                command.rect.min_x + (quad.rect.min_x - command.rect.min_x).round(),
                command.rect.min_y + (quad.rect.min_y - command.rect.min_y).round(),
                quad.atlas_entry
                    .width
                    .to_f32()
                    .expect("atlas width fits in f32"),
                quad.atlas_entry
                    .height
                    .to_f32()
                    .expect("atlas height fits in f32"),
            )
        })
        .collect();
    assert!(
        expected
            .iter()
            .any(|ink| ink.min_x < command.rect.min_x || ink.max_x > command.rect.max_x)
    );
    let element = GpuiTerminalElement::new(TerminalRenderFrame {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 100.0, 60.0),
        commands: vec![TerminalRenderCommand::Text(command)],
    });
    assert_eq!(element.glyph_sprite_rects(), expected);
}

#[rstest]
fn maple_outline_uses_the_same_em_size_as_shaping(
    #[values('H', 'g', 'P', 'é')] ch: char,
    #[values(1.0, 1.5, 2.0)] device_scale: f32,
) {
    use ab_glyph::{Font, FontArc, point};
    let font = FontArc::try_from_slice(include_bytes!("../assets/fonts/MapleMono-NF-Regular.ttf"))
        .unwrap();
    let mut command = text_command(&ch.to_string(), 20.0);
    command.rect = SurfaceRect::from_min_size(0.0, 0.0, 20.0, 40.0);
    command.face = Arc::new(ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: "Maple Mono NF".into(),
        fallback_families: vec![],
        style: FontStyle::Regular,
    });
    command.font_features = Arc::from([]);
    let mut atlas = TextAtlasBuilder::new_rgba(256, 256).expect("bounded test atlas");
    let quads = command_quads(&mut atlas, &command, device_scale);
    let quad = &quads[0];
    let tile = atlas.bgra_tile(quad).unwrap();
    let ink_rows: Vec<_> = tile
        .chunks_exact(
            usize::try_from(
                quad.atlas_entry
                    .width
                    .checked_mul(4)
                    .expect("atlas row width fits in u32"),
            )
            .expect("atlas row width fits in usize"),
        )
        .enumerate()
        .filter(|(_, row)| row.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0))
        .map(|(row, _)| row)
        .collect();

    // Independent font-unit oracle: font.size is pixels per em, not line height.
    let unit_scale = command.font_size * device_scale / font.units_per_em().unwrap();
    let height = font.height_unscaled() * unit_scale;
    let baseline = font
        .ascent_unscaled()
        .mul_add(unit_scale, 40.0f32.mul_add(device_scale, -height) / 2.0);
    let glyph = font
        .glyph_id(ch)
        .with_scale_and_position(height, point(0.0, baseline));
    let bounds = font.outline_glyph(glyph).unwrap().px_bounds();
    assert_eq!(
        ink_rows.first().copied(),
        Some(bounds.min.y.to_usize().expect("ink row fits in usize"))
    );
    assert_eq!(
        ink_rows.last().copied(),
        Some(
            bounds
                .max
                .y
                .to_usize()
                .expect("ink row fits in usize")
                .checked_sub(1)
                .expect("ink bounds include at least one row"),
        )
    );
}

#[test]
fn whitespace_clusters_do_not_create_invisible_quads_or_atlas_entries() {
    let command = text_command("A B", 30.0);
    let mut builder = TextAtlasBuilder::new(64, 64).expect("bounded test atlas");

    let quads = command_quads(&mut builder, &command, 1.0);

    assert_eq!(quads.len(), 2);
    assert_eq!(
        quads[0].rect,
        SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0)
    );
    assert_eq!(
        quads[1].rect,
        SurfaceRect::from_min_size(20.0, 0.0, 10.0, 20.0)
    );
    assert_eq!(builder.atlas_len(), 2);
}

#[rstest]
#[case::wide(1_000_000.0, 8.0)]
#[case::tall(8.0, 1_000_000.0)]
fn oversized_sprites_keep_their_geometry_with_a_bounded_complete_tile(
    #[case] width: f32,
    #[case] height: f32,
) {
    let command = SpriteCommandBatch {
        glyph: SpriteGlyph::from_char('█').expect("full block"),
        rect: SurfaceRect::from_min_size(0.0, 0.0, width, height),
        color: PlanColor {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
    };
    let mut builder = TextAtlasBuilder::new_rgba(32, 32).expect("bounded test atlas");
    let quad = builder.prepare_sprite_command(&command, 1.0);
    assert_eq!(quad.rect, command.rect);
    assert!(quad.atlas_entry.width > 1 && quad.atlas_entry.height > 1);
    assert!(quad.atlas_entry.width <= 4094 && quad.atlas_entry.height <= 4094);
    assert_eq!(quad.atlas_entry.width.min(quad.atlas_entry.height), 8);
    let tile = builder.bgra_tile(&quad).expect("complete sprite tile");
    assert_ne!(tile, Vec::<u8>::new());
    assert!(tile.iter().all(|channel| *channel == 255));
}

#[rstest]
#[case::inverse_cross('\u{1FBBD}', (8, 12), (0, 12))]
#[case::inverse_lower_right('\u{1FBBE}', (15, 12), (0, 0))]
#[case::inverse_four_corners('\u{1FBBF}', (8, 0), (8, 12))]
fn subtractive_sprites_rasterize_transparent_strokes_into_the_cached_gpu_tile(
    #[case] ch: char,
    #[case] clear_pixel: (u32, u32),
    #[case] filled_pixel: (u32, u32),
) {
    let color = PlanColor {
        r: 0x12,
        g: 0x34,
        b: 0x56,
        a: 255,
    };
    let command = SpriteCommandBatch {
        glyph: SpriteGlyph::from_char(ch).expect("subtractive sprite"),
        rect: SurfaceRect::from_min_size(0.0, 0.0, 16.0, 24.0),
        color,
    };
    let mut builder = TextAtlasBuilder::new_rgba(128, 128).expect("bounded test atlas");

    let first = builder.prepare_sprite_command(&command, 1.0);
    let first_tile = builder.bgra_tile(&first).expect("subtractive sprite tile");
    let second = builder.prepare_sprite_command(&command, 1.0);

    assert_eq!(builder.atlas_len(), 1, "the sprite raster must be cached");
    assert_eq!(first.atlas_entry, second.atlas_entry);
    assert_eq!(pixel(&first_tile, 16, clear_pixel), [0x56, 0x34, 0x12, 0]);
    assert_eq!(
        pixel(&first_tile, 16, filled_pixel),
        [0x56, 0x34, 0x12, 255]
    );
    assert!(
        first_tile
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] == 0)
    );
    assert!(
        first_tile
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] == 255)
    );
}

fn face() -> ResolvedFontFace {
    ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: "Test Mono".to_owned(),
        fallback_families: vec!["Fallback".to_owned()],
        style: FontStyle::Regular,
    }
}

const fn attrs() -> bootty_ui::paint_plan::TextAttrs {
    bootty_ui::paint_plan::TextAttrs {
        fg: PlanColor {
            r: 220,
            g: 221,
            b: 222,
            a: 255,
        },
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn assert_atlas_pixels(atlas: &GlyphAtlas, expected: &[(u32, u32, u8)]) {
    for (x, y, value) in expected {
        assert_eq!(atlas.atlas_pixel(*x, *y), Some(*value), "pixel {x},{y}");
    }
}

fn bgr_samples(atlas: &GlyphAtlas) -> [Option<u8>; 9] {
    [
        (1, 1, 0),
        (1, 1, 1),
        (1, 1, 2),
        (2, 1, 0),
        (2, 1, 1),
        (2, 1, 2),
        (3, 1, 0),
        (1, 2, 0),
        (2, 2, 2),
    ]
    .map(|(x, y, channel)| atlas.atlas_pixel_channel(x, y, channel))
}

fn pixel(bgra: &[u8], width: u32, (x, y): (u32, u32)) -> [u8; 4] {
    let start = usize::try_from(
        y.checked_mul(width)
            .and_then(|offset| offset.checked_add(x))
            .and_then(|offset| offset.checked_mul(4))
            .expect("pixel offset fits in u32"),
    )
    .expect("pixel offset fits in usize");
    let end = start.checked_add(4).expect("BGRA pixel end fits in usize");
    bgra.get(start..end)
        .expect("BGRA pixel")
        .try_into()
        .expect("BGRA pixel")
}

#[rstest]
#[case("Lilex", FontStyle::Bold, bootty_ui::assets::LILEX_BOLD)]
#[case("Lilex", FontStyle::Italic, bootty_ui::assets::LILEX_ITALIC)]
#[case("Lilex", FontStyle::BoldItalic, bootty_ui::assets::LILEX_BOLD_ITALIC)]
#[case("Lilex Bold", FontStyle::Regular, bootty_ui::assets::LILEX_BOLD)]
#[case("Lilex-Bold", FontStyle::Regular, bootty_ui::assets::LILEX_BOLD)]
#[case(
    "Maple Mono NF Regular",
    FontStyle::Regular,
    bootty_ui::assets::MAPLE_MONO_NF_REGULAR
)]
fn selected_face_preserves_natural_ink_and_bearings(
    #[case] family: &str,
    #[case] style: FontStyle,
    #[case] bytes: &'static [u8],
    #[values('H', 'f', 'j', 'é')] ch: char,
    #[values(1.0, 1.5, 2.0)] scale: f32,
) {
    use ab_glyph::{Font, FontArc, point};
    let font = FontArc::try_from_slice(bytes).unwrap();
    let mut command = text_command(&ch.to_string(), 7.0);
    command.rect = SurfaceRect::from_min_size(0.0, 0.0, 7.0, 22.0);
    command.font_size = 11.75;
    command.face = Arc::new(ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: family.into(),
        fallback_families: vec![],
        style,
    });
    command.font_features = Arc::from([FontFeature::new(*b"liga", 0)]);
    let actual = command_ink(&command, scale);
    let units = command.font_size * scale / font.units_per_em().unwrap();
    let height = font.height_unscaled() * units;
    let baseline = font
        .ascent_unscaled()
        .mul_add(units, command.rect.height().mul_add(scale, -height) / 2.0);
    let glyph = font
        .glyph_id(ch)
        .with_scale_and_position(height, point(0.0, baseline));
    let outline = font.outline_glyph(glyph).unwrap();
    let bounds = outline.px_bounds();
    let mut expected = std::collections::BTreeMap::new();
    outline.draw(|x, y, coverage| {
        let alpha = (coverage * 255.0)
            .round()
            .to_u8()
            .expect("coverage maps to an alpha byte");
        if alpha > 0 {
            expected.insert(
                (
                    bounds
                        .min
                        .x
                        .to_i32()
                        .expect("glyph x bound fits in i32")
                        .checked_add(i32::try_from(x).expect("glyph x fits in i32"))
                        .expect("glyph x coordinate fits in i32"),
                    bounds
                        .min
                        .y
                        .to_i32()
                        .expect("glyph y bound fits in i32")
                        .checked_add(i32::try_from(y).expect("glyph y fits in i32"))
                        .expect("glyph y coordinate fits in i32"),
                ),
                alpha,
            );
        }
    });
    assert_eq!(actual, expected, "{family} {style:?} {ch} at {scale}");
}

// Geometry assertions can retain quads; tests that inspect pixels copy them in the visitor.
fn command_quads(
    builder: &mut TextAtlasBuilder,
    command: &TextCommand,
    scale: f32,
) -> Vec<TexturedGlyphQuad> {
    let mut quads = Vec::new();
    builder.visit_text_command(command, scale, |_, quad| quads.push(quad));
    quads
}

fn command_ink(command: &TextCommand, scale: f32) -> std::collections::BTreeMap<(i32, i32), u8> {
    let mut builder = TextAtlasBuilder::new_rgba(256, 256).expect("bounded test atlas");
    command_ink_with_builder(&mut builder, command, scale)
}

#[rstest]
fn missing_nerd_icons_use_the_bundled_face_before_system_fallback(
    #[values(FontStyle::Regular, FontStyle::Bold)] style: FontStyle,
    #[values(1.0, 2.0)] scale: f32,
) {
    let mut command = text_command("\u{e7a8}", 20.0);
    command.font_size = 28.0;
    command.rect = SurfaceRect::from_min_size(0.0, 0.0, 20.0, 40.0);
    command.face = Arc::new(ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: "Lilex".into(),
        fallback_families: vec!["Maple Mono NF".into()],
        style,
    });
    let explicit = command_ink(&command, scale);
    assert!(!explicit.is_empty());
    Arc::make_mut(&mut command.face).fallback_families.clear();
    assert_eq!(command_ink(&command, scale), explicit);
}

#[rstest]
fn streamed_glyph_pixels_survive_atlas_recycling_inside_one_text_run() {
    let command = text_command("AB", 40.0);
    let mut fresh = TextAtlasBuilder::new_rgba(40, 4096).expect("bounded test atlas");
    let expected = command_ink_with_builder(&mut fresh, &command, 1.0);
    assert!(!expected.is_empty());

    let mut saturated = TextAtlasBuilder::new_rgba(40, 4096).expect("bounded test atlas");
    saturated.prepare_sprite_command(
        &SpriteCommandBatch {
            glyph: SpriteGlyph::from_char('█').unwrap(),
            rect: SurfaceRect::from_min_size(0.0, 0.0, 38.0, 4070.0),
            color: attrs().fg,
        },
        1.0,
    );
    let mut generations = Vec::new();
    let mut actual = std::collections::BTreeMap::new();
    saturated.visit_text_command(&command, 1.0, |builder, quad| {
        generations.push(builder.atlas_resized_count());
        let tile = builder.bgra_tile(&quad).unwrap();
        for (index, pixel) in tile.as_chunks::<4>().0.iter().enumerate() {
            if pixel[3] > 0 {
                let left = quad
                    .rect
                    .min_x
                    .round()
                    .to_i32()
                    .expect("quad x coordinate fits in i32");
                let top = quad
                    .rect
                    .min_y
                    .round()
                    .to_i32()
                    .expect("quad y coordinate fits in i32");
                let x = atlas_pixel_offset(index, quad.atlas_entry.width);
                let y =
                    atlas_pixel_row_offset(index, quad.atlas_entry.width, quad.atlas_entry.height);
                actual.insert(
                    (
                        left.checked_add(x).expect("atlas x coordinate fits in i32"),
                        top.checked_add(y).expect("atlas y coordinate fits in i32"),
                    ),
                    pixel[3],
                );
            }
        }
    });
    assert!(
        generations.windows(2).any(|pair| pair[0] != pair[1]),
        "the run must exercise a mid-command recycle"
    );
    assert_eq!(actual, expected);
    assert_eq!(
        command_ink_with_builder(&mut saturated, &command, 1.0),
        expected
    );
}

fn command_ink_with_builder(
    builder: &mut TextAtlasBuilder,
    command: &TextCommand,
    scale: f32,
) -> std::collections::BTreeMap<(i32, i32), u8> {
    let mut ink = std::collections::BTreeMap::new();
    builder.visit_text_command(command, scale, |builder, quad| {
        let tile = builder.bgra_tile(&quad).unwrap();
        let left = (quad.rect.min_x * scale)
            .round()
            .to_i32()
            .expect("quad x coordinate fits in i32");
        let top = (quad.rect.min_y * scale)
            .round()
            .to_i32()
            .expect("quad y coordinate fits in i32");
        for (i, pixel) in tile.as_chunks::<4>().0.iter().enumerate() {
            if pixel[3] > 0 {
                let x = atlas_pixel_offset(i, quad.atlas_entry.width);
                let y = atlas_pixel_row_offset(i, quad.atlas_entry.width, quad.atlas_entry.height);
                ink.insert(
                    (
                        left.checked_add(x).expect("atlas x coordinate fits in i32"),
                        top.checked_add(y).expect("atlas y coordinate fits in i32"),
                    ),
                    pixel[3],
                );
            }
        }
    });
    ink
}

fn atlas_pixel_offset(index: usize, extent: u32) -> i32 {
    let index = u32::try_from(index).expect("atlas pixel index fits in u32");
    let offset = index.checked_rem(extent).expect("atlas extent is nonzero");
    i32::try_from(offset).expect("atlas pixel offset fits in i32")
}

fn atlas_pixel_row_offset(index: usize, width: u32, height: u32) -> i32 {
    let width = usize::try_from(width).expect("atlas width fits in usize");
    let row = index.checked_div(width).expect("atlas width is nonzero");
    atlas_pixel_offset(row, height)
}

#[rstest]
#[case("Maple Mono NF")]
#[case("Lilex")]
fn mixed_width_runs_keep_the_same_ink_as_individual_cells(#[case] family: &str) {
    let pieces: [(&str, u16); 6] = [
        ("A", 1),
        ("é", 1),
        ("e\u{301}", 1),
        ("界", 2),
        ("0", 1),
        (" ", 1),
    ];
    let text = pieces
        .iter()
        .map(|(text, _)| *text)
        .collect::<String>()
        .repeat(8);
    let mut command = text_command(&text, 560.0);
    command.face = Arc::new(ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: family.into(),
        fallback_families: vec![],
        style: FontStyle::Regular,
    });
    command.font_features = Arc::from([FontFeature::new(*b"liga", 0)]);
    let actual = command_ink(&command, 2.0);
    let mut expected = std::collections::BTreeMap::new();
    let mut start = 0.0;
    for (text, cells) in pieces.into_iter().cycle().take(48) {
        command.text = text.into();
        command.rect = SurfaceRect::from_min_size(start, 0.0, f32::from(cells) * 10.0, 20.0);
        expected.extend(command_ink(&command, 2.0));
        start = command.rect.max_x;
    }
    assert_eq!(
        actual, expected,
        "{family} changed cell byte ranges in a longer run"
    );
}

#[rstest]
#[case("Maple Mono NF")]
#[case("Lilex")]
fn changing_ascii_font_features_matches_a_fresh_atlas(#[case] family: &str) {
    let mut command = text_command("0", 12.0);
    command.face = Arc::new(ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: family.into(),
        fallback_families: vec![],
        style: FontStyle::Regular,
    });
    let mut builder = TextAtlasBuilder::new_rgba(256, 256).expect("bounded test atlas");
    let mut previous_ink = None;
    for value in [0, 1, 0] {
        command.font_features = Arc::from([FontFeature::new(*b"zero", value)]);
        let expected = command_ink(&command, 2.0);
        assert!(!expected.is_empty());
        if let Some(previous) = previous_ink {
            pretty_assertions::assert_ne!(expected, previous, "{family} zero={value}");
        }
        assert_eq!(
            command_ink_with_builder(&mut builder, &command, 2.0),
            expected,
            "{family} zero={value} reused stale glyph ink"
        );
        previous_ink = Some(expected);
    }
}

#[rstest]
#[case("Maple Mono NF")]
#[case("Lilex")]
#[case("Menlo")]
fn combining_marks_render_like_the_equivalent_precomposed_character(
    #[case] family: &str,
    #[values(1.0, 2.0)] scale: f32,
) {
    let mut command = text_command("é", 9.0);
    command.face = Arc::new(ResolvedFontFace {
        assignment: bootty_config::FontStyleAssignment::Automatic,
        family: family.into(),
        fallback_families: vec![],
        style: FontStyle::Regular,
    });
    let composed = command_ink(&command, scale);
    command.text = "e\u{301}".into();
    assert_eq!(command_ink(&command, scale), composed);
}

#[cfg(target_os = "macos")]
#[rstest]
fn native_symbol_ink_survives_a_short_cell(
    #[values('◆', '◇', '★', '→')] ch: char,
    #[values(1.0, 2.0)] scale: f32,
) {
    // Changing row spacing may move ink, but must not erase its upper/lower edges.
    let render = |height| {
        let mut command = text_command(&ch.to_string(), 40.0);
        command.rect = SurfaceRect::from_min_size(0.0, 0.0, 40.0, height);
        command.font_size = 26.0;
        command.font_features = Arc::from([]);
        command.face = Arc::new(ResolvedFontFace {
            assignment: bootty_config::FontStyleAssignment::Automatic,
            family: "Maple Mono NF".into(),
            fallback_families: vec![],
            style: FontStyle::Regular,
        });
        let ink = command_ink(&command, scale);
        assert!(!ink.is_empty());
        let left = ink.keys().map(|(x, _)| *x).min().unwrap();
        let top = ink.keys().map(|(_, y)| *y).min().unwrap();
        ink.into_iter()
            .map(|((x, y), alpha)| {
                (
                    (
                        x.checked_sub(left).expect("ink x coordinate ordering"),
                        y.checked_sub(top).expect("ink y coordinate ordering"),
                    ),
                    alpha,
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(render(12.0), render(48.0), "{ch} at {scale}x");
}
