//! Semantic icon rendering backed by iconflow's embedded fonts.
//!
//! Callers use Bootty's stable semantic enum or icon slug strings; this module
//! handles compatibility aliases and keeps egui/font details out of extension APIs.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, Pos2};
use iconflow::{Pack, Size, Style, try_icon};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Terminal,
    Editor,
    Package,
    GitBranch,
    Bot,
    Sparkles,
}

impl Icon {
    pub const ALL: [Icon; 6] = [
        Self::Terminal,
        Self::Editor,
        Self::Package,
        Self::GitBranch,
        Self::Bot,
        Self::Sparkles,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Editor => "square-pen",
            Self::Package => "package",
            Self::GitBranch => "git-branch",
            Self::Bot => "bot",
            Self::Sparkles => "sparkles",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedIcon {
    pub family: &'static str,
    pub codepoint: u32,
}

/// Resolve an icon slug exposed to status/extensions.
pub fn resolve_slug(slug: &str) -> Option<ResolvedIcon> {
    let (pack, slug) = compatibility_icon(slug);
    let icon = try_icon(pack, slug, Style::Regular, Size::Regular).ok()?;
    Some(ResolvedIcon {
        family: icon.family,
        codepoint: icon.codepoint,
    })
}

/// Whether a status icon slug is drawable, so layout can reserve width only when needed.
pub fn has_slug(slug: &str) -> bool {
    resolve_slug(slug).is_some()
}

/// Merge iconflow's embedded icon fonts into egui font definitions.
pub fn add_icon_fonts(fonts: &mut FontDefinitions) {
    for asset in iconflow::fonts() {
        fonts.font_data.insert(
            asset.family.to_owned(),
            std::sync::Arc::new(FontData::from_static(asset.bytes)),
        );
        fonts
            .families
            .entry(FontFamily::Name(asset.family.into()))
            .or_default()
            .push(asset.family.to_owned());
    }
}

/// Install iconflow fonts during app startup, before any paint pass asks egui to resolve them.
pub fn install_icon_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    add_icon_fonts(&mut fonts);
    ctx.set_fonts(fonts);
}

/// Paint `icon` centered at `center`, `size` logical pixels square, tinted.
pub fn paint_icon(
    painter: &egui::Painter,
    icon: Icon,
    center: Pos2,
    size: f32,
    tint: egui::Color32,
) {
    paint_icon_slug(painter, icon.slug(), center, size, tint);
}

/// Paint an icon named by `slug` (as exposed to status modules), tinted.
/// Returns whether the slug resolved, so callers can lay out around it.
pub fn paint_icon_slug(
    painter: &egui::Painter,
    slug: &str,
    center: Pos2,
    size: f32,
    tint: egui::Color32,
) -> bool {
    let Some(icon) = resolve_slug(slug) else {
        return false;
    };
    let glyph = char::from_u32(icon.codepoint).unwrap_or('?');
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::new(size, FontFamily::Name(icon.family.into())),
        tint,
    );
    true
}

fn compatibility_icon(slug: &str) -> (Pack, &str) {
    match slug {
        "coffee-cup" => (Pack::Tabler, "coffee-off"),
        "coffee-cup-filled" => (Pack::Tabler, "coffee"),
        other => (Pack::Lucide, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_icons_resolve_through_iconflow() {
        for icon in Icon::ALL {
            assert!(
                resolve_slug(icon.slug()).is_some(),
                "semantic icon {:?} ('{}') failed to resolve through iconflow",
                icon,
                icon.slug()
            );
        }
    }

    #[test]
    fn status_bar_icon_slugs_resolve_for_public_status_api() {
        for slug in [
            "folder",
            "coffee-cup",
            "coffee-cup-filled",
            "plug-zap",
            "cpu",
            "memory-stick",
            "calendar",
            "clock",
        ] {
            assert!(has_slug(slug), "missing status icon '{slug}' in iconflow");
        }
    }

    #[test]
    fn missing_icon_slug_does_not_resolve() {
        assert!(!has_slug("not-a-real-lucide-icon"));
    }
}
