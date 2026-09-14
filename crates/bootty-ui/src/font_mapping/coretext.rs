//! CoreText keeps the selected face attached to both shaped runs and glyph coverage.
#![allow(
    unsafe_code,
    reason = "CoreText attributes and native drawing use owned CF handles at this boundary."
)]

use super::{FontMappings, postscript_name};
use anyhow::{Context as _, Result};
use core_foundation::{
    array::{CFArray, CFArrayRef},
    attributed_string::CFMutableAttributedString,
    base::{CFHash, CFRange, CFType, TCFType},
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use core_graphics::{
    base::kCGImageAlphaPremultipliedLast,
    color_space::CGColorSpace,
    context::{CGContext, CGTextDrawingMode},
    geometry::{CGAffineTransform, CGPoint},
};
use core_text::{
    font::{CTFont, CTFontRef, cascade_list_for_languages},
    font_descriptor::{
        CTFontDescriptorRef, kCTFontCascadeListAttribute, kCTFontFeatureSettingsAttribute,
    },
    line::CTLine,
    string_attributes::kCTFontAttributeName,
};
use font_kit::{
    font::Font as NativeFont, handle::Handle, source::SystemSource, sources::mem::MemSource,
};
use gpui_kit::{
    Bounds, DevicePixels, Font, FontId, FontMetrics, FontRun, GlyphId, Hsla, LineLayout, Pixels,
    PlatformTextSystem, RenderGlyphParams, SUBPIXEL_VARIANTS_X, SUBPIXEL_VARIANTS_Y, ShapedGlyph,
    ShapedRun, Size, TextRenderingMode, point, px, size,
};
use num_traits::ToPrimitive as _;
use pathfinder_geometry::transform2d::Transform2F;
use std::{
    borrow::Cow,
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

pub(super) struct CoreTextSystem {
    native: Arc<dyn PlatformTextSystem>,
    mappings: FontMappings,
    state: Mutex<Fonts>,
}

#[derive(Clone)]
struct FaceKey(CTFont);
impl PartialEq for FaceKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_CFType() == other.0.as_CFType()
    }
}
impl Eq for FaceKey {}
impl Hash for FaceKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // SAFETY: the key retains the CF font for the duration of this operation.
        unsafe { CFHash(self.0.as_CFTypeRef()) }.hash(state);
    }
}

struct Fonts {
    memory: MemSource,
    system: SystemSource,
    selections: HashMap<Font, FontId>,
    ids: HashMap<FaceKey, FontId>,
    faces: Vec<Arc<NativeFont>>,
    exact: HashMap<fontdb::ID, NativeFont>,
}

impl CoreTextSystem {
    pub(super) fn new(native: Arc<dyn PlatformTextSystem>, mappings: FontMappings) -> Self {
        Self {
            native,
            mappings,
            state: Mutex::new(Fonts {
                memory: MemSource::empty(),
                system: SystemSource::new(),
                selections: HashMap::new(),
                ids: HashMap::new(),
                faces: Vec::new(),
                exact: HashMap::new(),
            }),
        }
    }

    fn face(&self, id: FontId) -> Option<Arc<NativeFont>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .faces
            .get(id.0)
            .cloned()
    }
}

impl Fonts {
    fn insert(&mut self, font: NativeFont) -> FontId {
        let key = FaceKey(font.native_font());
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = FontId(self.faces.len());
        self.faces.push(Arc::new(font));
        self.ids.insert(key, id);
        id
    }

    fn select(&mut self, descriptor: &Font) -> Result<NativeFont> {
        if let Some(name) = postscript_name(&descriptor.family) {
            let database = crate::font_database::system_font_database();
            let face = database
                .faces()
                .find(|face| face.post_script_name == name)
                .with_context(|| format!("Unknown font face {name}"))?;
            if let Some(font) = self.exact.get(&face.id) {
                return Ok(font.clone());
            }
            let font = database
                .with_face_data(face.id, |bytes, index| {
                    NativeFont::from_bytes(Arc::new(bytes.to_vec()), index)
                })
                .context("Font face has no data")??;
            self.exact.insert(face.id, font.clone());
            return Ok(font);
        }
        let family = gpui_kit::font_name_with_fallbacks(&descriptor.family, ".AppleSystemUIFont");
        let candidates = self
            .memory
            .select_family_by_name(family)
            .or_else(|_| self.system.select_family_by_name(family))?;
        let fonts = candidates
            .fonts()
            .iter()
            .map(Handle::load)
            .collect::<Result<Vec<_>, _>>()?;
        let properties = fonts.iter().map(NativeFont::properties).collect::<Vec<_>>();
        let requested = font_kit::properties::Properties {
            weight: font_kit::properties::Weight(descriptor.weight.0),
            style: match descriptor.style {
                gpui_kit::FontStyle::Normal => font_kit::properties::Style::Normal,
                gpui_kit::FontStyle::Italic => font_kit::properties::Style::Italic,
                gpui_kit::FontStyle::Oblique => font_kit::properties::Style::Oblique,
            },
            stretch: font_kit::properties::Stretch::default(),
        };
        let index = font_kit::matching::find_best_match(&properties, &requested)?;
        fonts
            .get(index)
            .cloned()
            .context("Font matching returned no available face")
    }
}

impl PlatformTextSystem for CoreTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let mut handles = Vec::new();
        for bytes in fonts {
            let bytes = Arc::new(bytes.into_owned());
            let count = match NativeFont::analyze_bytes(bytes.clone())? {
                font_kit::file_type::FileType::Single => 1,
                font_kit::file_type::FileType::Collection(count) => count,
            };
            handles.extend((0..count).map(|index| Handle::from_memory(bytes.clone(), index)));
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .memory
            .add_fonts(handles.into_iter())?;
        Ok(())
    }
    fn all_font_names(&self) -> Vec<String> {
        let mut names = self.native.all_font_names();
        names.extend(
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .memory
                .all_families()
                .unwrap_or_default(),
        );
        names.sort_unstable();
        names.dedup();
        names
    }
    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        let mut fonts = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(id) = fonts.selections.get(descriptor) {
            return Ok(*id);
        }
        let resolved = self
            .mappings
            .resolve(descriptor)
            .unwrap_or_else(|| descriptor.clone());
        let font = fonts.select(&resolved)?;
        let font = configured_font(&font, &resolved, &mut fonts)?;
        let id = fonts.insert(font);
        fonts.selections.insert(descriptor.clone(), id);
        drop(fonts);
        Ok(id)
    }
    fn font_metrics(&self, id: FontId) -> FontMetrics {
        let Some(font) = self.face(id) else {
            // GPUI's metric API is infallible. Unknown IDs describe no glyphs, with a nonzero em
            // denominator so its downstream metric scaling stays finite.
            return FontMetrics {
                units_per_em: 1,
                ascent: 0.0,
                descent: 0.0,
                line_gap: 0.0,
                underline_position: 0.0,
                underline_thickness: 0.0,
                cap_height: 0.0,
                x_height: 0.0,
                bounding_box: Bounds::new(point(0.0, 0.0), size(0.0, 0.0)),
            };
        };
        let metrics = font.metrics();
        FontMetrics {
            units_per_em: metrics.units_per_em,
            ascent: metrics.ascent,
            descent: metrics.descent,
            line_gap: metrics.line_gap,
            underline_position: metrics.underline_position,
            underline_thickness: metrics.underline_thickness,
            cap_height: metrics.cap_height,
            x_height: metrics.x_height,
            bounding_box: Bounds::new(
                point(
                    metrics.bounding_box.origin_x(),
                    metrics.bounding_box.origin_y(),
                ),
                size(metrics.bounding_box.width(), metrics.bounding_box.height()),
            ),
        }
    }
    fn typographic_bounds(&self, id: FontId, glyph: GlyphId) -> Result<Bounds<f32>> {
        let rect = self
            .face(id)
            .context("Unknown font ID")?
            .typographic_bounds(glyph.0)?;
        Ok(Bounds::new(
            point(rect.origin_x(), rect.origin_y()),
            size(rect.width(), rect.height()),
        ))
    }
    fn advance(&self, id: FontId, glyph: GlyphId) -> Result<Size<f32>> {
        let advance = self.face(id).context("Unknown font ID")?.advance(glyph.0)?;
        Ok(size(advance.x(), advance.y()))
    }
    fn glyph_for_char(&self, id: FontId, ch: char) -> Option<GlyphId> {
        self.face(id)?.glyph_for_char(ch).map(GlyphId)
    }
    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let rect = self
            .face(params.font_id)
            .context("Unknown font ID")?
            .raster_bounds(
                params.glyph_id.0,
                params.font_size.into(),
                Transform2F::from_scale(params.scale_factor),
                font_kit::hinting::HintingOptions::None,
                font_kit::canvas::RasterizationOptions::GrayscaleAa,
            )?;
        Ok(Bounds::new(
            point(
                DevicePixels(
                    rect.origin_x()
                        .checked_sub(1)
                        .context("Glyph x origin overflow")?,
                ),
                DevicePixels(
                    rect.origin_y()
                        .checked_sub(1)
                        .context("Glyph y origin overflow")?,
                ),
            ),
            size(
                DevicePixels(
                    rect.width()
                        .checked_add(2)
                        .context("Glyph width overflow")?,
                ),
                DevicePixels(
                    rect.height()
                        .checked_add(2)
                        .context("Glyph height overflow")?,
                ),
            ),
        ))
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        draw_glyph(
            &self
                .face(params.font_id)
                .context("Unknown font ID")?
                .native_font(),
            params,
            bounds,
        )
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let mut fonts = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let layout = layout_font_line(&mut fonts, text, font_size, runs);
        drop(fonts);
        // Invalid font IDs or UTF-8 run boundaries cannot describe a shaped line.
        layout.unwrap_or_else(|| LineLayout {
            runs: Vec::new(),
            font_size,
            width: px(0.0),
            ascent: px(0.0),
            descent: px(0.0),
            len: text.len(),
        })
    }
    fn recommended_rendering_mode(&self, _: FontId, _: Pixels) -> TextRenderingMode {
        TextRenderingMode::Grayscale
    }
    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.native.glyph_dilation_for_color(color)
    }
}

fn attributed_line(
    fonts: &Fonts,
    text: &str,
    font_size: Pixels,
    runs: &[FontRun],
) -> Option<(CTLine, f32, f32)> {
    let mut attributed = CFMutableAttributedString::new();
    attributed.replace_str(&CFString::new(text), CFRange::init(0, 0));
    let mut byte_start = 0_usize;
    let mut utf16_start = 0_usize;
    let mut ascent = 0.0_f32;
    let mut descent = 0.0_f32;
    for (index, run) in runs.iter().enumerate() {
        let byte_end = byte_start.checked_add(run.len)?;
        let text_run = text.get(byte_start..byte_end)?;
        let count = text_run.encode_utf16().count();
        let font = fonts.faces.get(run.font_id.0)?;
        let metrics = font.metrics();
        let scale =
            f32::from(font_size) / metrics.units_per_em.to_f32().filter(|units| *units > 0.0)?;
        ascent = ascent.max(metrics.ascent * scale);
        descent = descent.max(-metrics.descent * scale);
        // Distinct adjacent font attributes prevent ligatures from crossing style runs.
        // The neighboring f32 value is below the native rasterizer's pixel precision.
        let run_size = if index % 2 == 0 {
            f32::from(font_size).next_up()
        } else {
            font_size.into()
        };
        let native = font.native_font().clone_with_font_size(f64::from(run_size));
        unsafe {
            attributed.set_attribute(
                CFRange::init(
                    isize::try_from(utf16_start).ok()?,
                    isize::try_from(count).ok()?,
                ),
                kCTFontAttributeName,
                &native,
            );
        }
        byte_start = byte_end;
        utf16_start = utf16_start.checked_add(count)?;
    }
    let line = CTLine::new_with_attributed_string(attributed.as_concrete_TypeRef());
    Some((line, ascent, descent))
}

fn layout_font_line(
    fonts: &mut Fonts,
    text: &str,
    font_size: Pixels,
    runs: &[FontRun],
) -> Option<LineLayout> {
    let (line, ascent, descent) = attributed_line(fonts, text, font_size, runs)?;
    // A direct lookup also handles RTL runs, where CoreText's indices run backwards.
    let mut byte_indices = Vec::with_capacity(text.encode_utf16().count().saturating_add(1));
    for (index, ch) in text.char_indices() {
        byte_indices.extend(std::iter::repeat_n(index, ch.len_utf16()));
    }
    byte_indices.push(text.len());
    let mut shaped = Vec::new();
    for run in line.glyph_runs().iter() {
        let Some(run_attributes) = run.attributes() else {
            continue;
        };
        let Some(native) = (unsafe {
            run_attributes
                .get(kCTFontAttributeName)
                .downcast::<CTFont>()
        }) else {
            continue;
        };
        let emoji = native.postscript_name().contains("AppleColorEmoji");
        let native_font = unsafe { NativeFont::from_native_font(&native) };
        let font_id = fonts.insert(native_font);
        let glyphs = run
            .glyphs()
            .iter()
            .zip(run.positions().iter())
            .zip(run.string_indices().iter())
            .filter_map(|((&id, position), &index)| {
                Some(ShapedGlyph {
                    id: GlyphId(u32::from(id)),
                    position: point(px(position.x.to_f32()?), px(position.y.to_f32()?)),
                    index: *byte_indices.get(usize::try_from(index).ok()?)?,
                    is_emoji: emoji,
                })
            })
            .collect();
        shaped.push(ShapedRun { font_id, glyphs });
    }
    Some(LineLayout {
        runs: shaped,
        font_size,
        width: px(line.get_typographic_bounds().width.to_f32()?),
        ascent: px(ascent),
        descent: px(descent),
        len: text.len(),
    })
}

fn configured_font(font: &NativeFont, descriptor: &Font, fonts: &mut Fonts) -> Result<NativeFont> {
    let mut attributes = Vec::<(CFString, CFType)>::new();
    let features = descriptor
        .features
        .tag_value_list()
        .iter()
        .map(|(tag, value)| unsafe {
            CFDictionary::from_CFType_pairs(&[
                (
                    CFString::wrap_under_get_rule(kCTFontOpenTypeFeatureTag),
                    CFString::new(tag).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kCTFontOpenTypeFeatureValue),
                    CFNumber::from(i64::from(*value)).as_CFType(),
                ),
            ])
        })
        .collect::<Vec<_>>();
    unsafe {
        attributes.push((
            CFString::wrap_under_get_rule(kCTFontFeatureSettingsAttribute),
            CFArray::from_CFTypes(&features).as_CFType(),
        ));
    }
    if let Some(fallbacks) = &descriptor.fallbacks {
        let mut cascade = fallbacks
            .fallback_list()
            .iter()
            .filter_map(|family| {
                let fallback = Font {
                    family: family.clone().into(),
                    fallbacks: None,
                    ..descriptor.clone()
                };
                // Resolve against our memory source too: bundled families need not be installed.
                fonts
                    .select(&fallback)
                    .ok()
                    .map(|font| font.native_font().copy_descriptor())
            })
            .collect::<Vec<_>>();
        let languages = unsafe {
            CFArray::<CFString>::wrap_under_create_rule(CFLocaleCopyPreferredLanguages())
        };
        cascade.extend(
            cascade_list_for_languages(&font.native_font(), &languages)
                .iter()
                .map(|entry| (*entry).clone()),
        );
        unsafe {
            attributes.push((
                CFString::wrap_under_get_rule(kCTFontCascadeListAttribute),
                CFArray::from_CFTypes(&cascade).as_CFType(),
            ));
        }
    }
    let descriptor = core_text::font_descriptor::new_from_attributes(
        &CFDictionary::from_CFType_pairs(&attributes),
    );
    let native = font.native_font();
    // SAFETY: all input CF handles live across this call; the returned Create handle is owned.
    unsafe {
        let raw = CTFontCreateCopyWithAttributes(
            native.as_concrete_TypeRef(),
            0.0,
            std::ptr::null(),
            descriptor.as_concrete_TypeRef(),
        );
        anyhow::ensure!(
            !raw.is_null(),
            "CoreText could not configure the selected face"
        );
        Ok(NativeFont::from_native_font(
            &CTFont::wrap_under_create_rule(raw),
        ))
    }
}

fn draw_glyph(
    font: &CTFont,
    params: &RenderGlyphParams,
    bounds: Bounds<DevicePixels>,
) -> Result<(Size<DevicePixels>, Vec<u8>)> {
    let width = usize::try_from(bounds.size.width.0)?
        .checked_add(usize::from(params.subpixel_variant.x != 0))
        .context("Glyph width overflow")?;
    let height = usize::try_from(bounds.size.height.0)?
        .checked_add(usize::from(params.subpixel_variant.y != 0))
        .context("Glyph height overflow")?;
    anyhow::ensure!(width != 0 && height != 0, "Empty glyph bounds");
    let output_size = size(
        DevicePixels(i32::try_from(width)?),
        DevicePixels(i32::try_from(height)?),
    );
    let channels = if params.is_emoji { 4 } else { 1 };
    let stride = width
        .checked_mul(channels)
        .context("Glyph stride overflow")?;
    let mut pixels = vec![
        0;
        stride
            .checked_mul(height)
            .context("Glyph bitmap overflow")?
    ];
    let colors = if params.is_emoji {
        CGColorSpace::create_device_rgb()
    } else {
        CGColorSpace::create_device_gray()
    };
    let context = CGContext::create_bitmap_context(
        Some(pixels.as_mut_ptr().cast()),
        width,
        height,
        8,
        stride,
        &colors,
        if params.is_emoji {
            kCGImageAlphaPremultipliedLast
        } else {
            7
        },
    );
    context.translate(
        -f64::from(bounds.origin.x.0),
        f64::from(bounds.origin.y.0) + f64::from(bounds.size.height.0),
    );
    context.scale(
        f64::from(params.scale_factor),
        f64::from(params.scale_factor),
    );
    context.set_text_drawing_mode(CGTextDrawingMode::CGTextFill);
    context.set_allows_antialiasing(true);
    context.set_should_antialias(true);
    context.set_allows_font_subpixel_positioning(true);
    context.set_should_subpixel_position_fonts(true);
    context.set_allows_font_subpixel_quantization(false);
    context.set_should_subpixel_quantize_fonts(false);
    context.set_should_smooth_fonts(params.dilation > 0);
    context.set_gray_fill_color(f64::from(params.dilation) * 0.25, 1.0);
    font.clone_with_font_size(f64::from(f32::from(params.font_size)))
        .draw_glyphs(
            &[u16::try_from(params.glyph_id.0)?],
            &[CGPoint::new(
                f64::from(params.subpixel_variant.x)
                    / f64::from(SUBPIXEL_VARIANTS_X)
                    / f64::from(params.scale_factor),
                f64::from(params.subpixel_variant.y)
                    / f64::from(SUBPIXEL_VARIANTS_Y)
                    / f64::from(params.scale_factor),
            )],
            context,
        );
    if params.is_emoji {
        for pixel in pixels.as_chunks_mut::<4>().0 {
            gpui_kit::swap_rgba_pa_to_bgra(pixel);
        }
    }
    Ok((output_size, pixels))
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontOpenTypeFeatureTag: CFStringRef;
    static kCTFontOpenTypeFeatureValue: CFStringRef;
    fn CTFontCreateCopyWithAttributes(
        font: CTFontRef,
        size: f64,
        matrix: *const CGAffineTransform,
        attributes: CTFontDescriptorRef,
    ) -> CTFontRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFLocaleCopyPreferredLanguages() -> CFArrayRef;
}
