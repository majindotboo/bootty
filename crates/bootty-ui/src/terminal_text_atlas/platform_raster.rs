use gpui_kit::{
    Bounds, DevicePixels, FontFallbacks, FontRun, PlatformTextSystem, RenderGlyphParams, point, px,
};
use image::{RgbaImage, imageops::overlay};
use num_traits::ToPrimitive as _;

use super::clusters::ShapedCluster;
use crate::paint_plan::PlanColor;
use crate::terminal_text::ResolvedFontFace;

/// Shape the complete grapheme before rasterizing. Character-to-glyph lookup cannot
/// resolve emoji modifiers or ZWJ sequences, and selectors must reach the shaper.
/// `font` is the resolved primary face; the color glyph itself comes from the
/// platform's font cascade.
pub(super) fn rasterize_color_cluster(
    text_system: &dyn PlatformTextSystem,
    mut font: gpui_kit::Font,
    face: &ResolvedFontFace,
    cluster: &ShapedCluster,
    physical_font_size: f32,
    tile: (u32, u32),
    foreground: PlanColor,
) -> Option<Vec<u8>> {
    let (width, height) = tile;
    font.fallbacks = Some(FontFallbacks::from_fonts(face.fallback_families.clone()));
    let font_id = text_system.font_id(&font).ok()?;
    let layout = text_system.layout_line(
        &cluster.text,
        px(physical_font_size),
        &[FontRun {
            len: cluster.text.len(),
            font_id,
        }],
    );
    let mut glyphs = Vec::new();
    for run in &layout.runs {
        for glyph in &run.glyphs {
            let params = RenderGlyphParams {
                font_id: run.font_id,
                glyph_id: glyph.id,
                font_size: layout.font_size,
                subpixel_variant: point(0, 0),
                scale_factor: 1.0,
                is_emoji: glyph.is_emoji,
                subpixel_rendering: false,
                dilation: 0,
            };
            let bounds = text_system.glyph_raster_bounds(&params).ok()?;
            glyphs.push((params, glyph.position, bounds));
        }
    }
    // Monochrome fallbacks still need the terminal foreground color.
    if !glyphs.iter().any(|(params, _, _)| params.is_emoji) {
        return None;
    }
    let bounds = raster_bounds(&glyphs)?;
    let fit = ((width.to_f32()? - 2.0).max(1.0) / bounds.size.width.0.to_f32()?)
        .min((height.to_f32()? - 2.0).max(1.0) / bounds.size.height.0.to_f32()?);
    if !fit.is_finite() || fit <= 0.0 {
        return None;
    }
    for (params, position, bounds) in &mut glyphs {
        params.scale_factor = fit;
        position.x = px(f32::from(position.x) * fit);
        position.y = px(f32::from(position.y) * fit);
        *bounds = text_system.glyph_raster_bounds(params).ok()?;
    }
    let bounds = raster_bounds(&glyphs)?;
    let origin_x = i64::from(width)
        .checked_sub(i64::from(bounds.size.width.0))?
        .checked_div(2)?
        .checked_sub(i64::from(bounds.origin.x.0))?;
    let origin_y = i64::from(height)
        .checked_sub(i64::from(bounds.size.height.0))?
        .checked_div(2)?
        .checked_sub(i64::from(bounds.origin.y.0))?;
    let image_len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    let mut image = RgbaImage::from_raw(width, height, vec![0; image_len])?;
    for (params, position, bounds) in glyphs {
        let (size, mut pixels) = text_system.rasterize_glyph(&params, bounds).ok()?;
        if params.is_emoji {
            // GPUI's native color raster paths use BGRA; our atlas stores RGBA.
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
        } else {
            pixels = pixels
                .into_iter()
                .flat_map(|alpha| [foreground.r, foreground.g, foreground.b, alpha])
                .collect();
        }
        let glyph = RgbaImage::from_raw(
            u32::try_from(size.width.0).ok()?,
            u32::try_from(size.height.0).ok()?,
            pixels,
        )?;
        overlay(
            &mut image,
            &glyph,
            origin_x
                .checked_add(f32::from(position.x).round().to_i64()?)?
                .checked_add(i64::from(bounds.origin.x.0))?,
            origin_y
                .checked_sub(f32::from(position.y).round().to_i64()?)?
                .checked_add(i64::from(bounds.origin.y.0))?,
        );
    }
    Some(image.into_raw())
}

fn raster_bounds(
    glyphs: &[(
        RenderGlyphParams,
        gpui_kit::Point<gpui_kit::Pixels>,
        Bounds<DevicePixels>,
    )],
) -> Option<Bounds<DevicePixels>> {
    let mut union = None;
    for (_, position, bounds) in glyphs {
        let left = bounds
            .origin
            .x
            .0
            .checked_add(f32::from(position.x).round().to_i32()?)?;
        let top = bounds
            .origin
            .y
            .0
            .checked_add((-f32::from(position.y).round()).to_i32()?)?;
        let right = left.checked_add(bounds.size.width.0)?;
        let bottom = top.checked_add(bounds.size.height.0)?;
        union = Some(
            union.map_or([left, top, right, bottom], |[x1, y1, x2, y2]: [i32; 4]| {
                [x1.min(left), y1.min(top), x2.max(right), y2.max(bottom)]
            }),
        );
    }
    let [left, top, right, bottom] = union?;
    Some(Bounds {
        origin: point(DevicePixels(left), DevicePixels(top)),
        size: gpui_kit::size(
            DevicePixels(right.checked_sub(left)?),
            DevicePixels(bottom.checked_sub(top)?),
        ),
    })
}

/// Keep terminal shaping and geometry, but use the host's rasterizer for native stroke coverage.
pub(super) fn rasterize_text_cluster(
    fonts: &mut super::font_library::FontLibrary,
    request: super::font_raster::RasterizeClusterRequest<'_>,
) -> Option<super::atlas::GlyphRaster> {
    use ab_glyph::{Font as _, ScaleFont as _};
    let system = request.platform_text_system?;
    let cluster = request.cluster;
    if cluster.text.chars().any(|ch| {
        super::clusters::is_private_use(ch)
            || crate::terminal_font_face::terminal_glyph_constraint(u32::from(ch)).does_anything()
    }) {
        return None;
    }
    let physical_size = request.font_size * request.pixels_per_point;
    let (font, synthesize_bold) =
        fonts.platform_font_for_cluster(request.face, cluster, physical_size)?;
    let font_id = system.font_id(&font).ok()?;
    let primary = fonts.font_for_face(request.face)?;
    let scaled = primary.as_scaled(crate::font_database::font_pixel_scale(
        &primary,
        physical_size,
    ));
    let (width, height) = request.tile;
    let baseline = ((height.to_f32()? - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let fallback_glyphs;
    let glyphs: &[_] = if cluster.glyphs.is_empty() {
        fallback_glyphs = fonts.shape_fallback_cluster(
            request.face,
            cluster,
            request.font_size,
            physical_size,
        )?;
        &fallback_glyphs
    } else {
        &cluster.glyphs
    };
    let first_cell = glyphs.iter().map(|glyph| glyph.cluster).min()?;
    let cell_width = width.to_f32()? / f32::from(request.constraint_cells.max(1));
    let dilation = glyph_dilation(system, request.foreground);
    let mut tiles = Vec::with_capacity(glyphs.len());
    let mut left = 0;
    let mut top = 0;
    let mut right = i32::try_from(width).ok()?;
    let mut bottom = i32::try_from(height).ok()?;
    for glyph in glyphs {
        let x = f32::mul_add(
            glyph.x_offset,
            request.pixels_per_point,
            glyph.cluster.checked_sub(first_cell)?.to_f32()? * cell_width,
        );
        let y = f32::mul_add(glyph.y_offset, -request.pixels_per_point, baseline);
        for bold_copy in 0..=u8::from(synthesize_bold) {
            let x = f32::mul_add(
                f32::from(bold_copy),
                (request.pixels_per_point * 0.45).max(1.0),
                x,
            );
            let x = (x * f32::from(gpui_kit::SUBPIXEL_VARIANTS_X)).round()
                / f32::from(gpui_kit::SUBPIXEL_VARIANTS_X);
            let y = (y * f32::from(gpui_kit::SUBPIXEL_VARIANTS_Y)).round()
                / f32::from(gpui_kit::SUBPIXEL_VARIANTS_Y);
            let params = RenderGlyphParams {
                font_id,
                glyph_id: gpui_kit::GlyphId(u32::from(glyph.glyph_id)),
                font_size: px(request.font_size),
                subpixel_variant: point(
                    ((x - x.floor()) * f32::from(gpui_kit::SUBPIXEL_VARIANTS_X)).to_u8()?,
                    ((y - y.floor()) * f32::from(gpui_kit::SUBPIXEL_VARIANTS_Y)).to_u8()?,
                ),
                scale_factor: request.pixels_per_point,
                is_emoji: false,
                subpixel_rendering: false,
                dilation,
            };
            let bounds = system.glyph_raster_bounds(&params).ok()?;
            if bounds.size.width.0 == 0 || bounds.size.height.0 == 0 {
                continue;
            }
            let (size, pixels) = system.rasterize_glyph(&params, bounds).ok()?;
            let origin_x = x.floor().to_i32()?.checked_add(bounds.origin.x.0)?;
            let origin_y = y.floor().to_i32()?.checked_add(bounds.origin.y.0)?;
            left = left.min(origin_x);
            top = top.min(origin_y);
            right = right.max(origin_x.checked_add(size.width.0)?);
            bottom = bottom.max(origin_y.checked_add(size.height.0)?);
            tiles.push(GlyphTile {
                x: origin_x,
                y: origin_y,
                size,
                pixels,
            });
        }
    }
    compose_text_tiles(tiles, [left, top, right, bottom])
}

struct GlyphTile {
    x: i32,
    y: i32,
    size: gpui_kit::Size<DevicePixels>,
    pixels: Vec<u8>,
}

fn compose_text_tiles(
    tiles: Vec<GlyphTile>,
    [left, top, right, bottom]: [i32; 4],
) -> Option<super::atlas::GlyphRaster> {
    let width = u32::try_from(right.checked_sub(left)?).ok()?;
    let height = u32::try_from(bottom.checked_sub(top)?).ok()?;
    let stride = usize::try_from(width).ok()?;
    let mut pixels = vec![0_u8; stride.checked_mul(usize::try_from(height).ok()?)?];
    for GlyphTile {
        x,
        y,
        size,
        pixels: tile,
    } in tiles
    {
        let tile_width = usize::try_from(size.width.0).ok()?;
        if tile_width == 0 {
            continue;
        }
        let tile_x = usize::try_from(x.checked_sub(left)?).ok()?;
        let tile_y = usize::try_from(y.checked_sub(top)?).ok()?;
        for (row, source) in tile.chunks_exact(tile_width).enumerate() {
            let start = tile_y
                .checked_add(row)?
                .checked_mul(stride)?
                .checked_add(tile_x)?;
            for (dst, src) in pixels
                .get_mut(start..start.checked_add(tile_width)?)?
                .iter_mut()
                .zip(source)
            {
                *dst = (*dst).max(*src);
            }
        }
    }
    let mut raster = super::atlas::GlyphRaster::new(pixels, false, width, height);
    raster.offset = [left, top];
    Some(raster)
}

pub(super) fn glyph_dilation(system: &dyn PlatformTextSystem, color: PlanColor) -> u8 {
    system.glyph_dilation_for_color(
        gpui_kit::rgba(u32::from_be_bytes([color.r, color.g, color.b, color.a])).into(),
    )
}
