//! Exact face aliases live in our font catalog; original font sources are shared unchanged.
use super::{FontMappings, postscript_name};
use anyhow::{Context as _, Result};
use cosmic_text::{Attrs, AttrsList, Family, FontSystem, ShapeLine, Shaping, fontdb};
use gpui_kit::{
    Bounds, DevicePixels, Font, FontFeatures, FontId, FontMetrics, FontRun, GlyphId, LineLayout,
    Pixels, PlatformTextSystem, RenderGlyphParams, SUBPIXEL_VARIANTS_X, SUBPIXEL_VARIANTS_Y,
    ShapedGlyph, ShapedRun, Size, TextRenderingMode, point, px, size,
};
use num_traits::ToPrimitive as _;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ops::{Add as _, Range},
    sync::{Arc, Mutex},
};
use swash::scale::{
    Render, ScaleContext, Source, StrikeWith,
    image::{Content, Image},
};
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct CosmicFontSystem {
    mappings: FontMappings,
    state: Mutex<Fonts>,
}

struct Face {
    font: Arc<cosmic_text::Font>,
    family: String,
    weight: fontdb::Weight,
    style: fontdb::Style,
    stretch: fontdb::Stretch,
    features: FontFeatures,
    fallbacks: Vec<FontId>,
    color: bool,
}

struct Fonts {
    system: FontSystem,
    faces: Vec<Face>,
    selections: HashMap<Font, FontId>,
    aliases: HashMap<String, fontdb::ID>,
    fallback_ids: HashMap<fontdb::ID, FontId>,
    scaler: ScaleContext,
    images: HashMap<RenderGlyphParams, Image>,
}

impl CosmicFontSystem {
    pub(super) fn new(_native: Arc<dyn PlatformTextSystem>, mappings: FontMappings) -> Self {
        let mut database = fontdb::Database::new();
        let mut files = HashSet::new();
        let mut buffers = HashSet::new();
        for face in crate::font_database::system_font_database().faces() {
            let source = match &face.source {
                ::fontdb::Source::Binary(data) => {
                    if !buffers.insert(Arc::as_ptr(data).cast::<()>()) {
                        continue;
                    }
                    fontdb::Source::Binary(data.clone())
                }
                ::fontdb::Source::File(path) => {
                    if !files.insert(path.clone()) {
                        continue;
                    }
                    fontdb::Source::File(path.clone())
                }
                ::fontdb::Source::SharedFile(path, data) => {
                    if !files.insert(path.clone()) {
                        continue;
                    }
                    fontdb::Source::SharedFile(path.clone(), data.clone())
                }
            };
            database.load_font_source(source);
        }
        database.set_sans_serif_family("IBM Plex Sans");
        database.set_monospace_family("Lilex");
        Self {
            mappings,
            state: Mutex::new(Fonts {
                system: FontSystem::new_with_locale_and_db(
                    sys_locale::get_locale().unwrap_or_else(|| "en-US".into()),
                    database,
                ),
                faces: Vec::new(),
                selections: HashMap::new(),
                aliases: HashMap::new(),
                fallback_ids: HashMap::new(),
                scaler: ScaleContext::new(),
                images: HashMap::new(),
            }),
        }
    }
}

impl Fonts {
    fn select(&mut self, descriptor: &Font) -> Result<FontId> {
        if let Some(id) = self.selections.get(descriptor) {
            return Ok(*id);
        }
        let id = if let Some(name) = postscript_name(&descriptor.family) {
            if let Some(id) = self.aliases.get(name) {
                *id
            } else {
                let mut face = self
                    .system
                    .db()
                    .faces()
                    .find(|face| face.post_script_name == name)
                    .cloned()
                    .with_context(|| format!("Unknown font face {name}"))?;
                let language = face
                    .families
                    .first()
                    .map_or(fontdb::Language::English_UnitedStates, |(_, lang)| *lang);
                face.families = vec![(descriptor.family.to_string(), language)];
                let id = self.system.db_mut().push_face_info(face);
                self.aliases.insert(name.to_owned(), id);
                id
            }
        } else {
            let family = gpui_kit::font_name_with_fallbacks(&descriptor.family, "IBM Plex Sans");
            self.system
                .db()
                .query(&fontdb::Query {
                    families: &[fontdb::Family::Name(family)],
                    weight: fontdb::Weight(
                        descriptor
                            .weight
                            .0
                            .clamp(1.0, 1000.0)
                            .to_u16()
                            .context("Font weight is outside the supported range")?,
                    ),
                    style: match descriptor.style {
                        gpui_kit::FontStyle::Normal => fontdb::Style::Normal,
                        gpui_kit::FontStyle::Italic => fontdb::Style::Italic,
                        gpui_kit::FontStyle::Oblique => fontdb::Style::Oblique,
                    },
                    ..fontdb::Query::default()
                })
                .with_context(|| format!("Unknown font family {family}"))?
        };
        let mut fallbacks = Vec::new();
        if let Some(names) = &descriptor.fallbacks {
            for name in names.fallback_list() {
                let fallback = Font {
                    family: name.clone().into(),
                    fallbacks: None,
                    ..descriptor.clone()
                };
                if let Ok(id) = self.select(&fallback) {
                    fallbacks.push(id);
                }
            }
        }
        let result = self.insert(id, descriptor.features.clone(), fallbacks)?;
        self.selections.insert(descriptor.clone(), result);
        Ok(result)
    }

    fn insert(
        &mut self,
        id: fontdb::ID,
        features: FontFeatures,
        fallbacks: Vec<FontId>,
    ) -> Result<FontId> {
        let info = self
            .system
            .db()
            .face(id)
            .context("Font face disappeared")?
            .clone();
        let font = self
            .system
            .get_font(id, info.weight)
            .context("Cannot load font face")?;
        let parsed = rustybuzz::ttf_parser::Face::parse(font.data(), info.index)?;
        let tables = parsed.tables();
        let color = tables.colr.is_some()
            || tables.cbdt.is_some()
            || tables.sbix.is_some()
            || tables.svg.is_some();
        let id = FontId(self.faces.len());
        self.faces.push(Face {
            font,
            family: info
                .families
                .first()
                .context("Font face has no family")?
                .0
                .clone(),
            weight: info.weight,
            style: info.style,
            stretch: info.stretch,
            features,
            fallbacks,
            color,
        });
        Ok(id)
    }

    fn attributes(&self, id: FontId) -> Attrs<'_> {
        let Some(face) = self.faces.get(id.0) else {
            return Attrs::new().metadata(id.0);
        };
        let mut features = cosmic_text::FontFeatures::default();
        for (tag, value) in face.features.tag_value_list() {
            if let Ok(tag) = <&[u8; 4]>::try_from(tag.as_bytes()) {
                features.set(cosmic_text::FeatureTag::new(tag), *value);
            }
        }
        Attrs::new()
            .family(Family::Name(&face.family))
            .weight(face.weight)
            .style(face.style)
            .stretch(face.stretch)
            .font_features(features)
            .metadata(id.0)
    }

    fn covers(&self, id: FontId, grapheme: &str) -> bool {
        let Some(face) = self.faces.get(id.0) else {
            return false;
        };
        let map = face.font.as_swash().charmap();
        grapheme
            .chars()
            .filter(|&ch| {
                ch != '\u{200d}' && !matches!(u32::from(ch), 0xfe00..=0xfe0f | 0xe0100..=0xe01ef)
            })
            .all(|ch| map.map(ch) != 0)
    }

    fn paragraph(
        &mut self,
        text: &str,
        font_size: Pixels,
        inputs: &[(Range<usize>, FontId)],
    ) -> LineLayout {
        let default = inputs.first().map_or(FontId(0), |(_, id)| *id);
        let mut attributes = AttrsList::new(&self.attributes(default));
        for (range, primary) in inputs {
            let fallbacks = self
                .faces
                .get(primary.0)
                .map_or(&[][..], |face| face.fallbacks.as_slice());
            if fallbacks.is_empty() {
                attributes.add_span(range.clone(), &self.attributes(*primary));
                continue;
            }
            let mut span_start = range.start;
            let mut span_font = *primary;
            let Some(range_text) = text.get(range.clone()) else {
                continue;
            };
            for (offset, grapheme) in range_text.grapheme_indices(true) {
                let selected = if self.covers(*primary, grapheme) {
                    *primary
                } else {
                    fallbacks
                        .iter()
                        .copied()
                        .find(|&id| self.covers(id, grapheme))
                        .unwrap_or(*primary)
                };
                let Some(start) = range.start.checked_add(offset) else {
                    continue;
                };
                if selected != span_font {
                    if start > span_start {
                        attributes.add_span(span_start..start, &self.attributes(span_font));
                    }
                    span_start = start;
                    span_font = selected;
                }
            }
            attributes.add_span(span_start..range.end, &self.attributes(span_font));
        }
        let shape = ShapeLine::new(&mut self.system, text, &attributes, Shaping::Advanced, 8);
        let layouts = shape.layout(
            font_size.into(),
            None,
            cosmic_text::Wrap::None,
            None,
            None,
            cosmic_text::Hinting::Disabled,
        );
        let mut result = LineLayout {
            font_size,
            len: text.len(),
            ..LineLayout::default()
        };
        for layout in layouts {
            result.ascent = result.ascent.max(px(layout.max_ascent));
            result.descent = result.descent.max(px(layout.max_descent));
            for glyph in layout.glyphs {
                let requested = FontId(glyph.metadata);
                let id = if self
                    .faces
                    .get(requested.0)
                    .is_some_and(|face| face.font.id() == glyph.font_id)
                {
                    requested
                } else if let Some(id) = self.fallback_ids.get(&glyph.font_id) {
                    *id
                } else {
                    let Ok(id) = self.insert(glyph.font_id, FontFeatures::default(), Vec::new())
                    else {
                        continue;
                    };
                    self.fallback_ids.insert(glyph.font_id, id);
                    id
                };
                let shaped = ShapedGlyph {
                    id: GlyphId(u32::from(glyph.glyph_id)),
                    position: point(
                        result
                            .width
                            .add(px(glyph.x_offset.mul_add(glyph.font_size, glyph.x))),
                        px(glyph.y_offset.mul_add(glyph.font_size, glyph.y)),
                    ),
                    index: glyph.start,
                    is_emoji: self.faces.get(id.0).is_some_and(|face| face.color),
                };
                if let Some(last) = result.runs.last_mut().filter(|run| run.font_id == id) {
                    last.glyphs.push(shaped);
                } else {
                    result.runs.push(ShapedRun {
                        font_id: id,
                        glyphs: vec![shaped],
                    });
                }
            }
            result.width = result.width.add(px(layout.w));
        }
        result
    }

    fn render(&mut self, params: &RenderGlyphParams) -> Result<Image> {
        let font = self
            .faces
            .get(params.font_id.0)
            .map(|face| face.font.clone())
            .context("Font ID is not registered")?;
        let mut scaler = self
            .scaler
            .builder(font.as_swash())
            .size(f32::from(params.font_size) * params.scale_factor)
            .hint(true)
            .build();
        let sources: &[Source] = if params.is_emoji {
            &[
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ]
        } else {
            &[Source::Bitmap(StrikeWith::ExactSize), Source::Outline]
        };
        Render::new(sources)
            .format(if params.subpixel_rendering {
                swash::zeno::Format::subpixel_bgra()
            } else {
                swash::zeno::Format::Alpha
            })
            .offset(swash::zeno::Vector::new(
                f32::from(params.subpixel_variant.x)
                    / f32::from(SUBPIXEL_VARIANTS_X)
                    / params.scale_factor,
                f32::from(params.subpixel_variant.y)
                    / f32::from(SUBPIXEL_VARIANTS_Y)
                    / params.scale_factor,
            ))
            .render(&mut scaler, u16::try_from(params.glyph_id.0)?)
            .context("Cannot rasterize glyph")
    }
}

impl PlatformTextSystem for CosmicFontSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for bytes in fonts {
            state.system.db_mut().load_font_data(bytes.into_owned());
        }
        drop(state);
        Ok(())
    }
    fn all_font_names(&self) -> Vec<String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut names = state
            .system
            .db()
            .faces()
            .flat_map(|face| face.families.iter())
            .filter(|(name, _)| postscript_name(name).is_none())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        drop(state);
        names
    }
    fn font_id(&self, descriptor: &Font) -> Result<FontId> {
        let resolved = self
            .mappings
            .resolve(descriptor)
            .unwrap_or_else(|| descriptor.clone());
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .select(&resolved)
    }
    fn font_metrics(&self, id: FontId) -> FontMetrics {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(font) = state.faces.get(id.0).map(|face| face.font.clone()) else {
            drop(state);
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
        drop(state);
        let m = font.as_swash().metrics(&[]);
        FontMetrics {
            units_per_em: u32::from(m.units_per_em),
            ascent: m.ascent,
            descent: -m.descent,
            line_gap: m.leading,
            underline_position: m.underline_offset,
            underline_thickness: m.stroke_size,
            cap_height: m.cap_height,
            x_height: m.x_height,
            bounding_box: Bounds::new(point(0.0, 0.0), size(m.max_width, m.ascent + m.descent)),
        }
    }
    fn typographic_bounds(&self, id: FontId, glyph: GlyphId) -> Result<Bounds<f32>> {
        Ok(Bounds::new(point(0.0, 0.0), self.advance(id, glyph)?))
    }
    fn advance(&self, id: FontId, glyph: GlyphId) -> Result<Size<f32>> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(font) = state.faces.get(id.0).map(|face| face.font.clone()) else {
            drop(state);
            return Err(anyhow::anyhow!("Font ID is not registered"));
        };
        drop(state);
        let metrics = font.as_swash().glyph_metrics(&[]);
        let glyph = u16::try_from(glyph.0)?;
        Ok(size(
            metrics.advance_width(glyph),
            metrics.advance_height(glyph),
        ))
    }
    fn glyph_for_char(&self, id: FontId, ch: char) -> Option<GlyphId> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(font) = state.faces.get(id.0).map(|face| face.font.clone()) else {
            drop(state);
            return None;
        };
        drop(state);
        let glyph = font.as_swash().charmap().map(ch);
        (glyph != 0).then_some(GlyphId(u32::from(glyph)))
    }
    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let image = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .render(params)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bounds = Bounds::new(
            point(
                DevicePixels(image.placement.left),
                DevicePixels(
                    image
                        .placement
                        .top
                        .checked_neg()
                        .context("Glyph vertical placement exceeds pixel bounds")?,
                ),
            ),
            size(
                DevicePixels(i32::try_from(image.placement.width)?),
                DevicePixels(i32::try_from(image.placement.height)?),
            ),
        );
        state.images.insert(params.clone(), image);
        drop(state);
        Ok(bounds)
    }
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        let cached = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .images
            .remove(params);
        let mut image = match cached {
            Some(image) => image,
            None => self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .render(params)?,
        };
        if image.content == Content::Mask {
            if params.subpixel_rendering || params.is_emoji {
                image.data = image.data.into_iter().flat_map(|a| [a, a, a, a]).collect();
            }
        } else {
            for pixel in image.data.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
        }
        Ok((bounds.size, image.data))
    }
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let mut result = LineLayout {
            font_size,
            len: text.len(),
            ..LineLayout::default()
        };
        if runs.is_empty() {
            return result;
        }
        let mut offset = 0_usize;
        let mut inputs = Vec::with_capacity(runs.len());
        for run in runs {
            let start = offset;
            let Some(end) = offset.checked_add(run.len) else {
                return result;
            };
            if text.get(start..end).is_none() {
                return result;
            }
            inputs.push((start..end, run.font_id));
            offset = end;
        }
        let face_count = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .faces
            .len();
        if inputs.iter().any(|(_, id)| id.0 >= face_count) {
            return result;
        }
        let mut start = 0;
        let boundaries = text
            .char_indices()
            .filter(|(_, ch)| matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
            .filter_map(|(index, ch)| index.checked_add(ch.len_utf8()).map(|next| (index, next)))
            .chain(std::iter::once((text.len(), text.len())));
        for (end, next) in boundaries {
            if end > start {
                let selected = inputs
                    .iter()
                    .filter_map(|(range, id)| {
                        let a = range.start.max(start);
                        let b = range.end.min(end);
                        if a >= b {
                            return None;
                        }
                        Some((a.checked_sub(start)?..b.checked_sub(start)?, *id))
                    })
                    .collect::<Vec<_>>();
                let Some(line_text) = text.get(start..end) else {
                    start = next;
                    continue;
                };
                let mut line = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .paragraph(line_text, font_size, &selected);
                for run in &mut line.runs {
                    for glyph in &mut run.glyphs {
                        let Some(index) = glyph.index.checked_add(start) else {
                            continue;
                        };
                        glyph.index = index;
                        glyph.position.x = glyph.position.x.add(result.width);
                    }
                }
                result.runs.extend(line.runs);
                result.width = result.width.add(line.width);
                result.ascent = result.ascent.max(line.ascent);
                result.descent = result.descent.max(line.descent);
            }
            start = next;
        }
        result
    }
    fn recommended_rendering_mode(&self, _: FontId, _: Pixels) -> TextRenderingMode {
        TextRenderingMode::Subpixel
    }
}
