#![allow(
    unsafe_code,
    reason = "Native glyph rasterization requires CoreText pointer APIs; buffers and owned handles are confined to this adapter."
)]

use super::atlas::GlyphRaster;
use super::clusters::ShapedCluster;
use crate::{terminal_font_face::FontFaceMetrics, terminal_text::ResolvedFontFace};

#[cfg(target_os = "macos")]
use crate::terminal_font_face::{GlyphConstraintSize, GlyphSize, terminal_glyph_constraint};

#[cfg(target_os = "macos")]
use core_foundation::{
    base::{CFRange, TCFType},
    string::{CFString, CFStringRef},
};
#[cfg(target_os = "macos")]
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
#[cfg(target_os = "macos")]
use core_text::font::{CTFont, CTFontRef};
#[cfg(target_os = "macos")]
use num_traits::ToPrimitive as _;
#[cfg(target_os = "macos")]
use std::ffi::c_void;

#[cfg(target_os = "macos")]
type CGContextRef = *mut c_void;
#[cfg(target_os = "macos")]
type CGColorSpaceRef = *mut c_void;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceGray() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextRelease(context: CGContextRef);
    fn CGContextSetGrayFillColor(context: CGContextRef, gray: f64, alpha: f64);
    fn CGContextFillRect(context: CGContextRef, rect: CGRect);
    fn CGContextSetAllowsFontSmoothing(context: CGContextRef, allows: bool);
    fn CGContextSetShouldSmoothFonts(context: CGContextRef, should: bool);
    fn CGContextSetAllowsFontSubpixelPositioning(context: CGContextRef, allows: bool);
    fn CGContextSetShouldSubpixelPositionFonts(context: CGContextRef, should: bool);
    fn CGContextSetAllowsFontSubpixelQuantization(context: CGContextRef, allows: bool);
    fn CGContextSetShouldSubpixelQuantizeFonts(context: CGContextRef, should: bool);
    fn CGContextSetAllowsAntialiasing(context: CGContextRef, allows: bool);
    fn CGContextSetShouldAntialias(context: CGContextRef, should: bool);
    fn CGContextTranslateCTM(context: CGContextRef, tx: f64, ty: f64);
    fn CGContextScaleCTM(context: CGContextRef, sx: f64, sy: f64);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontDrawGlyphs(
        font: CTFontRef,
        glyphs: *const u16,
        positions: *const CGPoint,
        count: usize,
        context: CGContextRef,
    );
    fn CTFontCreateForString(
        current_font: CTFontRef,
        string: CFStringRef,
        range: CFRange,
    ) -> CTFontRef;
}

// CoreText has no notion of the CSS-style generic "monospace": CTFontCreateWithName falls back to
// Helvetica (proportional), which then shadows the per-glyph cascade for any symbol Helvetica
// happens to carry. Resolve the generic through the shared font DB — the same source the primary
// text path uses — so symbol/color fallbacks also land on a real monospaced face. Menlo is the
// guaranteed-present macOS backstop.
#[cfg(target_os = "macos")]
pub(super) fn coretext_family_name(family: &str) -> std::borrow::Cow<'_, str> {
    let family = crate::font_database::font_name_with_fallbacks(family, family);
    if family != "monospace" {
        return std::borrow::Cow::Borrowed(family);
    }
    let database = crate::font_database::system_font_database();
    database
        .query(&fontdb::Query {
            families: &[fontdb::Family::Monospace],
            ..fontdb::Query::default()
        })
        .and_then(|id| database.faces().find(|face| face.id == id))
        .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        .map_or(std::borrow::Cow::Borrowed("Menlo"), std::borrow::Cow::Owned)
}

#[cfg(target_os = "macos")]
pub(super) fn rasterize_symbol_cluster(
    face: &ResolvedFontFace,
    cluster: &ShapedCluster,
    physical_font_size: f32,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    width: u32,
    height: u32,
) -> Option<GlyphRaster> {
    let ch = cluster.text.chars().next()?;
    if cluster.text.chars().nth(1).is_some()
        || !terminal_glyph_constraint(u32::from(ch)).does_anything()
    {
        return None;
    }

    let families = std::iter::once(face.family.as_str())
        .chain(face.fallback_families.iter().map(String::as_str));

    for family in families {
        let database = crate::font_database::system_font_database();
        let query = if family == "monospace" {
            fontdb::Family::Monospace
        } else {
            fontdb::Family::Name(family)
        };
        let family = crate::font_database::query_assigned_font_id(
            database,
            &[query],
            face.style,
            &face.assignment,
        )
        .and_then(|id| database.face(id))
        .map_or(family, |font| font.post_script_name.as_str());

        if let Some(alpha) = rasterize_symbol_with_family(
            family,
            ch,
            physical_font_size,
            metrics,
            constraint_cells,
            width,
            height,
        ) {
            return Some(alpha);
        }
    }

    let names = fallback_names(&face.family, ch, physical_font_size)?;
    rasterize_symbol_with_family(
        &names.postscript,
        ch,
        physical_font_size,
        metrics,
        constraint_cells,
        width,
        height,
    )
}

#[cfg(not(target_os = "macos"))]
pub(super) fn rasterize_symbol_cluster(
    _face: &ResolvedFontFace,
    _cluster: &ShapedCluster,
    _physical_font_size: f32,
    _metrics: FontFaceMetrics,
    _constraint_cells: u16,
    _width: u32,
    _height: u32,
) -> Option<GlyphRaster> {
    None
}

#[cfg(target_os = "macos")]
pub(super) fn rasterize_symbol_with_family(
    family: &str,
    ch: char,
    physical_font_size: f32,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    width: u32,
    height: u32,
) -> Option<GlyphRaster> {
    let mut utf16 = [0_u16; 2];
    if ch.encode_utf16(&mut utf16).len() != 1 {
        return None;
    }
    let font = core_text::font::new_from_name(
        coretext_family_name(family).as_ref(),
        f64::from(physical_font_size.max(1.0)),
    )
    .ok()?;
    let mut glyph = 0_u16;
    // SAFETY: both buffers contain the single UTF-16 code unit supplied to CoreText.
    let supports = unsafe { font.get_glyphs_for_characters(utf16.as_ptr(), &raw mut glyph, 1) };
    if !supports || glyph == 0 {
        return None;
    }
    let rect = font.get_bounding_rects_for_glyphs(0, &[glyph]);
    if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return None;
    }
    let constraint = terminal_glyph_constraint(u32::from(ch));
    let mut constrained = constraint.constrain(
        GlyphSize {
            width: rect.size.width,
            height: rect.size.height,
            x: rect.origin.x,
            y: rect.origin.y + f64::from(metrics.cell_baseline),
        },
        metrics,
        u8::try_from(constraint_cells).unwrap_or(u8::MAX),
    );
    if constraint.size != GlyphConstraintSize::Stretch {
        let dx = (f64::from(metrics.cell_width) - metrics.face_width) / 2.0;
        constrained.x += dx;
        if dx < 0.0 {
            constrained.x -= dx.trunc();
        }
    }
    rasterize_symbol_glyph(&font, glyph, rect, constrained, [width, height])
}

#[cfg(target_os = "macos")]
fn rasterize_symbol_glyph(
    font: &CTFont,
    glyph: u16,
    rect: CGRect,
    constrained: GlyphSize,
    [width, height]: [u32; 2],
) -> Option<GlyphRaster> {
    let px_x = constrained.x.floor().to_i32()?;
    let px_y = constrained.y.floor().to_i32()?;
    let frac_x = constrained.x - constrained.x.floor();
    let frac_y = constrained.y - constrained.y.floor();
    let px_width = (constrained.width + frac_x).ceil().max(1.0).to_u32()?;
    let px_height = (constrained.height + frac_y).ceil().max(1.0).to_u32()?;
    if px_width > width.saturating_mul(2) || px_height > height.saturating_mul(2) {
        return None;
    }
    let top_y = i32::try_from(height)
        .ok()?
        .checked_sub(px_y.checked_add(i32::try_from(px_height).ok()?)?)?;
    let mut mask = vec![0; pixel_buffer_len(px_width, px_height, 1)?];
    draw_symbol_mask(
        font,
        glyph,
        rect,
        constrained,
        [frac_x, frac_y],
        [px_width, px_height],
        &mut mask,
    )?;
    // Preserve the complete native glyph and its bearings. The terminal viewport,
    // not an intermediate cell-sized mask, owns clipping.
    let mut raster = GlyphRaster::new(mask, false, px_width, px_height);
    raster.offset = [px_x, top_y];
    Some(raster)
}

#[cfg(target_os = "macos")]
fn draw_symbol_mask(
    font: &CTFont,
    glyph: u16,
    rect: CGRect,
    constrained: GlyphSize,
    [frac_x, frac_y]: [f64; 2],
    [width, height]: [u32; 2],
    mask: &mut [u8],
) -> Option<()> {
    let buffer_width = usize::try_from(width).ok()?;
    let buffer_height = usize::try_from(height).ok()?;
    // SAFETY: the caller allocates the checked width × height buffer. Native Create
    // handles are checked and released; the context cannot outlive the borrowed mask.
    unsafe {
        let color_space = CGColorSpaceCreateDeviceGray();
        if color_space.is_null() {
            return None;
        }
        let context = CGBitmapContextCreate(
            mask.as_mut_ptr().cast(),
            buffer_width,
            buffer_height,
            8,
            buffer_width,
            color_space,
            0,
        );
        CGColorSpaceRelease(color_space);
        if context.is_null() {
            return None;
        }
        CGContextSetGrayFillColor(context, 0.0, 1.0);
        CGContextFillRect(
            context,
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: f64::from(width),
                    height: f64::from(height),
                },
            },
        );
        CGContextSetAllowsFontSmoothing(context, true);
        CGContextSetShouldSmoothFonts(context, false);
        CGContextSetAllowsFontSubpixelPositioning(context, true);
        CGContextSetShouldSubpixelPositionFonts(context, true);
        CGContextSetAllowsFontSubpixelQuantization(context, false);
        CGContextSetShouldSubpixelQuantizeFonts(context, false);
        CGContextSetAllowsAntialiasing(context, true);
        CGContextSetShouldAntialias(context, true);
        CGContextSetGrayFillColor(context, 1.0, 1.0);
        CGContextTranslateCTM(context, frac_x, frac_y);
        CGContextScaleCTM(
            context,
            constrained.width / rect.size.width,
            constrained.height / rect.size.height,
        );
        CTFontDrawGlyphs(
            font.as_concrete_TypeRef(),
            &raw const glyph,
            &CGPoint {
                x: -rect.origin.x,
                y: -rect.origin.y,
            },
            1,
            context,
        );
        CGContextRelease(context);
    }
    Some(())
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CoreTextFallbackNames {
    pub(super) family: String,
    pub(super) postscript: String,
}

#[cfg(target_os = "macos")]
pub(super) fn fallback_names(
    base_family: &str,
    ch: char,
    physical_font_size: f32,
) -> Option<CoreTextFallbackNames> {
    let base_font = core_text::font::new_from_name(
        coretext_family_name(base_family).as_ref(),
        f64::from(physical_font_size.max(1.0)),
    )
    .ok()?;
    let string = CFString::new(&ch.to_string());
    let range = CFRange {
        location: 0,
        length: isize::try_from(ch.len_utf16()).ok()?,
    };
    // SAFETY: owned font and string handles live through creation; the range counts
    // UTF-16 units. The non-null Create result transfers ownership to CTFont.
    let fallback_font = unsafe {
        let raw = CTFontCreateForString(
            base_font.as_concrete_TypeRef(),
            string.as_concrete_TypeRef(),
            range,
        );
        if raw.is_null() {
            return None;
        }
        CTFont::wrap_under_create_rule(raw)
    };
    let postscript = fallback_font.postscript_name();
    if postscript == "LastResort" {
        return None;
    }
    Some(CoreTextFallbackNames {
        family: fallback_font.family_name(),
        postscript,
    })
}

#[cfg(target_os = "macos")]
fn pixel_buffer_len(width: u32, height: u32, channels: usize) -> Option<usize> {
    let len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(channels)?;
    isize::try_from(len).ok()?;
    Some(len)
}
