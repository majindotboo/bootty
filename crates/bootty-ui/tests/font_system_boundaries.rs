#![cfg(test)]
#![cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]

use bootty_ui::{
    assets::BoottyAssets,
    font_mapping::{FontMappings, wrap_text_system},
};
use gpui_kit::{
    FontId, FontRun, GlyphId, NoopTextSystem, PlatformTextSystem, RenderGlyphParams, TextSystem,
    point, px,
};
use pretty_assertions::assert_eq;
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

#[rstest]
#[case(1)]
#[case(2)]
#[case(3)]
#[case(usize::MAX)]
fn native_shaping_rejects_invalid_utf8_run_boundaries(
    native_system: Arc<dyn PlatformTextSystem>,
    #[case] len: usize,
) {
    let font_id = native_system.font_id(&gpui_kit::font("Lilex")).unwrap();
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
