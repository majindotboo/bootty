use super::clusters::{ShapedCluster, is_combining_mark, is_private_use, is_variation_selector};
#[cfg(target_os = "macos")]
use super::coretext;
use super::shaping;
use super::shaping::font_has_ligature_features;
use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont, point};
use num_traits::ToPrimitive as _;
use std::collections::HashMap;

use crate::font_database::{
    font_style, font_weight, load_font_id, query_assigned_font_id, query_font_id,
    system_font_database,
};
use crate::terminal_font_face::FontFaceMetrics;
use crate::terminal_text::{FontFeature, ResolvedFontFace};

#[derive(Clone, Debug)]
pub(super) struct FontLibrary {
    database: &'static fontdb::Database,
    font_ids: HashMap<ResolvedFontFace, Option<fontdb::ID>>,
    fonts_by_id: HashMap<fontdb::ID, Option<FontArc>>,
    fallback_font_ids: HashMap<FallbackFontKey, Option<fontdb::ID>>,
    metrics: HashMap<FontMetricsKey, FontFaceMetrics>,
    shaping_capable: HashMap<fontdb::ID, bool>,
    bold_synthesis: HashMap<(fontdb::ID, ResolvedFontFace), bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FallbackFontKey {
    face: ResolvedFontFace,
    text: String,
    physical_font_size_bits: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FontMetricsKey {
    face: ResolvedFontFace,
    scale_x_bits: u32,
    scale_y_bits: u32,
    constraint_cells: u16,
    width: u32,
    height: u32,
}

impl FontLibrary {
    pub(super) fn new() -> Self {
        Self {
            database: system_font_database(),
            font_ids: HashMap::new(),
            fonts_by_id: HashMap::new(),
            fallback_font_ids: HashMap::new(),
            metrics: HashMap::new(),
            shaping_capable: HashMap::new(),
            bold_synthesis: HashMap::new(),
        }
    }

    /// Shapes text after `FontLibrary` resolves the primary face and caches its capability.
    pub(super) fn shape_into_clusters(
        &mut self,
        face: &ResolvedFontFace,
        text: &str,
        font_size: f32,
        features: &[FontFeature],
        clusters: &mut Vec<ShapedCluster>,
    ) -> Option<(u16, usize)> {
        let id = self.primary_font_id(face)?;
        // Explicit non-ligature features (for example `zero` or `ss01`) still
        // require shaping when a font has no common ligature tables.
        let explicit_features = features.iter().any(|feature| feature.tag() != *b"liga");
        if !explicit_features
            && !text.chars().any(is_combining_mark)
            && !self.font_has_shaping_features(id)
        {
            return None;
        }
        let font = self.font_for_id(id)?;
        shaping::shape_clusters(
            self.database,
            id,
            &font,
            text,
            font_size,
            features,
            clusters,
        )
    }
    fn primary_font_id(&mut self, face: &ResolvedFontFace) -> Option<fontdb::ID> {
        if !self.font_ids.contains_key(face) {
            let mut id = None;
            for family in std::iter::once(&face.family).chain(face.fallback_families.iter()) {
                let query_family = if family == "monospace" {
                    fontdb::Family::Monospace
                } else {
                    fontdb::Family::Name(family)
                };
                if let Some(found) = query_assigned_font_id(
                    self.database,
                    &[query_family],
                    face.style,
                    &face.assignment,
                ) {
                    id = Some(found);
                    break;
                }
            }
            let id = id.or_else(|| {
                query_assigned_font_id(
                    self.database,
                    &[fontdb::Family::Monospace],
                    face.style,
                    &face.assignment,
                )
            });
            self.font_ids.insert(face.clone(), id);
        }
        self.font_ids.get(face).copied().flatten()
    }

    fn font_has_shaping_features(&mut self, id: fontdb::ID) -> bool {
        let database = self.database;
        *self.shaping_capable.entry(id).or_insert_with(|| {
            database
                .with_face_data(id, font_has_ligature_features)
                .unwrap_or(false)
        })
    }

    pub(super) fn font_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
    ) -> Option<(FontArc, bool)> {
        let id = self.font_id_for_cluster(face, cluster, physical_font_size, true)?;
        let synthesize_bold = self.synthesize_bold(id, face);
        self.font_for_id(id).map(|font| (font, synthesize_bold))
    }

    pub(super) fn shape_fallback_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        font_size: f32,
        physical_font_size: f32,
    ) -> Option<Vec<super::clusters::ShapedGlyph>> {
        let id = self.font_id_for_cluster(face, cluster, physical_font_size, true)?;
        self.database
            .with_face_data(id, |data, index| {
                shaping::shape_run(data, index, &cluster.text, font_size, &[])
            })
            .flatten()
    }

    pub(super) fn primary_platform_font(
        &mut self,
        face: &ResolvedFontFace,
    ) -> Option<gpui_kit::Font> {
        let id = self.primary_font_id(face)?;
        self.database
            .face(id)
            .map(crate::font_database::native_font)
    }

    pub(super) fn platform_font_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
    ) -> Option<(gpui_kit::Font, bool)> {
        let id = self.font_id_for_cluster(face, cluster, physical_font_size, true)?;
        let info = self.database.face(id)?;
        let font = crate::font_database::native_font(info);
        Some((font, self.synthesize_bold(id, face)))
    }

    fn font_id_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
        fallback_to_primary: bool,
    ) -> Option<fontdb::ID> {
        let primary_id = self.primary_font_id(face);
        let primary_font = primary_id.and_then(|id| self.font_for_id(id));
        if fallback_to_primary && primary_font.is_none() {
            return None;
        }
        if primary_font
            .as_ref()
            .is_some_and(|font| font_supports_cluster(font, cluster))
        {
            return primary_id;
        }

        for family in &face.fallback_families {
            let candidate = ResolvedFontFace {
                family: family.clone(),
                fallback_families: Vec::new(),
                style: face.style,
                assignment: face.assignment.clone(),
            };
            if let Some(id) = self.primary_font_id(&candidate)
                && let Some(font) = self.font_for_id(id)
                && font_supports_cluster(&font, cluster)
            {
                return Some(id);
            }
        }

        let fallback_key = FallbackFontKey {
            face: face.clone(),
            text: cluster.text.clone(),
            physical_font_size_bits: physical_font_size.to_bits(),
        };
        let fallback_id = if let Some(id) = self.fallback_font_ids.get(&fallback_key) {
            *id
        } else {
            let id = font_id_supporting_cluster(self.database, face, cluster, physical_font_size);
            // Terminal output can supply unlimited distinct clusters. Keep font fallback
            // decisions bounded like shaped text; eviction only requires resolving again.
            if self.fallback_font_ids.len() >= super::SHAPED_RUN_CACHE_CAP {
                self.fallback_font_ids.clear();
            }
            self.fallback_font_ids.insert(fallback_key, id);
            id
        };
        if fallback_to_primary {
            fallback_id
                .filter(|id| self.font_for_id(*id).is_some())
                .or(primary_id)
        } else {
            fallback_id
        }
    }

    pub(super) fn font_for_face(&mut self, face: &ResolvedFontFace) -> Option<FontArc> {
        let id = self.primary_font_id(face)?;
        self.font_for_id(id)
    }

    pub(super) fn synthesize_bold_for_face(&mut self, face: &ResolvedFontFace) -> bool {
        self.primary_font_id(face)
            .is_some_and(|id| self.synthesize_bold(id, face))
    }

    fn synthesize_bold(&mut self, id: fontdb::ID, face: &ResolvedFontFace) -> bool {
        if font_weight(face.style) != fontdb::Weight::BOLD
            || face.assignment != bootty_config::FontStyleAssignment::Automatic
        {
            return false;
        }
        *self
            .bold_synthesis
            .entry((id, face.clone()))
            .or_insert_with(|| needs_synthetic_bold(self.database, id, face))
    }

    fn font_for_id(&mut self, id: fontdb::ID) -> Option<FontArc> {
        let database = self.database;
        self.fonts_by_id
            .entry(id)
            .or_insert_with(|| load_font_id(database, id))
            .clone()
    }

    pub(super) fn font_face_metrics_for(
        &mut self,
        face: &ResolvedFontFace,
        font: &FontArc,
        scale: PxScale,
        constraint_cells: u16,
        width: u32,
        height: u32,
    ) -> FontFaceMetrics {
        let key = FontMetricsKey {
            face: face.clone(),
            scale_x_bits: scale.x.to_bits(),
            scale_y_bits: scale.y.to_bits(),
            constraint_cells,
            width,
            height,
        };
        *self
            .metrics
            .entry(key)
            .or_insert_with(|| font_face_metrics(font, scale, constraint_cells, width, height))
    }
}

fn font_supports_cluster(font: &FontArc, cluster: &ShapedCluster) -> bool {
    cluster
        .text
        .chars()
        .filter(|ch| !is_variation_selector(*ch))
        .all(|ch| font.glyph_id(ch) != GlyphId(0))
}

pub(super) fn font_face_metrics(
    font: &FontArc,
    scale: PxScale,
    constraint_cells: u16,
    width: u32,
    height: u32,
) -> FontFaceMetrics {
    let scaled = font.as_scaled(scale);
    let cell_width = width.to_f32().unwrap_or_default() / f32::from(constraint_cells.max(1));
    let pixel_height = height.to_f32().unwrap_or_default();
    let baseline = ((pixel_height - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let face_width = (' '..='~')
        .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
        .fold(0.0_f32, f32::max)
        .min(cell_width)
        .max(1.0);
    let face_height = scaled.height();
    let cap_height = scaled
        .outline_glyph(
            scaled
                .glyph_id('H')
                .with_scale_and_position(scale, point(0.0, 0.0)),
        )
        .map_or(face_height, |glyph| glyph.px_bounds().height());

    FontFaceMetrics {
        cell_width: cell_width
            .round()
            .clamp(1.0, f32::from(u16::MAX))
            .to_u16()
            .unwrap_or(1),
        cell_height: u16::try_from(height.max(1)).unwrap_or(u16::MAX),
        cell_baseline: (pixel_height - baseline)
            .round()
            .clamp(0.0, f32::from(u16::MAX))
            .to_u16()
            .unwrap_or(0),
        icon_height: f64::from(face_height),
        icon_height_single: f64::from(2.0f32.mul_add(cap_height, face_height) / 3.0),
        face_width: f64::from(face_width),
        face_height: f64::from(face_height),
        face_y: f64::from(((pixel_height - face_height) * 0.5).max(0.0)),
    }
}

fn needs_synthetic_bold(
    database: &fontdb::Database,
    id: fontdb::ID,
    face: &ResolvedFontFace,
) -> bool {
    let Some(selected) = database.face(id) else {
        return false;
    };
    let family_matches = |base: &&fontdb::FaceInfo| {
        base.families.iter().any(|(name, _)| {
            selected
                .families
                .iter()
                .any(|(selected_name, _)| selected_name == name)
        })
    };
    let configured_base = std::iter::once(&face.family)
        .chain(&face.fallback_families)
        .filter_map(|name| {
            query_font_id(
                database,
                &[if name == "monospace" {
                    fontdb::Family::Monospace
                } else {
                    fontdb::Family::Name(name)
                }],
                crate::terminal_text::FontStyle::Regular,
            )
        })
        .filter_map(|id| database.face(id))
        .find(family_matches);
    // Each fallback has its own base. OS-selected fallbacks use their family's Regular.
    let base = configured_base.or_else(|| {
        let families = selected
            .families
            .iter()
            .map(|(name, _)| fontdb::Family::Name(name))
            .collect::<Vec<_>>();
        query_font_id(
            database,
            &families,
            crate::terminal_text::FontStyle::Regular,
        )
        .and_then(|id| database.face(id))
    });
    base.is_some_and(|base| selected.weight <= base.weight)
}

fn font_id_supporting_cluster(
    database: &fontdb::Database,
    face: &ResolvedFontFace,
    cluster: &ShapedCluster,
    physical_font_size: f32,
) -> Option<fontdb::ID> {
    let ch = cluster.text.chars().next()?;
    let (wanted_weight, wanted_style) = assigned_font_properties(database, face);
    // System private-use coverage can be empty or unrelated to Nerd Fonts. Honor
    // the bundled icon face after the user's explicit families, before OS fallback.
    if is_private_use(ch)
        && let Some(id) = database.query(&fontdb::Query {
            families: &[fontdb::Family::Name("Maple Mono NF")],
            weight: wanted_weight,
            style: wanted_style,
            ..Default::default()
        })
        && load_font_id(database, id).is_some_and(|font| font_supports_cluster(&font, cluster))
    {
        return Some(id);
    }
    if let Some(id) = coretext_fallback_font_id(database, face, ch, physical_font_size)
        && load_font_id(database, id).is_some_and(|font| font_supports_cluster(&font, cluster))
    {
        return Some(id);
    }

    let faces = database
        .faces()
        .filter(|face| face.style == wanted_style && face.weight == wanted_weight)
        .chain(database.faces().filter(|face| face.style == wanted_style))
        .chain(database.faces());

    for face in faces {
        let Some(font) = load_font_id(database, face.id) else {
            continue;
        };
        if font_supports_cluster(&font, cluster) {
            return Some(face.id);
        }
    }

    None
}

fn assigned_font_properties(
    database: &fontdb::Database,
    face: &ResolvedFontFace,
) -> (fontdb::Weight, fontdb::Style) {
    let families = std::iter::once(&face.family)
        .chain(&face.fallback_families)
        .map(|name| {
            if name == "monospace" {
                fontdb::Family::Monospace
            } else {
                fontdb::Family::Name(name)
            }
        })
        .collect::<Vec<_>>();
    query_assigned_font_id(database, &families, face.style, &face.assignment)
        .and_then(|id| database.face(id))
        .map_or_else(
            || (font_weight(face.style), font_style(face.style)),
            |face| (face.weight, face.style),
        )
}

#[cfg(target_os = "macos")]
fn coretext_fallback_font_id(
    database: &fontdb::Database,
    face: &ResolvedFontFace,
    ch: char,
    physical_font_size: f32,
) -> Option<fontdb::ID> {
    let families = [fontdb::Family::Name(&face.family)];
    let primary = query_assigned_font_id(database, &families, face.style, &face.assignment)
        .and_then(|id| database.face(id));
    let name = primary.map_or(face.family.as_str(), |face| face.post_script_name.as_str());
    let names = coretext::fallback_names(name, ch, physical_font_size)?;
    font_id_for_postscript_or_family(database, &names.postscript, &names.family, face)
}

#[cfg(not(target_os = "macos"))]
fn coretext_fallback_font_id(
    _database: &fontdb::Database,
    _face: &ResolvedFontFace,
    _ch: char,
    _physical_font_size: f32,
) -> Option<fontdb::ID> {
    None
}

#[cfg(target_os = "macos")]
fn font_id_for_postscript_or_family(
    database: &fontdb::Database,
    postscript: &str,
    family: &str,
    face: &ResolvedFontFace,
) -> Option<fontdb::ID> {
    let (wanted_weight, wanted_style) = assigned_font_properties(database, face);
    database
        .faces()
        .find(|candidate| {
            candidate.post_script_name == postscript
                && candidate.style == wanted_style
                && candidate.weight == wanted_weight
        })
        .or_else(|| {
            database.faces().find(|candidate| {
                candidate
                    .families
                    .iter()
                    .any(|(candidate_family, _)| candidate_family == family)
                    && candidate.style == wanted_style
                    && candidate.weight == wanted_weight
            })
        })
        .or_else(|| {
            database
                .faces()
                .find(|candidate| candidate.post_script_name == postscript)
        })
        .or_else(|| {
            database.faces().find(|candidate| {
                candidate
                    .families
                    .iter()
                    .any(|(candidate_family, _)| candidate_family == family)
            })
        })
        .map(|candidate| candidate.id)
}
