use super::atlas::alpha_to_atlas_pixels;
use super::atlas::{GlyphAtlasFormat, GlyphRaster};
use super::clusters::{
    ShapedCluster, ShapedGlyph, is_color_emoji_cluster, is_combining_mark, is_private_use,
    is_variation_selector,
};
use super::coretext;
use super::font_library::{FontLibrary, font_face_metrics};
use crate::font_database::font_pixel_scale;
use crate::paint_plan::PlanColor;
use crate::terminal_font_face::{FontFaceMetrics, GlyphSize, terminal_glyph_constraint};
use crate::terminal_text::ResolvedFontFace;
use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont, point};
use num_traits::ToPrimitive as _;

#[derive(Clone, Copy)]
pub(super) struct RasterizeClusterRequest<'a> {
    pub(super) platform_text_system: Option<&'a dyn gpui_kit::PlatformTextSystem>,
    pub(super) foreground: PlanColor,
    pub(super) face: &'a ResolvedFontFace,
    pub(super) cluster: &'a ShapedCluster,
    pub(super) font_size: f32,
    pub(super) pixels_per_point: f32,
    pub(super) constraint_cells: u16,
    pub(super) tile: (u32, u32),
    pub(super) format: GlyphAtlasFormat,
}

#[derive(Clone, Copy)]
pub(super) struct PositionedClusterGlyphRequest {
    ch: char,
    glyph_id: GlyphId,
    scale: PxScale,
    position: ab_glyph::Point,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    tile: (u32, u32),
}

pub(super) fn rasterize_cluster(
    fonts: &mut FontLibrary,
    request: RasterizeClusterRequest<'_>,
) -> GlyphRaster {
    let RasterizeClusterRequest {
        platform_text_system,
        foreground,
        face,
        cluster,
        font_size,
        pixels_per_point,
        constraint_cells,
        tile: (width, height),
        format,
    } = request;
    if cluster.is_whitespace {
        return GlyphRaster::new(Vec::new(), false, width, height);
    }
    // An explicit emoji-presentation cluster (VS16, or a default-emoji codepoint) renders as a
    // color emoji, even when the primary font carries a monochrome glyph for the base symbol —
    // otherwise ⚠️/❤️ draw as a theme-tinted text glyph, and the rendering flips with whatever
    // the shaper happened to produce. Skip the by-glyph path so it reaches the color path below.
    let prefer_color_emoji = format == GlyphAtlasFormat::Rgba && is_color_emoji_cluster(cluster);
    // Headless hosts and fonts unavailable to the platform retain the portable raster fallback.
    if !prefer_color_emoji
        && let Some(mut raster) = super::platform_raster::rasterize_text_cluster(fonts, request)
    {
        raster.pixels = alpha_to_atlas_pixels(format, raster.pixels);
        return raster;
    }
    if !cluster.glyphs.is_empty()
        && !prefer_color_emoji
        && let Some(font) = fonts.font_for_face(face)
        && let Some(mut raster) = rasterize_glyph_cluster(RasterizeGlyphClusterRequest {
            font: &font,
            glyphs: &cluster.glyphs,
            physical_font_size: font_size * pixels_per_point,
            pixels_per_point,
            synthesize_bold: fonts.synthesize_bold_for_face(face),
            baseline: None,
            constraint_cells,
            tile: (width, height),
        })
    {
        raster.pixels = alpha_to_atlas_pixels(format, raster.pixels);
        return raster;
    }
    if prefer_color_emoji
        && let Some(platform_text_system) = platform_text_system
        && let Some(font) = fonts.primary_platform_font(face)
        && let Some(pixels) = super::platform_raster::rasterize_color_cluster(
            platform_text_system,
            font,
            face,
            cluster,
            font_size * pixels_per_point,
            (width, height),
            foreground,
        )
    {
        return GlyphRaster::new(pixels, true, width, height);
    }
    rasterize_fallback_cluster(fonts, request)
}

fn rasterize_fallback_cluster(
    fonts: &mut FontLibrary,
    request: RasterizeClusterRequest<'_>,
) -> GlyphRaster {
    let RasterizeClusterRequest {
        face,
        cluster,
        font_size,
        pixels_per_point,
        constraint_cells,
        tile: (width, height),
        format,
        ..
    } = request;
    let physical_font_size = font_size * pixels_per_point;
    let primary_font = fonts.font_for_face(face);
    let primary_metrics = primary_font.as_ref().map(|font| {
        let scale = font_pixel_scale(font, physical_font_size);
        fonts.font_face_metrics_for(face, font, scale, constraint_cells, width, height)
    });
    if let Some(raster) = rasterize_symbol_fallback(request, primary_metrics) {
        return raster;
    }
    let Some((font, synthesize_bold)) = fonts.font_for_cluster(face, cluster, physical_font_size)
    else {
        return GlyphRaster::new(
            alpha_to_atlas_pixels(format, fallback_cluster_mask(cluster, width, height)),
            false,
            width,
            height,
        );
    };
    let scale = font_pixel_scale(&font, physical_font_size);
    let scaled = font.as_scaled(scale);
    let metrics = primary_metrics
        .unwrap_or_else(|| font_face_metrics(&font, scale, constraint_cells, width, height));
    let baseline_font = primary_font.as_ref().unwrap_or(&font);
    let baseline_scale =
        baseline_font.as_scaled(font_pixel_scale(baseline_font, physical_font_size));
    let baseline = ((height.to_f32().unwrap_or_default() - baseline_scale.height()) * 0.5).max(0.0)
        + baseline_scale.ascent();
    if cluster.text.chars().any(is_combining_mark)
        && let Some(glyphs) =
            fonts.shape_fallback_cluster(face, cluster, font_size, physical_font_size)
        && let Some(mut raster) = rasterize_glyph_cluster(RasterizeGlyphClusterRequest {
            font: &font,
            glyphs: &glyphs,
            physical_font_size,
            pixels_per_point,
            synthesize_bold,
            baseline: Some(baseline),
            constraint_cells,
            tile: (width, height),
        })
    {
        raster.pixels = alpha_to_atlas_pixels(format, raster.pixels);
        return raster;
    }
    let mut pen_x = 0.0_f32;
    let mut outlines = Vec::new();

    for ch in cluster.text.chars() {
        if is_combining_mark(ch) || is_variation_selector(ch) {
            continue;
        }
        let glyph_id = scaled.glyph_id(ch);
        if glyph_id.0 == 0 {
            continue;
        }
        let glyph = positioned_cluster_glyph(
            &font,
            PositionedClusterGlyphRequest {
                ch,
                glyph_id,
                scale,
                position: point(pen_x, baseline),
                metrics,
                constraint_cells,
                tile: (width, height),
            },
        );
        let glyph_scaled = font.as_scaled(glyph.scale);
        outlines.extend(glyph_scaled.outline_glyph(glyph.clone()));
        if synthesize_bold {
            let mut bold = glyph;
            bold.position.x += (pixels_per_point * 0.45).max(1.0);
            outlines.extend(glyph_scaled.outline_glyph(bold));
        }
        pen_x += scaled.h_advance(glyph_id);
    }

    let mut raster = rasterize_outlines(&outlines, width, height)
        .filter(|raster| raster.pixels.iter().any(|value| *value > 0))
        .unwrap_or_else(|| {
            GlyphRaster::new(
                fallback_cluster_mask(cluster, width, height),
                false,
                width,
                height,
            )
        });
    raster.pixels = alpha_to_atlas_pixels(format, raster.pixels);
    raster
}

fn rasterize_symbol_fallback(
    request: RasterizeClusterRequest<'_>,
    metrics: Option<FontFaceMetrics>,
) -> Option<GlyphRaster> {
    if request.cluster.text.chars().any(is_private_use) {
        return None;
    }
    let mut raster = coretext::rasterize_symbol_cluster(
        request.face,
        request.cluster,
        request.font_size * request.pixels_per_point,
        metrics?,
        request.constraint_cells,
        request.tile.0,
        request.tile.1,
    )?;
    raster.pixels = alpha_to_atlas_pixels(request.format, raster.pixels);
    Some(raster)
}

// Keep natural bearings and a shared baseline. The atlas stores ink outside the grid
// span; only the terminal viewport clips it, not individual cells or style runs.
fn rasterize_outlines(
    outlines: &[ab_glyph::OutlinedGlyph],
    width: u32,
    height: u32,
) -> Option<GlyphRaster> {
    let mut left = 0.0_f32;
    let mut top = 0.0_f32;
    let mut right = width.to_f32().unwrap_or_default();
    let mut bottom = height.to_f32().unwrap_or_default();
    for outline in outlines {
        let bounds = outline.px_bounds();
        left = left.min(bounds.min.x);
        top = top.min(bounds.min.y);
        right = right.max(bounds.max.x);
        bottom = bottom.max(bounds.max.y);
    }
    let width = (right - left).to_u32()?;
    let height = (bottom - top).to_u32()?;
    let mut raster = GlyphRaster::new(alpha_mask(width, height)?, false, width, height);
    raster.offset = [left.to_i32()?, top.to_i32()?];
    for outline in outlines {
        let bounds = outline.px_bounds();
        let origin_x = (bounds.min.x - left).to_u32()?;
        let origin_y = (bounds.min.y - top).to_u32()?;
        outline.draw(|x, y, coverage| {
            let Some(px) = origin_x.checked_add(x) else {
                return;
            };
            let index = origin_y
                .checked_add(y)
                .and_then(|py| py.checked_mul(width))
                .and_then(|row| row.checked_add(px))
                .and_then(|index| usize::try_from(index).ok());
            if let Some(dst) = index.and_then(|index| raster.pixels.get_mut(index)) {
                let coverage = (coverage * 255.0)
                    .round()
                    .clamp(0.0, 255.0)
                    .to_u8()
                    .unwrap_or_default();
                *dst = (*dst).max(coverage);
            }
        });
    }
    Some(raster)
}

#[derive(Clone, Copy)]
struct RasterizeGlyphClusterRequest<'a> {
    font: &'a FontArc,
    glyphs: &'a [ShapedGlyph],
    physical_font_size: f32,
    pixels_per_point: f32,
    synthesize_bold: bool,
    baseline: Option<f32>,
    constraint_cells: u16,
    tile: (u32, u32),
}

/// Draws a shaped cluster from its glyph ids into an alpha mask. Glyph offsets
/// arrive in logical pixels (shaped at the logical font size) and scale up by
/// `pixels_per_point` to device pixels. Synthetic weight is only used when the
/// selected font has no bold face.
fn rasterize_glyph_cluster(request: RasterizeGlyphClusterRequest<'_>) -> Option<GlyphRaster> {
    let RasterizeGlyphClusterRequest {
        font,
        glyphs,
        physical_font_size,
        pixels_per_point,
        synthesize_bold,
        baseline,
        constraint_cells,
        tile: (width, height),
    } = request;
    let scale = font_pixel_scale(font, physical_font_size);
    let scaled = font.as_scaled(scale);
    let baseline = baseline.unwrap_or_else(|| {
        ((height.to_f32().unwrap_or_default() - scaled.height()) * 0.5).max(0.0) + scaled.ascent()
    });
    let bold_offset = (pixels_per_point * 0.45).max(1.0);

    let cluster_start = glyphs.iter().map(|glyph| glyph.cluster).min().unwrap_or(0);
    let cell_width = width.to_f32().unwrap_or_default() / f32::from(constraint_cells.max(1));
    let mut outlines = Vec::new();

    for glyph in glyphs {
        let glyph_id = GlyphId(glyph.glyph_id);
        let cell_offset = glyph
            .cluster
            .saturating_sub(cluster_start)
            .to_f32()
            .unwrap_or_default()
            * cell_width;
        let x = f32::mul_add(glyph.x_offset, pixels_per_point, cell_offset);
        let y = f32::mul_add(glyph.y_offset, -pixels_per_point, baseline);
        outlines.extend(scaled.outline_glyph(glyph_id.with_scale_and_position(scale, point(x, y))));
        if synthesize_bold {
            outlines.extend(
                scaled.outline_glyph(
                    glyph_id.with_scale_and_position(scale, point(x + bold_offset, y)),
                ),
            );
        }
    }
    (!outlines.is_empty())
        .then(|| rasterize_outlines(&outlines, width, height))
        .flatten()
}

fn positioned_cluster_glyph(
    font: &FontArc,
    request: PositionedClusterGlyphRequest,
) -> ab_glyph::Glyph {
    let PositionedClusterGlyphRequest {
        ch,
        glyph_id,
        scale,
        position,
        metrics,
        constraint_cells,
        tile,
    } = request;
    let glyph = glyph_id.with_scale_and_position(scale, position);
    let scaled = font.as_scaled(scale);
    let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
        return glyph;
    };
    let bounds = outlined.px_bounds();
    let tile_width = tile.0.to_f32().unwrap_or_default();
    let tile_height = tile.1.to_f32().unwrap_or_default();

    let constraint = terminal_glyph_constraint(u32::from(ch));
    if constraint.does_anything() {
        let constrained = constraint.constrain(
            GlyphSize {
                width: f64::from(bounds.width()),
                height: f64::from(bounds.height()),
                x: f64::from(bounds.min.x),
                y: f64::from(tile_height - bounds.max.y),
            },
            metrics,
            u8::try_from(constraint_cells).unwrap_or(u8::MAX),
        );
        let (Some(constrained_width), Some(constrained_x), Some(constrained_top)) = (
            constrained.width.to_f32().filter(|value| value.is_finite()),
            constrained.x.to_f32().filter(|value| value.is_finite()),
            (constrained.y + constrained.height)
                .to_f32()
                .filter(|value| value.is_finite()),
        ) else {
            return glyph;
        };
        let scale_factor = (constrained_width / bounds.width()).max(0.01);
        let scale = PxScale {
            x: scale.x * scale_factor,
            y: scale.y * scale_factor,
        };
        let scaled = font.as_scaled(scale);
        let glyph = glyph_id.with_scale_and_position(scale, point(0.0, 0.0));
        let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
            return glyph;
        };
        let bounds = outlined.px_bounds();
        return glyph_id.with_scale_and_position(
            scale,
            point(
                constrained_x - bounds.min.x,
                tile_height - constrained_top - bounds.min.y,
            ),
        );
    }

    // Text keeps natural size, baseline and bearings. Only private-use icons are
    // fitted here; explicit symbol constraints were handled above.
    if !is_private_use(ch) {
        return glyph;
    }
    let fit = (tile_width / bounds.width())
        .min(tile_height / bounds.height())
        .min(1.0);
    let scale = PxScale {
        x: scale.x * fit,
        y: scale.y * fit,
    };
    let scaled = font.as_scaled(scale);
    let baseline = ((tile_height - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let glyph = glyph_id.with_scale_and_position(scale, point(position.x, baseline));
    let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
        return glyph;
    };
    let bounds = outlined.px_bounds();
    let dx = (tile_width - bounds.width()).mul_add(0.5, -bounds.min.x);
    let dy = (tile_height - bounds.height()).mul_add(0.5, -bounds.min.y);
    glyph_id.with_scale_and_position(scale, point(position.x + dx, baseline + dy))
}

fn alpha_mask(width: u32, height: u32) -> Option<Vec<u8>> {
    let len = usize::try_from(width.checked_mul(height)?).ok()?;
    Some(vec![0; len])
}

fn fallback_cluster_mask(cluster: &ShapedCluster, width: u32, height: u32) -> Vec<u8> {
    let Some(mut alpha) = alpha_mask(width, height) else {
        return Vec::new();
    };
    if cluster.is_whitespace {
        return alpha;
    }
    if let Some(ch) = cluster.text.chars().next()
        && draw_fallback_arrow(&mut alpha, ch, width, height)
    {
        return alpha;
    }
    let seed = u32::from(cluster.text.chars().next().unwrap_or(' '));
    let margin_x = (width / 6).min(width.saturating_sub(1));
    let margin_y = (height / 6).min(height.saturating_sub(1));
    let Ok(stride) = usize::try_from(width.max(1)) else {
        return alpha;
    };
    let rows = (0..height).zip(alpha.chunks_exact_mut(stride));
    for (y, row) in rows.filter(|(y, _)| *y >= margin_y && *y < height.saturating_sub(margin_y)) {
        for (x, pixel) in (0..width)
            .zip(row)
            .filter(|(x, _)| *x >= margin_x && *x < width.saturating_sub(margin_x))
        {
            let pattern = (x % 3)
                .saturating_add(y % 3)
                .saturating_add(seed % 3)
                .is_multiple_of(3);
            if pattern || cluster.text != " " {
                *pixel = 220;
            }
        }
    }
    alpha
}

fn draw_fallback_arrow(alpha: &mut [u8], ch: char, width: u32, height: u32) -> bool {
    let up = match ch {
        '\u{21e1}' | '\u{2191}' | '\u{21e7}' => true,
        '\u{21e3}' | '\u{2193}' | '\u{21e9}' => false,
        _ => return false,
    };
    let stroke = (width / 6).max(1);
    let center_x = width / 2;
    let top = height / 4;
    let bottom = height.saturating_sub(height / 4);

    let stem_y = if up {
        top.saturating_add(height / 8)
    } else {
        top
    };
    fill_pixel_rect(
        alpha,
        width,
        center_x.saturating_sub(stroke / 2),
        stem_y,
        stroke,
        bottom.saturating_sub(top.saturating_add(height / 8)),
    );
    for offset in 0..=(width / 4).max(1) {
        let y = if up {
            top.saturating_add(offset)
        } else {
            bottom.saturating_sub(offset)
        };
        for x in [
            center_x.saturating_sub(offset),
            center_x.saturating_add(offset),
        ] {
            fill_pixel_rect(alpha, width, x, y, stroke, stroke);
        }
    }
    true
}

fn fill_pixel_rect(alpha: &mut [u8], width: u32, x: u32, y: u32, rect_width: u32, height: u32) {
    let Ok(stride) = usize::try_from(width.max(1)) else {
        return;
    };
    let (Ok(x), Ok(y), Ok(rect_width), Ok(height)) = (
        usize::try_from(x),
        usize::try_from(y),
        usize::try_from(rect_width),
        usize::try_from(height),
    ) else {
        return;
    };
    for row in alpha.chunks_exact_mut(stride).skip(y).take(height) {
        for pixel in row.iter_mut().skip(x).take(rect_width) {
            *pixel = 220;
        }
    }
}
