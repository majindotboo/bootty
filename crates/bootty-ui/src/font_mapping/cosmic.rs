//! Exact face aliases live in our font catalog; original font sources are shared unchanged.
use super::{FontMappings, postscript_name};
use anyhow::{Context as _, Result};
use cosmic_text::{Attrs, AttrsList, Family, FontSystem, ShapeLine, Shaping, fontdb};
use gpui_kit::{
    Bounds, DevicePixels, Font, FontFeatures, FontId, FontMetrics, FontRun, GlyphId, LineLayout,
    Pixels, PlatformTextSystem, RenderGlyphParams, SUBPIXEL_VARIANTS_X, SUBPIXEL_VARIANTS_Y,
    ShapedGlyph, ShapedRun, Size, TextRenderingMode, point, px, size,
};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ops::Range,
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
                    weight: fontdb::Weight(descriptor.weight.0.clamp(1.0, 1000.0) as u16),
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
            family: info.families[0].0.clone(),
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
        let face = &self.faces[id.0];
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
        let map = self.faces[id.0].font.as_swash().charmap();
        grapheme
            .chars()
            .filter(|&ch| {
                ch != '\u{200d}' && !matches!(ch as u32, 0xfe00..=0xfe0f | 0xe0100..=0xe01ef)
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
            if self.faces[primary.0].fallbacks.is_empty() {
                attributes.add_span(range.clone(), &self.attributes(*primary));
                continue;
            }
            let mut span_start = range.start;
            let mut span_font = *primary;
            for (offset, grapheme) in text[range.clone()].grapheme_indices(true) {
                let selected = if self.covers(*primary, grapheme) {
                    *primary
                } else {
                    self.faces[primary.0]
                        .fallbacks
                        .iter()
                        .copied()
                        .find(|&id| self.covers(id, grapheme))
                        .unwrap_or(*primary)
                };
                let start = range.start + offset;
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
                let id = if self.faces[requested.0].font.id() == glyph.font_id {
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
                        result.width + px(glyph.x + glyph.x_offset * glyph.font_size),
                        px(glyph.y + glyph.y_offset * glyph.font_size),
                    ),
                    index: glyph.start,
                    is_emoji: self.faces[id.0].color,
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
            result.width += px(layout.w);
        }
        result
    }

    fn render(&mut self, params: &RenderGlyphParams) -> Result<Image> {
        let font = self.faces[params.font_id.0].font.clone();
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
        let m = state.faces[id.0].font.as_swash().metrics(&[]);
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
        let metrics = state.faces[id.0].font.as_swash().glyph_metrics(&[]);
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
        let glyph = state.faces[id.0].font.as_swash().charmap().map(ch);
        (glyph != 0).then_some(GlyphId(u32::from(glyph)))
    }
    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let image = state.render(params)?;
        let bounds = Bounds::new(
            point(
                DevicePixels(image.placement.left),
                DevicePixels(-image.placement.top),
            ),
            size(
                DevicePixels(i32::try_from(image.placement.width)?),
                DevicePixels(i32::try_from(image.placement.height)?),
            ),
        );
        state.images.insert(params.clone(), image);
        Ok(bounds)
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
        let mut image = match state.images.remove(params) {
            Some(image) => image,
            None => state.render(params)?,
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
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut result = LineLayout {
            font_size,
            len: text.len(),
            ..LineLayout::default()
        };
        if runs.is_empty() {
            return result;
        }
        let mut offset = 0;
        let inputs = runs
            .iter()
            .map(|run| {
                let start = offset;
                offset += run.len;
                (start..offset, run.font_id)
            })
            .collect::<Vec<_>>();
        let mut start = 0;
        let boundaries = text
            .char_indices()
            .filter(|(_, ch)| matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}'))
            .map(|(index, ch)| (index, index + ch.len_utf8()))
            .chain(std::iter::once((text.len(), text.len())));
        for (end, next) in boundaries {
            if end > start {
                let selected = inputs
                    .iter()
                    .filter_map(|(range, id)| {
                        let a = range.start.max(start);
                        let b = range.end.min(end);
                        (a < b).then_some((a - start..b - start, *id))
                    })
                    .collect::<Vec<_>>();
                let mut line = state.paragraph(&text[start..end], font_size, &selected);
                for run in &mut line.runs {
                    for glyph in &mut run.glyphs {
                        glyph.index += start;
                        glyph.position.x += result.width;
                    }
                }
                result.runs.extend(line.runs);
                result.width += line.width;
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
