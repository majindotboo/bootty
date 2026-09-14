use std::sync::OnceLock;
#[cfg(target_os = "macos")]
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use ab_glyph::{Font, FontArc, FontVec, PxScale};

use crate::terminal_text::FontStyle;
use bootty_config::FontStyleAssignment;

/// Convert logical/device pixels per em to `ab_glyph`'s ascent-to-descent scale.
pub(crate) fn font_pixel_scale(font: &impl Font, em_size: f32) -> PxScale {
    font.pt_to_px_scale(em_size.max(1.0) * 72.0 / 96.0)
        .unwrap_or_else(|| PxScale::from(em_size.max(1.0)))
}

#[cfg(target_os = "macos")]
use std::path::PathBuf;

pub(super) fn query_font_id(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    style: FontStyle,
) -> Option<fontdb::ID> {
    query_assigned_font_id(database, families, style, &FontStyleAssignment::Automatic)
}

pub(super) fn query_assigned_font_id(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    style: FontStyle,
    assignment: &FontStyleAssignment,
) -> Option<fontdb::ID> {
    let base = query_base_font_id(database, families)?;
    resolve_font_assignment(
        database,
        base,
        font_weight(style).0,
        font_style(style),
        assignment,
    )
}

fn query_base_font_id(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
) -> Option<fontdb::ID> {
    for family in families {
        let family = match family {
            fontdb::Family::Name(name) => {
                fontdb::Family::Name(font_name_with_fallbacks(name, name))
            }
            family => *family,
        };
        if let Some(id) = database.query(&fontdb::Query {
            families: &[family],
            ..fontdb::Query::default()
        }) {
            return Some(id);
        }
        let fontdb::Family::Name(name) = family else {
            continue;
        };
        // Face names preserve the selected base weight instead of restarting at Regular.
        if let Some(face) = database
            .faces()
            .find(|face| face.post_script_name.eq_ignore_ascii_case(name))
        {
            return Some(face.id);
        }
        if let Some(face) = database.faces().find(|face| {
            database
                .with_face_data(face.id, |data, index| {
                    rustybuzz::ttf_parser::Face::parse(data, index).is_ok_and(|font| {
                        font.names().into_iter().any(|record| {
                            record.name_id == rustybuzz::ttf_parser::name_id::FULL_NAME
                                && record
                                    .to_string()
                                    .is_some_and(|full| full.eq_ignore_ascii_case(name))
                        })
                    })
                })
                .unwrap_or(false)
        }) {
            return Some(face.id);
        }
    }
    None
}

/// Apply semantic weight offsets in the font's OS/2 scale before native normalization.
pub(crate) fn resolve_font_assignment(
    database: &fontdb::Database,
    base: fontdb::ID,
    weight: u16,
    style: fontdb::Style,
    assignment: &FontStyleAssignment,
) -> Option<fontdb::ID> {
    let base = database.face(base)?;
    match assignment {
        FontStyleAssignment::Disabled => Some(base.id),
        FontStyleAssignment::Named(name) => Some(
            database
                .faces()
                .find(|face| {
                    face.families.iter().any(|(name, _)| {
                        base.families.iter().any(|(base_name, _)| name == base_name)
                    }) && advertised_font_style(database, face).eq_ignore_ascii_case(name)
                })
                .map_or(base.id, |face| face.id),
        ),
        FontStyleAssignment::Automatic => {
            if weight == 400 && style == fontdb::Style::Normal {
                return Some(base.id);
            }
            let families = base
                .families
                .iter()
                .map(|(name, _)| fontdb::Family::Name(name))
                .collect::<Vec<_>>();
            database
                .query(&fontdb::Query {
                    families: &families,
                    weight: fontdb::Weight(
                        u16::try_from(
                            i32::from(base.weight.0)
                                .saturating_add(i32::from(weight))
                                .saturating_sub(400)
                                .clamp(1, 1000),
                        )
                        .ok()?,
                    ),
                    style: if style == fontdb::Style::Normal {
                        base.style
                    } else {
                        style
                    },
                    ..fontdb::Query::default()
                })
                .or(Some(base.id))
        }
    }
}

fn font_name(database: &fontdb::Database, id: fontdb::ID, name_ids: &[u16]) -> Option<String> {
    database
        .with_face_data(id, |data, index| {
            let font = rustybuzz::ttf_parser::Face::parse(data, index).ok()?;
            name_ids.iter().find_map(|id| {
                font.names()
                    .into_iter()
                    .filter(|name| name.name_id == *id)
                    .find_map(|name| name.to_string().filter(|name| !name.is_empty()))
            })
        })
        .flatten()
}

pub(crate) fn advertised_font_style(
    database: &fontdb::Database,
    face: &fontdb::FaceInfo,
) -> String {
    font_name(
        database,
        face.id,
        &[
            rustybuzz::ttf_parser::name_id::TYPOGRAPHIC_SUBFAMILY,
            rustybuzz::ttf_parser::name_id::SUBFAMILY,
        ],
    )
    .unwrap_or_else(|| face.post_script_name.clone())
}

/// Advertised styles in the selected family, including italic and same-weight siblings.
pub(crate) fn font_style_names(name: &str) -> Vec<String> {
    let database = system_font_database();
    let Some(base) = query_base_font_id(database, &[fontdb::Family::Name(name)])
        .and_then(|id| database.face(id))
    else {
        return Vec::new();
    };
    let mut styles = database
        .faces()
        .filter(|face| {
            face.families
                .iter()
                .any(|(name, _)| base.families.iter().any(|(base_name, _)| name == base_name))
        })
        .map(|face| (face.weight.0, advertised_font_style(database, face)))
        .collect::<Vec<_>>();
    styles.sort();
    let mut seen = std::collections::HashSet::new();
    styles
        .into_iter()
        .map(|(_, name)| name)
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

pub(super) fn load_font_id(database: &fontdb::Database, id: fontdb::ID) -> Option<FontArc> {
    database
        .with_face_data(id, |data, index| {
            FontVec::try_from_vec_and_index(data.to_vec(), index)
                .ok()
                .map(FontArc::new)
        })
        .flatten()
}

pub(crate) fn load_matching_font(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    style: FontStyle,
) -> Option<FontArc> {
    query_font_id(database, families, style).and_then(|id| load_font_id(database, id))
}

pub(super) const fn font_weight(style: FontStyle) -> fontdb::Weight {
    match style {
        FontStyle::Bold | FontStyle::BoldItalic => fontdb::Weight::BOLD,
        FontStyle::Regular | FontStyle::Italic => fontdb::Weight::NORMAL,
    }
}

pub(super) const fn font_style(style: FontStyle) -> fontdb::Style {
    match style {
        FontStyle::Italic | FontStyle::BoldItalic => fontdb::Style::Italic,
        FontStyle::Regular | FontStyle::Bold => fontdb::Style::Normal,
    }
}

/// Every family name the system database exposes, sorted and de-duplicated. Scanning the database
/// is expensive, so callers read this once and keep the list.
#[must_use]
pub fn installed_family_names() -> Vec<String> {
    let mut names: Vec<String> = system_font_database()
        .faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

pub(crate) struct FontSelection {
    pub family: String,
    pub weight: u16,
    pub weights: Vec<(String, String)>,
    pub selected: String,
}

pub(crate) fn font_selection(name: &str) -> FontSelection {
    let database = system_font_database();
    let face = query_font_id(database, &[fontdb::Family::Name(name)], FontStyle::Regular)
        .and_then(|id| database.face(id));
    let family = face
        .and_then(|face| face.families.first())
        .map_or(name, |(name, _)| name)
        .to_owned();
    let weight = face.map_or(400, |face| face.weight.0);
    let mut weights = std::collections::BTreeMap::new();
    for face in database.faces().filter(|face| {
        face.style == fontdb::Style::Normal && face.families.iter().any(|(name, _)| name == &family)
    }) {
        let label = advertised_font_style(database, face);
        weights
            .entry((face.weight.0, label.clone()))
            .or_insert_with(|| (label, face.post_script_name.clone()));
    }
    let selected = face.map_or_else(|| name.to_owned(), |face| face.post_script_name.clone());
    if weights.is_empty() {
        weights.insert(
            (weight, "Regular".to_owned()),
            ("Regular".to_owned(), name.to_owned()),
        );
    }
    FontSelection {
        family,
        weight,
        weights: weights.into_values().collect(),
        selected,
    }
}

/// Retain the concrete face identity through native matching, including equal-weight siblings.
pub(crate) fn native_font(face: &fontdb::FaceInfo) -> gpui_kit::Font {
    let mut font = crate::font_mapping::exact_font(&face.post_script_name);
    font.weight = gpui_kit::FontWeight(platform_font_weight(face));
    font.style = match face.style {
        fontdb::Style::Normal => gpui_kit::FontStyle::Normal,
        fontdb::Style::Italic => gpui_kit::FontStyle::Italic,
        fontdb::Style::Oblique => gpui_kit::FontStyle::Oblique,
    };
    font
}

/// DirectWrite and Cosmic use the font's OS/2 weight directly.
#[cfg(not(target_os = "macos"))]
pub(crate) fn platform_font_weight(face: &fontdb::FaceInfo) -> f32 {
    f32::from(face.weight.0)
}

/// Match GPUI's native properties for the exact face selected by the font database.
#[cfg(target_os = "macos")]
pub(crate) fn platform_font_weight(face: &fontdb::FaceInfo) -> f32 {
    static WEIGHTS: OnceLock<Mutex<HashMap<fontdb::ID, f32>>> = OnceLock::new();
    let mut weights = WEIGHTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *weights.entry(face.id).or_insert_with(|| {
        // CoreText's normalized weight is not a linear mapping of OS/2 usWeightClass.
        // Reuse GPUI's loader so matching targets the chosen face's native properties.
        system_font_database()
            .with_face_data(face.id, |bytes, index| {
                font_kit::font::Font::from_bytes(Arc::new(bytes.to_vec()), index)
                    .ok()
                    .map(|font| font.properties().weight.0)
            })
            .flatten()
            .unwrap_or_else(|| f32::from(face.weight.0))
    })
}

pub(crate) fn font_with_weight(family: &str, weight: u16) -> String {
    let database = system_font_database();
    database
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: fontdb::Weight(weight),
            ..Default::default()
        })
        .and_then(|id| database.face(id))
        .map_or_else(|| family.to_owned(), |face| face.post_script_name.clone())
}

pub fn system_font_database() -> &'static fontdb::Database {
    static SYSTEM_FONT_DATABASE: OnceLock<fontdb::Database> = OnceLock::new();
    SYSTEM_FONT_DATABASE.get_or_init(load_system_font_database)
}

#[doc(hidden)]
#[must_use]
pub fn load_system_font_database() -> fontdb::Database {
    let mut database = fontdb::Database::new();
    for font in [
        crate::assets::MAPLE_MONO_NF_REGULAR,
        crate::assets::MAPLE_MONO_VARIABLE,
        crate::assets::LILEX_REGULAR,
        crate::assets::LILEX_BOLD,
        crate::assets::LILEX_ITALIC,
        crate::assets::LILEX_BOLD_ITALIC,
        crate::assets::IBM_PLEX_SANS_REGULAR,
        crate::assets::IBM_PLEX_SANS_SEMIBOLD,
        crate::assets::IBM_PLEX_SANS_ITALIC,
        crate::assets::IBM_PLEX_SANS_SEMIBOLD_ITALIC,
    ] {
        database.load_font_data(font.to_vec());
    }
    database.load_system_fonts();
    load_macos_fonts(&mut database);
    set_generic_monospace_family(&mut database);
    database
}

/// Maps Zed's stable virtual font names to the concrete families bundled with Bootty.
#[must_use]
pub fn font_name_with_fallbacks<'a>(name: &'a str, system: &'a str) -> &'a str {
    gpui_kit::font_name_with_fallbacks(name, system)
}

// Keep fontdb's generic family on a real fixed-pitch system font.
fn set_generic_monospace_family(database: &mut fontdb::Database) {
    if let Some(family) = MONOSPACE_FAMILY_CANDIDATES.iter().find(|family| {
        database
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                ..fontdb::Query::default()
            })
            .is_some()
    }) {
        database.set_monospace_family(*family);
    }
}

#[cfg(target_os = "macos")]
const MONOSPACE_FAMILY_CANDIDATES: &[&str] = &["SF Mono", "Menlo", "Monaco"];

#[cfg(windows)]
const MONOSPACE_FAMILY_CANDIDATES: &[&str] = &["Cascadia Mono", "Consolas", "Courier New"];

#[cfg(not(any(target_os = "macos", windows)))]
const MONOSPACE_FAMILY_CANDIDATES: &[&str] = &[
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
    "JetBrains Mono",
    "Source Code Pro",
];

#[cfg(target_os = "macos")]
fn load_macos_fonts(database: &mut fontdb::Database) {
    for dir in [
        PathBuf::from("/opt/zerobrew/share/fonts"),
        PathBuf::from("/opt/homebrew/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ] {
        database.load_fonts_dir(dir);
    }
}

#[cfg(not(target_os = "macos"))]
const fn load_macos_fonts(_database: &mut fontdb::Database) {}
