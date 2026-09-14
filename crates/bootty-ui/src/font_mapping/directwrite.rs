//! Native DirectWrite layouts retain exact faces; ordinary rasterization uses the host backend.
#![allow(
    unsafe_code,
    reason = "DirectWrite COM callbacks and retained font handles are confined to this native adapter."
)]

mod raster;
mod renderer;

use super::{FontMappings, postscript_name};
use anyhow::{Context as _, Result};
use gpui_kit::{
    Bounds, DevicePixels, Font, FontId, FontMetrics, FontRun, FontStyle, FontWeight, GlyphId, Hsla,
    LineLayout, Pixels, PlatformTextSystem, RenderGlyphParams, ShapedGlyph, ShapedRun, Size,
    TextRenderingMode, point, px, size,
};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use unicode_segmentation::UnicodeSegmentation;
use windows::{
    Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG,
        DWRITE_FONT_METRICS1, DWRITE_FONT_SIMULATIONS_NONE, DWRITE_FONT_STYLE_ITALIC,
        DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STYLE_OBLIQUE, DWRITE_GLYPH_METRICS,
        DWRITE_INFORMATIONAL_STRING_POSTSCRIPT_NAME, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS,
        DWRITE_TEXT_RANGE, DWriteCreateFactory, IDWriteFactory5, IDWriteFontCollection1,
        IDWriteFontFace3, IDWriteFontFile, IDWriteInMemoryFontFileLoader, IDWriteLocalizedStrings,
        IDWriteTextRenderer, IDWriteTypography,
    },
    core::{HSTRING, IUnknown, Interface},
};

pub(super) struct WindowsFontSystem {
    native: Arc<dyn PlatformTextSystem>,
    policy_font: FontId,
    mappings: FontMappings,
    state: Mutex<Fonts>,
}

struct Face {
    native: IDWriteFontFace3,
    family: HSTRING,
    collection: IDWriteFontCollection1,
    typography: IDWriteTypography,
    fallbacks: Vec<FontId>,
    delegate: Option<FontId>,
}

struct Fonts {
    factory: IDWriteFactory5,
    loader: IDWriteInMemoryFontFileLoader,
    files: HashMap<FontFileKey, IDWriteFontFile>,
    system_family: String,
    locale: HSTRING,
    catalog: fontdb::Database,
    faces: Vec<Face>,
    selections: HashMap<Font, FontId>,
    native_ids: HashMap<usize, FontId>,
    color: Option<raster::ColorRaster>,
}

#[derive(Eq, PartialEq, Hash)]
enum FontFileKey {
    File(std::path::PathBuf),
    Memory(usize),
}

impl Drop for Fonts {
    fn drop(&mut self) {
        self.color.take();
        self.faces.clear();
        self.files.clear();
        // No font references escape this engine; release them before unregistering the loader.
        let _ = unsafe { self.factory.UnregisterFontFileLoader(&self.loader) };
    }
}

impl WindowsFontSystem {
    pub(super) fn new(native: Arc<dyn PlatformTextSystem>, mappings: FontMappings) -> Result<Self> {
        // The existing Windows host already requires DirectWrite5; this uses the shared OS factory.
        let policy_font = native.font_id(&gpui_kit::font(".SystemUIFont"))?;
        let factory: IDWriteFactory5 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let loader = unsafe { factory.CreateInMemoryFontFileLoader()? };
        unsafe {
            factory.RegisterFontFileLoader(&loader)?;
        }
        Ok(Self {
            native,
            policy_font,
            mappings,
            state: Mutex::new(Fonts {
                factory,
                loader,
                files: HashMap::new(),
                system_family: system_ui_family(),
                locale: HSTRING::from(sys_locale::get_locale().unwrap_or_else(|| "en-US".into())),
                catalog: crate::font_database::system_font_database().clone(),
                faces: Vec::new(),
                selections: HashMap::new(),
                native_ids: HashMap::new(),
                color: None,
            }),
        })
    }
}

fn localized_name(names: &IDWriteLocalizedStrings) -> Result<String> {
    let count = unsafe { names.GetStringLength(0)? } as usize;
    let mut buffer = vec![0; count + 1];
    unsafe {
        names.GetString(0, &mut buffer)?;
    }
    Ok(String::from_utf16(&buffer[..count])?)
}

fn system_ui_family() -> String {
    use windows::Win32::{
        Graphics::Gdi::LOGFONTW,
        UI::WindowsAndMessaging::{
            SPI_GETICONTITLELOGFONT, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        },
    };
    let mut font = LOGFONTW::default();
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETICONTITLELOGFONT,
            std::mem::size_of::<LOGFONTW>() as u32,
            Some((&raw mut font).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if read.is_err() {
        return "Segoe UI".into();
    }
    String::from_utf16_lossy(&font.lfFaceName)
        .trim_matches('\0')
        .to_owned()
}

impl Fonts {
    fn select(&mut self, descriptor: &Font, platform: &dyn PlatformTextSystem) -> Result<FontId> {
        if let Some(id) = self.selections.get(descriptor) {
            return Ok(*id);
        }
        let id = if let Some(name) = postscript_name(&descriptor.family) {
            self.catalog
                .faces()
                .find(|face| face.post_script_name == name)
                .map(|face| face.id)
        } else {
            let family =
                gpui_kit::font_name_with_fallbacks(&descriptor.family, &self.system_family);
            self.catalog.query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                weight: fontdb::Weight(descriptor.weight.0.clamp(1.0, 1000.0) as u16),
                style: match descriptor.style {
                    FontStyle::Normal => fontdb::Style::Normal,
                    FontStyle::Italic => fontdb::Style::Italic,
                    FontStyle::Oblique => fontdb::Style::Oblique,
                },
                ..fontdb::Query::default()
            })
        }
        .context("Requested font face is unavailable")?;
        let face = self.catalog.face(id).context("Font face disappeared")?;
        let key = match &face.source {
            fontdb::Source::Binary(data) => {
                FontFileKey::Memory(Arc::as_ptr(data).cast::<()>().addr())
            }
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                FontFileKey::File(path.clone())
            }
        };
        let index = face.index;
        let file = if let Some(file) = self.files.get(&key) {
            file.clone()
        } else {
            let file = self
                .catalog
                .with_face_data(id, |data, _| -> Result<IDWriteFontFile> {
                    // With no owner object DirectWrite copies the data; our file cache shares
                    // that one native buffer across every face in the original collection.
                    Ok(unsafe {
                        self.loader.CreateInMemoryFontFileReference(
                            &self.factory,
                            data.as_ptr().cast(),
                            u32::try_from(data.len())?,
                            None::<&IUnknown>,
                        )?
                    })
                })
                .context("Font face has no data")??;
            self.files.insert(key, file.clone());
            file
        };
        let native = unsafe {
            self.factory
                .CreateFontFaceReference(&file, index, DWRITE_FONT_SIMULATIONS_NONE)?
                .CreateFontFace()?
        };
        let mut fallbacks = Vec::new();
        if let Some(list) = &descriptor.fallbacks {
            for family in list.fallback_list() {
                let fallback = Font {
                    family: family.clone().into(),
                    fallbacks: None,
                    ..descriptor.clone()
                };
                if let Ok(id) = self.select(&fallback, platform) {
                    fallbacks.push(id);
                }
            }
        }
        let typography = unsafe { self.factory.CreateTypography()? };
        for (tag, value) in descriptor.features.tag_value_list() {
            if let Ok(tag) = <[u8; 4]>::try_from(tag.as_bytes()) {
                unsafe {
                    typography.AddFontFeature(DWRITE_FONT_FEATURE {
                        nameTag: DWRITE_FONT_FEATURE_TAG(u32::from_le_bytes(tag)),
                        parameter: *value,
                    })?;
                }
            }
        }
        let result = self.insert(native, typography, fallbacks, platform)?;
        self.selections.insert(descriptor.clone(), result);
        Ok(result)
    }

    fn insert(
        &mut self,
        native: IDWriteFontFace3,
        typography: IDWriteTypography,
        fallbacks: Vec<FontId>,
        platform: &dyn PlatformTextSystem,
    ) -> Result<FontId> {
        let family = localized_name(&unsafe { native.GetFamilyNames()? })?;
        let weight = unsafe { native.GetWeight() }.0;
        let style = unsafe { native.GetStyle() };
        let descriptor = Font {
            family: family.clone().into(),
            weight: FontWeight(weight as f32),
            style: match style {
                DWRITE_FONT_STYLE_ITALIC => FontStyle::Italic,
                DWRITE_FONT_STYLE_OBLIQUE => FontStyle::Oblique,
                _ => FontStyle::Normal,
            },
            ..gpui_kit::font(family.clone())
        };
        let same_properties = self
            .catalog
            .faces()
            .filter(|face| {
                face.families.iter().any(|(name, _)| name == &family)
                    && i32::from(face.weight.0) == weight
                    && match face.style {
                        fontdb::Style::Normal => style == DWRITE_FONT_STYLE_NORMAL,
                        fontdb::Style::Italic => style == DWRITE_FONT_STYLE_ITALIC,
                        fontdb::Style::Oblique => style == DWRITE_FONT_STYLE_OBLIQUE,
                    }
            })
            .map(|face| face.post_script_name.as_str())
            .collect::<HashSet<_>>();
        // Delegate only where these native properties identify one concrete face.
        let mut names = None;
        let mut exists = windows::core::BOOL::default();
        unsafe {
            native.GetInformationalStrings(
                DWRITE_INFORMATIONAL_STRING_POSTSCRIPT_NAME,
                &mut names,
                &mut exists,
            )?;
        }
        let postscript = names.as_ref().map(localized_name).transpose()?;
        let delegate = if same_properties.len() == 1
            && postscript
                .as_deref()
                .is_some_and(|name| same_properties.contains(name))
        {
            platform.font_id(&descriptor).ok()
        } else {
            None
        };
        let builder = unsafe { self.factory.CreateFontSetBuilder()? };
        unsafe {
            builder.AddFontFaceReference2(&native.GetFontFaceReference()?)?;
        }
        let collection = unsafe {
            self.factory
                .CreateFontCollectionFromFontSet(&builder.CreateFontSet()?)?
        };
        let key = native.cast::<IUnknown>()?.as_raw().addr();
        let id = FontId(self.faces.len());
        self.faces.push(Face {
            native,
            family: HSTRING::from(family),
            collection,
            typography,
            fallbacks,
            delegate,
        });
        self.native_ids.insert(key, id);
        Ok(id)
    }

    fn covers(&self, id: FontId, text: &str) -> bool {
        let characters = text
            .chars()
            .filter(|&ch| {
                ch != '\u{200d}' && !matches!(ch as u32, 0xfe00..=0xfe0f | 0xe0100..=0xe01ef)
            })
            .map(u32::from)
            .collect::<Vec<_>>();
        let mut glyphs = vec![0; characters.len()];
        unsafe {
            self.faces[id.0].native.GetGlyphIndices(
                characters.as_ptr(),
                characters.len() as u32,
                glyphs.as_mut_ptr(),
            )
        }
        .is_ok()
            && glyphs.iter().all(|&id| id != 0)
    }

    fn layout(
        &mut self,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        platform: &dyn PlatformTextSystem,
    ) -> Result<LineLayout> {
        let Some(first) = runs.first() else {
            return Ok(LineLayout {
                font_size,
                len: text.len(),
                ..LineLayout::default()
            });
        };
        let first = &self.faces[first.font_id.0];
        let format = unsafe {
            self.factory.CreateTextFormat(
                &first.family,
                &first.collection,
                first.native.GetWeight(),
                first.native.GetStyle(),
                first.native.GetStretch(),
                font_size.into(),
                &self.locale,
            )?
        };
        let wide = text.encode_utf16().collect::<Vec<_>>();
        let layout = unsafe {
            self.factory
                .CreateTextLayout(&wide, &format, f32::INFINITY, f32::INFINITY)?
        };
        let mut byte_start = 0;
        let mut utf16_start = 0;
        let mut assigned = Vec::new();
        for (run_index, run) in runs.iter().enumerate() {
            let content = &text[byte_start..byte_start + run.len];
            let mut spans: Vec<(usize, FontId)> = Vec::new();
            if self.faces[run.font_id.0].fallbacks.is_empty() {
                spans.push((content.encode_utf16().count(), run.font_id));
            } else {
                for grapheme in content.graphemes(true) {
                    let id = if self.covers(run.font_id, grapheme) {
                        run.font_id
                    } else {
                        self.faces[run.font_id.0]
                            .fallbacks
                            .iter()
                            .copied()
                            .find(|&id| self.covers(id, grapheme))
                            .unwrap_or(run.font_id)
                    };
                    let length = grapheme.encode_utf16().count();
                    if let Some((previous_length, previous_id)) = spans.last_mut()
                        && *previous_id == id
                    {
                        *previous_length += length;
                    } else {
                        spans.push((length, id));
                    }
                }
            }
            for (length, id) in spans {
                let face = &self.faces[id.0];
                let length = u32::try_from(length)?;
                let range = DWRITE_TEXT_RANGE {
                    startPosition: utf16_start,
                    length,
                };
                unsafe {
                    layout.SetFontCollection(&face.collection, range)?;
                    layout.SetFontFamilyName(&face.family, range)?;
                    layout.SetFontWeight(face.native.GetWeight(), range)?;
                    layout.SetFontStyle(face.native.GetStyle(), range)?;
                    layout.SetFontStretch(face.native.GetStretch(), range)?;
                    layout.SetTypography(&face.typography, range)?;
                    layout.SetFontSize(
                        if run_index % 2 == 1 {
                            f32::from(font_size).next_up()
                        } else {
                            font_size.into()
                        },
                        range,
                    )?;
                }
                assigned.push((utf16_start..utf16_start + length, id));
                utf16_start += length;
            }
            byte_start += run.len;
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let renderer: IDWriteTextRenderer = renderer::LayoutRenderer(output.clone()).into();
        unsafe {
            layout.Draw(None, &renderer, 0.0, 0.0)?;
        }
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            layout.GetMetrics(&mut metrics)?;
        }
        let mut count = 0;
        let mut line_metrics = vec![DWRITE_LINE_METRICS::default(); wide.len().max(1)];
        unsafe {
            layout.GetLineMetrics(Some(&mut line_metrics), &mut count)?;
        }
        let baseline = line_metrics[0].baseline;
        let mut result = LineLayout {
            font_size,
            len: text.len(),
            width: px(metrics.widthIncludingTrailingWhitespace),
            ascent: px(baseline),
            descent: px(line_metrics[0].height - baseline),
            runs: Vec::new(),
        };
        let mut bytes = Vec::with_capacity(wide.len() + 1);
        for (offset, ch) in text.char_indices() {
            bytes.extend(std::iter::repeat_n(offset, ch.len_utf16()));
        }
        bytes.push(text.len());
        for run in output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            let key = run.face.cast::<IUnknown>()?.as_raw().addr();
            let configured = assigned
                .iter()
                .find(|(range, _)| range.contains(&run.text_start))
                .map(|(_, id)| *id);
            // DirectWrite can return a new interface for the same face. Keep the original
            // font ID when it matches, including that ID's features and explicit fallbacks.
            let configured = if let Some(id) = configured {
                let requested = unsafe { self.faces[id.0].native.GetFontFaceReference()? };
                let actual = unsafe { run.face.GetFontFaceReference()? };
                unsafe { requested.Equals(&actual).as_bool() }.then_some(id)
            } else {
                None
            };
            let id = if let Some(id) = configured {
                id
            } else if let Some(id) = self.native_ids.get(&key) {
                *id
            } else {
                let typography = unsafe { self.factory.CreateTypography()? };
                self.insert(run.face.clone(), typography, Vec::new(), platform)?
            };
            let color = unsafe { run.face.IsColorFont().as_bool() };
            let mut pen = run.x;
            let mut shaped = Vec::with_capacity(run.glyphs.len());
            for (index, glyph) in run.glyphs.iter().enumerate() {
                if run.rtl {
                    pen -= glyph.advance;
                }
                let direction = if run.rtl { -1.0 } else { 1.0 };
                shaped.push(ShapedGlyph {
                    id: GlyphId(u32::from(glyph.id)),
                    position: point(
                        px(pen + direction * glyph.offset.advanceOffset),
                        px(run.y - baseline - glyph.offset.ascenderOffset),
                    ),
                    index: bytes[run.indices[index]],
                    is_emoji: color,
                });
                if !run.rtl {
                    pen += glyph.advance;
                }
            }
            result.runs.push(ShapedRun {
                font_id: id,
                glyphs: shaped,
            });
        }
        Ok(result)
    }
}

impl PlatformTextSystem for WindowsFontSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.native.add_fonts(fonts.clone())?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for bytes in fonts {
            state.catalog.load_font_data(bytes.into_owned());
        }
        Ok(())
    }
    fn all_font_names(&self) -> Vec<String> {
        self.native.all_font_names()
    }
    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        let resolved = self
            .mappings
            .resolve(descriptor)
            .unwrap_or_else(|| descriptor.clone());
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .select(&resolved, self.native.as_ref())
    }
    fn font_metrics(&self, id: FontId) -> FontMetrics {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut metrics = DWRITE_FONT_METRICS1::default();
        unsafe {
            state.faces[id.0].native.GetMetrics(&mut metrics);
        }
        let base = metrics.Base;
        FontMetrics {
            units_per_em: u32::from(base.designUnitsPerEm),
            ascent: f32::from(base.ascent),
            descent: -f32::from(base.descent),
            line_gap: f32::from(base.lineGap),
            underline_position: f32::from(base.underlinePosition),
            underline_thickness: f32::from(base.underlineThickness),
            cap_height: f32::from(base.capHeight),
            x_height: f32::from(base.xHeight),
            bounding_box: Bounds::new(
                point(
                    f32::from(metrics.glyphBoxLeft),
                    f32::from(metrics.glyphBoxBottom),
                ),
                size(
                    f32::from(metrics.glyphBoxRight) - f32::from(metrics.glyphBoxLeft),
                    f32::from(metrics.glyphBoxTop) - f32::from(metrics.glyphBoxBottom),
                ),
            ),
        }
    }
    fn typographic_bounds(&self, id: FontId, glyph: GlyphId) -> Result<Bounds<f32>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let metric = glyph_metrics(&state.faces[id.0].native, glyph)?;
        Ok(Bounds::new(
            point(
                metric.leftSideBearing as f32,
                (i64::from(metric.verticalOriginY) + i64::from(metric.bottomSideBearing)
                    - i64::from(metric.advanceHeight)) as f32,
            ),
            size(
                (i64::from(metric.advanceWidth)
                    - i64::from(metric.leftSideBearing)
                    - i64::from(metric.rightSideBearing)) as f32,
                (i64::from(metric.advanceHeight)
                    - i64::from(metric.topSideBearing)
                    - i64::from(metric.bottomSideBearing)) as f32,
            ),
        ))
    }
    fn advance(&self, id: FontId, glyph: GlyphId) -> Result<Size<f32>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let metric = glyph_metrics(&state.faces[id.0].native, glyph)?;
        Ok(size(
            metric.advanceWidth as f32,
            metric.advanceHeight as f32,
        ))
    }
    fn glyph_for_char(&self, id: FontId, ch: char) -> Option<GlyphId> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let character = u32::from(ch);
        let mut glyph = 0;
        unsafe {
            state.faces[id.0]
                .native
                .GetGlyphIndices(&character, 1, &mut glyph)
        }
        .ok()?;
        (glyph != 0).then_some(GlyphId(u32::from(glyph)))
    }
    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(id) = state.faces[params.font_id.0].delegate {
            return self.native.glyph_raster_bounds(&RenderGlyphParams {
                font_id: id,
                ..params.clone()
            });
        }
        state.bounds(params)
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(id) = state.faces[params.font_id.0].delegate {
            return self.native.rasterize_glyph(
                &RenderGlyphParams {
                    font_id: id,
                    ..params.clone()
                },
                bounds,
            );
        }
        state.raster(params, bounds)
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .layout(text, font_size, runs, self.native.as_ref())
            .expect("DirectWrite failed to shape a font-resolved line")
    }
    fn recommended_rendering_mode(&self, _id: FontId, font_size: Pixels) -> TextRenderingMode {
        // Windows's recommendation is the system ClearType preference, independent of face.
        self.native
            .recommended_rendering_mode(self.policy_font, font_size)
    }
    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.native.glyph_dilation_for_color(color)
    }
}

fn glyph_metrics(face: &IDWriteFontFace3, glyph: GlyphId) -> Result<DWRITE_GLYPH_METRICS> {
    let glyph = u16::try_from(glyph.0)?;
    let mut metrics = DWRITE_GLYPH_METRICS::default();
    unsafe {
        face.GetDesignGlyphMetrics(&glyph, 1, &mut metrics, false)?;
    }
    Ok(metrics)
}
