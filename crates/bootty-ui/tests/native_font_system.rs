#![cfg(test)]
#![cfg(target_os = "macos")]

use bootty_config::{FontStyleAssignment, FontWeightAssignments, FontWeightRole};
use bootty_ui::{
    assets::BoottyAssets,
    font_database::system_font_database,
    font_mapping::{FontMappings, wrap_text_system},
    gpui::{UiPalette, init_theme, setup_ui_font, update_ui_font, update_ui_font_weights},
};
use gpui_kit::{
    AppContext as _, Font, FontFeatures, FontId, FontRun, FontWeight, GlyphId, NoopTextSystem,
    PlatformTextSystem, RenderGlyphParams, TestAppContext, TextSystem, point, px,
};
use pretty_assertions::{assert_eq, assert_ne};
use rstest::{fixture, rstest};
use std::sync::Arc;

#[fixture]
fn native_system() -> Arc<dyn PlatformTextSystem> {
    let system = wrap_text_system(Arc::new(NoopTextSystem), FontMappings::default());
    BoottyAssets
        .load_fonts(&TextSystem::new(system.clone()))
        .unwrap();
    system
}

fn loaded_face(name: &str) -> font_kit::font::Font {
    let database = system_font_database();
    let face = database
        .faces()
        .find(|face| face.post_script_name == name)
        .unwrap_or_else(|| panic!("Missing native test font {name}"));
    database
        .with_face_data(face.id, |data, index| {
            font_kit::font::Font::from_bytes(Arc::new(data.to_vec()), index).unwrap()
        })
        .unwrap()
}

fn assert_face(system: &dyn PlatformTextSystem, descriptor: &Font, expected: &str) -> FontId {
    let id = system.font_id(descriptor).unwrap();
    let native = loaded_face(expected);
    for ch in ['a', 'W', 'g', '0', '@'] {
        let glyph = native.glyph_for_char(ch).unwrap();
        assert_eq!(
            system.glyph_for_char(id, ch),
            Some(GlyphId(glyph)),
            "{expected}: {ch}"
        );
        assert_eq!(
            system.advance(id, GlyphId(glyph)).unwrap().width.to_bits(),
            native.advance(glyph).unwrap().x().to_bits(),
            "{expected}: {ch} advance"
        );
        let rect = native.typographic_bounds(glyph).unwrap();
        let bounds = system.typographic_bounds(id, GlyphId(glyph)).unwrap();
        assert_eq!(
            (
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height
            ),
            (
                rect.origin_x(),
                rect.origin_y(),
                rect.width(),
                rect.height()
            ),
            "{expected}: {ch} outline bounds"
        );
    }
    id
}

fn pixels(system: &dyn PlatformTextSystem, id: FontId, glyph: GlyphId, dilation: u8) -> Vec<u8> {
    let params = RenderGlyphParams {
        font_id: id,
        glyph_id: glyph,
        font_size: px(20.0),
        scale_factor: 2.0,
        subpixel_variant: point(0, 0),
        is_emoji: false,
        subpixel_rendering: false,
        dilation,
    };
    let bounds = system.glyph_raster_bounds(&params).unwrap();
    let (size, pixels) = system.rasterize_glyph(&params, bounds).unwrap();
    let pixel_count = size
        .width
        .0
        .checked_mul(size.height.0)
        .expect("native raster dimensions fit in u32");
    assert_eq!(
        pixels.len(),
        usize::try_from(pixel_count).expect("native raster dimensions fit in usize")
    );
    assert!(pixels.iter().any(|&alpha| alpha > 0));
    pixels
}

#[gpui_kit::test]
fn accepted_ui_styles_reach_exact_native_faces_and_keep_old_snapshots(cx: &TestAppContext) {
    let mappings = FontMappings::default();
    let system = wrap_text_system(Arc::new(NoopTextSystem), mappings.clone());
    BoottyAssets
        .load_fonts(&TextSystem::new(system.clone()))
        .unwrap();
    cx.update(|cx| {
        cx.set_global(mappings);
        init_theme(UiPalette::default(), cx);
        cx.open_window(gpui_kit::WindowOptions::default(), |window, cx| {
            update_ui_font(&["Lilex-Regular".into()], 16.0, cx);
            let regular = setup_ui_font(window, cx);
            let mut bold = regular.clone();
            bold.weight = FontWeight::BOLD;
            let regular_id = assert_face(system.as_ref(), &regular, "Lilex-Regular");
            let original_bold_id = assert_face(system.as_ref(), &bold, "Lilex-Bold");
            let regular_pixels = pixels(
                system.as_ref(),
                regular_id,
                system.glyph_for_char(regular_id, 'a').unwrap(),
                0,
            );
            let bold_pixels = pixels(
                system.as_ref(),
                original_bold_id,
                system.glyph_for_char(original_bold_id, 'a').unwrap(),
                0,
            );
            assert_ne!(regular_pixels, bold_pixels);

            for (assignment, expected) in [
                (FontStyleAssignment::Disabled, "Lilex-Regular"),
                (FontStyleAssignment::Named("Italic".into()), "Lilex-Italic"),
                (
                    FontStyleAssignment::Named("Regular".into()),
                    "Lilex-Regular",
                ),
            ] {
                let assignments = FontWeightAssignments::from([(FontWeightRole::Bold, assignment)]);
                update_ui_font_weights(&assignments, cx);
                let mut selected = setup_ui_font(window, cx);
                selected.weight = FontWeight::BOLD;
                assert_face(system.as_ref(), &selected, expected);
                assert_eq!(
                    assert_face(system.as_ref(), &bold, "Lilex-Bold"),
                    original_bold_id
                );
            }
            update_ui_font_weights(&FontWeightAssignments::new(), cx);
            assert_additional_installed_faces(system.as_ref(), window, cx);
            cx.new(|_| gpui_kit::Empty)
        })
        .unwrap();
    });
}

fn assert_additional_installed_faces(
    system: &dyn PlatformTextSystem,
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) {
    // Exercise the reported Maple family when its additional installed faces exist.
    // CI still covers exact named selection using the bundled Lilex styles below.
    if system_font_database()
        .faces()
        .any(|face| face.post_script_name == "MapleMono-NF-Thin")
    {
        let mut previous = None;
        for (base, expected) in [
            (
                "MapleMono-NF-Regular",
                ["Regular", "Medium", "SemiBold", "Bold"],
            ),
            // fontdb's 450 cutoff prefers Regular400 for Thin250 + Semibold200.
            (
                "MapleMono-NF-Thin",
                ["Thin", "Light", "Regular", "SemiBold"],
            ),
        ] {
            update_ui_font(&[base.into()], 16.0, cx);
            let root = setup_ui_font(window, cx);
            for (weight, expected) in [
                FontWeight::NORMAL,
                FontWeight::MEDIUM,
                FontWeight::SEMIBOLD,
                FontWeight::BOLD,
            ]
            .into_iter()
            .zip(expected)
            {
                let mut descriptor = root.clone();
                descriptor.weight = weight;
                let name = format!("MapleMono-NF-{expected}");
                let id = assert_face(system, &descriptor, &name);
                let mask = pixels(system, id, system.glyph_for_char(id, 'a').unwrap(), 0);
                if weight == FontWeight::NORMAL
                    && let Some(mask_before) = previous.replace(mask.clone())
                {
                    assert_ne!(
                        mask_before, mask,
                        "Regular to Thin must change actual coverage"
                    );
                }
            }
        }
    }

    // These installed siblings share weight/style properties; matching those alone
    // cannot implement the named-face selection. Avenir is supplied by macOS.
    let mut sibling_pixels = Vec::new();
    for name in ["Avenir-Book", "Avenir-Roman"] {
        update_ui_font(&[name.into()], 16.0, cx);
        let descriptor = setup_ui_font(window, cx);
        let id = assert_face(system, &descriptor, name);
        sibling_pixels.push(pixels(
            system,
            id,
            system.glyph_for_char(id, 'a').unwrap(),
            0,
        ));
    }
    assert_ne!(sibling_pixels[0], sibling_pixels[1]);
}

#[rstest]
fn native_opentype_features_and_style_boundaries_are_preserved(
    native_system: Arc<dyn PlatformTextSystem>,
) {
    let mut font = gpui_kit::font("Times");
    let shape = |font: &Font, lengths: &[usize]| {
        let id = native_system.font_id(font).unwrap();
        let runs = lengths
            .iter()
            .map(|&len| FontRun { len, font_id: id })
            .collect::<Vec<_>>();
        native_system
            .layout_line("fi", px(20.0), &runs)
            .runs
            .into_iter()
            .flat_map(|run| run.glyphs)
            .map(|glyph| glyph.id)
            .collect::<Vec<_>>()
    };
    font.features = FontFeatures(Arc::new(vec![("liga".into(), 1)]));
    let ligature = shape(&font, &[2]);
    let separate_styles = shape(&font, &[1, 1]);
    font.features = FontFeatures(Arc::new(vec![("liga".into(), 0)]));
    let disabled = shape(&font, &[2]);
    assert_eq!(ligature.len(), 1);
    assert_eq!(disabled.len(), 2);
    assert_eq!(separate_styles, disabled);
}

#[rstest]
fn mixed_scripts_keep_native_fallback_handles_and_utf8_indices(
    native_system: Arc<dyn PlatformTextSystem>,
) {
    let pieces = ["abc ", "אבג ", "🧑🏽‍🚀"];
    let text = pieces.concat();
    let regular = native_system.font_id(&gpui_kit::font("Lilex")).unwrap();
    let mut bold_font = gpui_kit::font("IBM Plex Sans");
    bold_font.weight = FontWeight::BOLD;
    let bold = native_system.font_id(&bold_font).unwrap();
    let runs = pieces
        .iter()
        .zip([regular, bold, regular])
        .map(|(text, font_id)| FontRun {
            len: text.len(),
            font_id,
        })
        .collect::<Vec<_>>();
    let line = native_system.layout_line(&text, px(20.0), &runs);
    assert_eq!(line.len, text.len());
    assert!(line.width > px(0.0));
    let mut has_color = false;
    for run in &line.runs {
        for glyph in &run.glyphs {
            assert!(text.is_char_boundary(glyph.index));
            assert!(f32::from(glyph.position.x).is_finite());
            let params = RenderGlyphParams {
                font_id: run.font_id,
                glyph_id: glyph.id,
                font_size: line.font_size,
                scale_factor: 2.0,
                subpixel_variant: point(1, 0),
                is_emoji: glyph.is_emoji,
                subpixel_rendering: false,
                dilation: 0,
            };
            let bounds = native_system.glyph_raster_bounds(&params).unwrap();
            let (size, bitmap) = native_system.rasterize_glyph(&params, bounds).unwrap();
            let channels = if glyph.is_emoji { 4 } else { 1 };
            let pixel_count = size
                .width
                .0
                .checked_mul(size.height.0)
                .expect("native raster dimensions fit in u32");
            assert_eq!(
                bitmap.len(),
                usize::try_from(pixel_count)
                    .expect("native raster dimensions fit in usize")
                    .checked_mul(channels)
                    .expect("native raster channel count fits in usize")
            );
            if glyph.is_emoji {
                has_color = true;
                assert_eq!(
                    text.get(glyph.index..)
                        .expect("glyph index is a UTF-8 boundary"),
                    pieces[2]
                );
                assert!(bitmap.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
            }
        }
    }
    assert!(
        has_color,
        "the complete emoji sequence must reach the native color fallback"
    );
}

#[rstest]
fn native_smoothing_changes_coverage_without_changing_face(
    native_system: Arc<dyn PlatformTextSystem>,
) {
    let id = native_system.font_id(&gpui_kit::font("Lilex")).unwrap();
    let glyph = native_system.glyph_for_char(id, 'a').unwrap();
    assert_ne!(
        pixels(native_system.as_ref(), id, glyph, 0),
        pixels(native_system.as_ref(), id, glyph, 4)
    );
}

#[rstest]
#[case("Lilex-Regular", "MapleMono-NF-Regular")]
#[case("Lilex-Regular", "Lilex-Bold")]
fn warm_terminal_atlas_changes_pixels_with_selected_font(
    native_system: Arc<dyn PlatformTextSystem>,
    #[case] first: &str,
    #[case] second: &str,
) {
    use bootty_terminal::geometry::SurfaceRect;
    use bootty_ui::{
        paint_plan::{PlanColor, TextAttrs},
        terminal_render::TextCommand,
        terminal_text::{FontResolver, TerminalTextConfig},
        terminal_text_atlas::TextAtlasBuilder,
    };

    let mut atlas = TextAtlasBuilder::with_platform_text_system(256, 256, native_system)
        .expect("bounded test atlas");
    let attrs = TextAttrs {
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
    };
    let mut results = Vec::new();
    for family in [first, second, first] {
        let resolver = FontResolver::new(TerminalTextConfig {
            families: vec![family.into()],
            ..TerminalTextConfig::default()
        });
        // Keep text, geometry, and cache alive while only the accepted font changes.
        let command = TextCommand {
            rect: SurfaceRect::from_min_size(0.0, 0.0, 12.0, 24.0),
            text: "a".into(),
            attrs,
            face: Arc::new(resolver.resolve_face(&attrs)),
            font_size: 20.0,
            cell_width: 12.0,
            font_features: Arc::from([]),
        };
        let mut tiles = Vec::new();
        atlas.visit_text_command(&command, 2.0, |atlas, quad| {
            let pixels = atlas.bgra_tile(&quad).unwrap();
            assert!(pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
            tiles.push(pixels);
        });
        assert_eq!(tiles.len(), 1);
        results.push(tiles);
    }
    assert_ne!(
        results[0], results[1],
        "{first} to {second} must change ink"
    );
    assert_eq!(
        results[0], results[2],
        "restoring the family must restore its ink"
    );
}

#[rstest]
#[case(1)]
#[case(2)]
#[case(3)]
#[case(usize::MAX)]
fn native_shaping_rejects_invalid_utf8_run_boundaries(
    native_system: Arc<dyn PlatformTextSystem>,
    #[case] len: usize,
) {
    let font_id = native_system.font_id(&gpui_kit::font("Times")).unwrap();
    let layout = native_system.layout_line("🦀", px(16.0), &[FontRun { len, font_id }]);
    assert!(layout.runs.is_empty());
    assert_eq!(layout.width, px(0.0));
    assert_eq!(layout.len, "🦀".len());
}

#[rstest]
fn unknown_native_font_has_no_glyphs_or_raster(native_system: Arc<dyn PlatformTextSystem>) {
    let font_id = FontId(usize::MAX);
    assert_eq!(native_system.glyph_for_char(font_id, 'a'), None);
    assert!(native_system.advance(font_id, GlyphId(1)).is_err());
    assert!(
        native_system
            .typographic_bounds(font_id, GlyphId(1))
            .is_err()
    );
    let metrics = native_system.font_metrics(font_id);
    assert!(f32::from(metrics.ascent(px(16.0))).is_finite());
    let layout = native_system.layout_line("a", px(16.0), &[FontRun { len: 1, font_id }]);
    assert!(layout.runs.is_empty());
    let params = RenderGlyphParams {
        font_id,
        glyph_id: GlyphId(1),
        font_size: px(16.0),
        scale_factor: 1.0,
        subpixel_variant: point(0, 0),
        is_emoji: false,
        subpixel_rendering: false,
        dilation: 0,
    };
    assert!(native_system.glyph_raster_bounds(&params).is_err());
}
