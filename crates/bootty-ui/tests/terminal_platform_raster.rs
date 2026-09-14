#![cfg(test)]

use bootty_terminal::geometry::SurfaceRect;
use bootty_ui::{
    paint_plan::{PlanColor, TextAttrs},
    terminal_render::TextCommand,
    terminal_text::{FontFeature, FontStyle, ResolvedFontFace},
    terminal_text_atlas::TextAtlasBuilder,
};
use gpui_kit::{
    Bounds, DevicePixels, Font, FontId, FontMetrics, FontRun, GlyphId, Hsla, LineLayout,
    NoopTextSystem, Pixels, PlatformTextSystem, RenderGlyphParams, Size, TextRenderingMode, point,
    size,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct NativeCoverage {
    requests: Mutex<Vec<RenderGlyphParams>>,
    fonts: Mutex<Vec<Font>>,
    registered_fonts: Mutex<Vec<Vec<u8>>>,
}

impl PlatformTextSystem for NativeCoverage {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        self.registered_fonts
            .lock()
            .unwrap()
            .extend(fonts.into_iter().map(Cow::into_owned));
        Ok(())
    }
    fn all_font_names(&self) -> Vec<String> {
        vec!["Lilex".into()]
    }
    fn font_id(&self, font: &Font) -> anyhow::Result<FontId> {
        self.fonts.lock().unwrap().push(font.clone());
        Ok(FontId(1))
    }
    fn font_metrics(&self, id: FontId) -> FontMetrics {
        NoopTextSystem.font_metrics(id)
    }
    fn typographic_bounds(&self, id: FontId, glyph: GlyphId) -> anyhow::Result<Bounds<f32>> {
        NoopTextSystem.typographic_bounds(id, glyph)
    }
    fn advance(&self, id: FontId, glyph: GlyphId) -> anyhow::Result<Size<f32>> {
        NoopTextSystem.advance(id, glyph)
    }
    fn glyph_for_char(&self, id: FontId, ch: char) -> Option<GlyphId> {
        NoopTextSystem.glyph_for_char(id, ch)
    }
    fn glyph_raster_bounds(&self, _: &RenderGlyphParams) -> anyhow::Result<Bounds<DevicePixels>> {
        Ok(Bounds {
            origin: point(DevicePixels(-1), DevicePixels(-3)),
            size: size(DevicePixels(2), DevicePixels(2)),
        })
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> anyhow::Result<(Size<DevicePixels>, Vec<u8>)> {
        self.requests.lock().unwrap().push(params.clone());
        Ok((
            bounds.size,
            vec![
                0,
                32_u8
                    .checked_add(params.dilation)
                    .expect("fixture coverage fits"),
                127,
                255,
            ],
        ))
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        NoopTextSystem.layout_line(text, font_size, runs)
    }
    fn recommended_rendering_mode(&self, _: FontId, _: Pixels) -> TextRenderingMode {
        TextRenderingMode::PlatformDefault
    }
    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        if color.l > 0.5 { 4 } else { 0 }
    }
}

#[rstest]
#[case("Lilex-Regular", gpui_kit::FontWeight::NORMAL)]
#[case("Lilex-Bold", gpui_kit::FontWeight::BOLD)]
fn terminal_preserves_native_coverage_and_caches_each_smoothing_level(
    #[case] face: &str,
    #[case] weight: gpui_kit::FontWeight,
    #[values(false, true)] shaped: bool,
) {
    let system = Arc::new(NativeCoverage::default());
    let mut atlas = TextAtlasBuilder::with_platform_text_system(256, 256, system.clone())
        .expect("bounded test atlas");
    let mut command = command_for_face(face);
    command.font_features = Arc::from([FontFeature::new(
        if shaped { *b"calt" } else { *b"liga" },
        0,
    )]);
    for (foreground, dilation) in [(255, 4), (0, 0), (255, 4)] {
        command.attrs.fg = PlanColor {
            r: foreground,
            g: foreground,
            b: foreground,
            a: 255,
        };
        let mut quads = Vec::new();
        atlas.visit_text_command(&command, 2.0, |_, quad| quads.push(quad));
        assert_eq!(quads.len(), 1);
        let tile = atlas.bgra_tile(&quads[0]).unwrap();
        let ink: Vec<_> = tile
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .filter(|alpha| *alpha != 0)
            .collect();
        assert_eq!(
            ink,
            vec![
                32_u8.checked_add(dilation).expect("fixture coverage fits"),
                127,
                255
            ]
        );
        assert!(
            quads[0].rect.min_x < 0.0,
            "native left bearing must survive"
        );
    }
    let requests = system.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "the repeated light foreground reuses its native raster"
    );
    assert_eq!(
        requests
            .iter()
            .map(|request| request.dilation)
            .collect::<Vec<_>>(),
        vec![4, 0]
    );
    assert!(system.fonts.lock().unwrap().iter().all(|font| {
        font.family
            .strip_prefix(".Bootty-Face:")
            .is_some_and(|name| name.starts_with("Lilex-"))
            && font.weight == weight
    }));
}

#[rstest]
fn app_bootstrap_preserves_every_installed_style_of_bundled_families() {
    use std::collections::BTreeSet;

    let system = Arc::new(NativeCoverage::default());
    let text_system = gpui_kit::TextSystem::new(system.clone());
    bootty_ui::assets::BoottyAssets
        .load_fonts(&text_system)
        .unwrap();
    let mut registered = fontdb::Database::new();
    for bytes in system.registered_fonts.lock().unwrap().iter() {
        registered.load_font_data(bytes.clone());
    }
    let available = bootty_ui::font_database::system_font_database();
    for family in ["Maple Mono NF", "Maple Mono", "Lilex", "IBM Plex Sans"] {
        let styles = |database: &fontdb::Database| -> BTreeSet<String> {
            database
                .faces()
                .filter(|face| face.families.iter().any(|(name, _)| name == family))
                .map(|face| face.post_script_name.clone())
                .collect()
        };
        let expected = styles(available);
        assert!(!expected.is_empty(), "{family} has bundled faces");
        assert_eq!(
            styles(&registered),
            expected,
            "{family}: renderer must expose the same styles as the picker"
        );
    }
}

fn command_for_face(face: &str) -> TextCommand {
    TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 24.0),
        text: "H".into(),
        attrs: TextAttrs {
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
        },
        face: Arc::new(ResolvedFontFace {
            assignment: bootty_config::FontStyleAssignment::Automatic,
            family: face.into(),
            fallback_families: vec![],
            style: FontStyle::Regular,
        }),
        font_size: 14.0,
        cell_width: 10.0,
        font_features: Arc::from([FontFeature::new(*b"liga", 0)]),
    }
}

#[rstest]
fn selected_faces_use_the_native_matchers_weight() {
    let database = bootty_ui::font_database::system_font_database();
    for face in database.faces().filter(|face| {
        face.style == fontdb::Style::Normal
            && face
                .families
                .iter()
                .any(|(family, _)| family == "Maple Mono NF")
    }) {
        let system = Arc::new(NativeCoverage::default());
        let mut atlas = TextAtlasBuilder::with_platform_text_system(256, 256, system.clone())
            .expect("bounded test atlas");
        let expected = database
            .with_face_data(face.id, |bytes, index| {
                font_kit::font::Font::from_bytes(Arc::new(bytes.to_vec()), index)
                    .unwrap()
                    .properties()
                    .weight
                    .0
            })
            .unwrap();
        let command = command_for_face(&face.post_script_name);
        atlas.visit_text_command(&command, 2.0, |_, _| {});
        assert_eq!(
            system.fonts.lock().unwrap().last().unwrap().weight,
            gpui_kit::FontWeight(expected),
            "{} must use its native weight, not OS/2 {}",
            face.post_script_name,
            face.weight.0
        );
    }
}

#[rstest]
#[case(true, false, bootty_config::FontStyleAssignment::Named("Regular".into()), "Lilex-Regular")]
#[case(
    false,
    true,
    bootty_config::FontStyleAssignment::Disabled,
    "Lilex-Regular"
)]
#[case(true, true, bootty_config::FontStyleAssignment::Named("Bold Italic".into()), "Lilex-BoldItalic")]
#[case(true, false, bootty_config::FontStyleAssignment::Named("Missing style".into()), "Lilex-Regular")]
fn terminal_style_assignments_reach_native_rasterization(
    #[case] bold: bool,
    #[case] italic: bool,
    #[case] assignment: bootty_config::FontStyleAssignment,
    #[case] expected: &str,
) {
    use bootty_ui::terminal_text::{FontResolver, TerminalTextConfig};
    let system = Arc::new(NativeCoverage::default());
    let mut atlas = TextAtlasBuilder::with_platform_text_system(256, 256, system.clone())
        .expect("bounded test atlas");
    let mut command = command_for_face("Lilex-Regular");
    command.attrs.bold = bold;
    command.attrs.italic = italic;
    let resolver = FontResolver::new(TerminalTextConfig {
        families: vec!["Lilex-Regular".into()],
        style_bold: assignment.clone(),
        style_italic: assignment.clone(),
        style_bold_italic: assignment,
        ..Default::default()
    });
    command.face = Arc::new(resolver.resolve_face(&command.attrs));
    atlas.visit_text_command(&command, 2.0, |_, _| {});
    assert_eq!(
        system
            .fonts
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .family
            .strip_prefix(".Bootty-Face:")
            .unwrap(),
        expected
    );
}

#[rstest]
fn changing_style_assignment_invalidates_the_terminal_raster_cache() {
    use bootty_config::FontStyleAssignment;
    use bootty_ui::terminal_text::{FontResolver, TerminalTextConfig};
    let system = Arc::new(NativeCoverage::default());
    let mut atlas = TextAtlasBuilder::with_platform_text_system(256, 256, system.clone())
        .expect("bounded test atlas");
    let mut command = command_for_face("Lilex-Regular");
    command.attrs.bold = true;
    for assignment in [
        FontStyleAssignment::Automatic,
        FontStyleAssignment::Named("Regular".into()),
    ] {
        let resolver = FontResolver::new(TerminalTextConfig {
            families: vec!["Lilex-Regular".into()],
            style_bold: assignment,
            ..Default::default()
        });
        command.face = Arc::new(resolver.resolve_face(&command.attrs));
        atlas.visit_text_command(&command, 2.0, |_, _| {});
    }
    assert_eq!(
        system
            .fonts
            .lock()
            .unwrap()
            .iter()
            .map(|font| font
                .family
                .strip_prefix(".Bootty-Face:")
                .unwrap()
                .to_owned())
            .collect::<Vec<_>>(),
        vec!["Lilex-Bold", "Lilex-Regular"]
    );
}

#[rstest]
fn fallback_bold_uses_its_own_base_weight() {
    use ab_glyph::Font as _;
    use bootty_ui::terminal_text::{FontResolver, TerminalTextConfig};
    // The bundled Lilex lacks this character; Plex has it in both base and semibold.
    let ch = 'ﬁ';
    let primary = ab_glyph::FontArc::try_from_slice(bootty_ui::assets::LILEX_BOLD).unwrap();
    let fallback =
        ab_glyph::FontArc::try_from_slice(bootty_ui::assets::IBM_PLEX_SANS_SEMIBOLD).unwrap();
    assert_eq!(primary.glyph_id(ch), ab_glyph::GlyphId(0));
    assert_ne!(fallback.glyph_id(ch), ab_glyph::GlyphId(0));
    let system = Arc::new(NativeCoverage::default());
    let mut atlas = TextAtlasBuilder::with_platform_text_system(256, 256, system.clone())
        .expect("bounded test atlas");
    let resolver = FontResolver::new(TerminalTextConfig {
        families: vec!["Lilex-Bold".into(), "IBM Plex Sans".into()],
        ..Default::default()
    });
    let mut command = command_for_face("Lilex-Bold");
    command.text = ch.to_string();
    command.attrs.bold = true;
    command.face = Arc::new(resolver.resolve_face(&command.attrs));
    let mut quads = Vec::new();
    atlas.visit_text_command(&command, 2.0, |_, quad| quads.push(quad));
    let ink = atlas
        .bgra_tile(&quads[0])
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .filter(|alpha| *alpha != 0)
        .collect::<Vec<_>>();
    assert_eq!(
        ink,
        vec![36, 127, 255],
        "fallback's real heavier face must keep native coverage without a synthetic duplicate"
    );
    let fonts = system.fonts.lock().unwrap();
    let font = fonts
        .last()
        .expect("Plex's heavier face needs no synthetic bold raster");
    assert_eq!(
        font.family.strip_prefix(".Bootty-Face:"),
        Some("IBMPlexSans-SmBld")
    );
    drop(fonts);
}
