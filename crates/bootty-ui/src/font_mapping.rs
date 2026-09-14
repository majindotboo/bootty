//! Resolve semantic UI weights through an immutable snapshot of the selected font styles.

use num_traits::ToPrimitive as _;

use bootty_config::{FontStyleAssignment, FontWeightAssignments, FontWeightRole};
use gpui_kit::{
    Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Global, PlatformTextSystem,
    SharedString,
};
use std::sync::{Arc, RwLock};

#[cfg(target_os = "macos")]
mod coretext;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod cosmic;
#[cfg(target_os = "windows")]
mod directwrite;

/// Use Bootty's font selections for both GPUI text and terminal glyph rendering.
/// Install the same mappings as a GPUI global before initializing Bootty's theme.
pub fn wrap_text_system(
    native: Arc<dyn PlatformTextSystem>,
    mappings: FontMappings,
) -> Arc<dyn PlatformTextSystem> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(coretext::CoreTextSystem::new(native, mappings))
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        Arc::new(cosmic::CosmicFontSystem::new(native, mappings))
    }
    #[cfg(target_os = "windows")]
    {
        Arc::new(
            directwrite::WindowsFontSystem::new(native, mappings)
                .expect("Bootty could not initialize DirectWrite fonts"),
        )
    }
}

const FACE_PREFIX: &str = ".Bootty-Face:";
const MAPPING_PREFIX: &str = ".Bootty-UI:";

/// Shared by the host text system and the application's accepted theme settings.
#[derive(Clone, Default)]
pub struct FontMappings(Arc<RwLock<Vec<Mapping>>>);
impl Global for FontMappings {}

struct Mapping {
    base: fontdb::ID,
    faces: Vec<(FontWeight, FontStyle, Font)>,
}

impl FontMappings {
    fn register(&self, mapping: Mapping) -> SharedString {
        let mut mappings = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let name = format!("{MAPPING_PREFIX}{}", mappings.len()).into();
        mappings.push(mapping);
        name
    }

    fn resolve(&self, requested: &Font) -> Option<Font> {
        let index = requested
            .family
            .strip_prefix(MAPPING_PREFIX)?
            .parse::<usize>()
            .ok()?;
        let mappings = self
            .0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mapping = mappings.get(index)?;
        let base = mapping.base;
        let selected = mapping
            .faces
            .iter()
            .find(|(weight, style, _)| *weight == requested.weight && *style == requested.style)
            .map(|(_, _, font)| font.clone());
        drop(mappings);
        let selected = selected.or_else(|| {
            ui_face(
                base,
                requested.weight.0.clamp(1.0, 1000.0).to_u16()?,
                requested.style,
                &FontStyleAssignment::Automatic,
            )
        })?;
        Some(Font {
            family: selected.family,
            weight: selected.weight,
            style: selected.style,
            ..requested.clone()
        })
    }
}

pub(crate) fn exact_font(postscript_name: &str) -> Font {
    gpui_kit::font(format!("{FACE_PREFIX}{postscript_name}"))
}

pub(crate) fn postscript_name(family: &str) -> Option<&str> {
    family.strip_prefix(FACE_PREFIX)
}

use crate::font_database::{
    native_font, query_font_id, resolve_font_assignment, system_font_database,
};

pub(crate) fn ui_font(
    families: &[String],
    assignments: &FontWeightAssignments,
    mappings: &FontMappings,
) -> Font {
    let database = system_font_database();
    let names = families
        .iter()
        .map(|name| fontdb::Family::Name(name))
        .collect::<Vec<_>>();
    let base =
        query_font_id(database, &names, crate::terminal_text::FontStyle::Regular).or_else(|| {
            query_font_id(
                database,
                &[fontdb::Family::Name("IBM Plex Sans")],
                crate::terminal_text::FontStyle::Regular,
            )
        });
    let fallback_names = families
        .iter()
        .skip(1)
        .map(|name| crate::font_database::font_selection(name).family)
        .collect::<Vec<_>>();
    let mut font = gpui_kit::font("IBM Plex Sans");
    font.features = FontFeatures::disable_ligatures();
    font.fallbacks =
        (!fallback_names.is_empty()).then(|| FontFallbacks::from_fonts(fallback_names));
    let Some(base) = base else { return font };
    let mut resolved = Vec::with_capacity(27);
    for role in FontWeightRole::ALL {
        let assignment = assignments
            .get(&role)
            .unwrap_or(&FontStyleAssignment::Automatic);
        for style in [FontStyle::Normal, FontStyle::Italic, FontStyle::Oblique] {
            if let Some(face) = ui_face(base, role.weight(), style, assignment) {
                resolved.push((FontWeight(f32::from(role.weight())), style, face));
            }
        }
    }
    font.family = mappings.register(Mapping {
        base,
        faces: resolved,
    });
    font
}

fn ui_face(
    base: fontdb::ID,
    weight: u16,
    style: FontStyle,
    assignment: &FontStyleAssignment,
) -> Option<Font> {
    let database = system_font_database();
    let native_style = match style {
        FontStyle::Normal => fontdb::Style::Normal,
        FontStyle::Italic => fontdb::Style::Italic,
        FontStyle::Oblique => fontdb::Style::Oblique,
    };
    // UI assignments change the weight role; emphasis remains an independent style request.
    let id = resolve_font_assignment(database, base, weight, fontdb::Style::Normal, assignment)?;
    let id = resolve_font_assignment(
        database,
        id,
        400,
        native_style,
        &FontStyleAssignment::Automatic,
    )?;
    database.face(id).map(native_font)
}
