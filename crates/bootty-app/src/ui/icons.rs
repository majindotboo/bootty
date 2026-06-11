//! Semantic sidebar icons rasterized from bundled SVG bodies.
//!
//! `assets/icons-lucide.json` is a trimmed iconify pack (lucide, ISC license)
//! holding only the icons referenced here. Bodies render to a white alpha
//! shape once per (icon, pixel size) and are cached as textures in the egui
//! context; callers tint at draw time via `painter.image`.

use std::collections::HashMap;
use std::sync::OnceLock;

use eframe::egui::{self, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use resvg::tiny_skia;
use serde::Deserialize;

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

#[derive(Deserialize)]
struct Pack {
    width: u32,
    height: u32,
    icons: HashMap<String, PackIcon>,
}

#[derive(Deserialize)]
struct PackIcon {
    body: String,
}

fn pack() -> &'static Pack {
    static PACK: OnceLock<Pack> = OnceLock::new();
    PACK.get_or_init(|| {
        serde_json::from_slice(include_bytes!("../../assets/icons-lucide.json"))
            .expect("bundled icon pack json")
    })
}

/// Rasterize to a white shape so the draw-time tint supplies the color.
fn rasterize(icon: Icon, px: u32) -> Option<ColorImage> {
    let pack = pack();
    let body = &pack.icons.get(icon.slug())?.body;
    // Iconify bodies reference `currentColor`; `color="white"` on the wrapper
    // resolves it to white so the alpha channel carries the shape.
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" color="white">{body}</svg>"#,
        w = pack.width,
        h = pack.height,
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    let scale = px as f32 / pack.width.max(pack.height) as f32;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    Some(ColorImage::from_rgba_premultiplied([w, h], pixmap.data()))
}

fn icon_texture(ctx: &egui::Context, icon: Icon, px: u32) -> Option<TextureHandle> {
    let id = egui::Id::new(("bootty-icon", icon, px));
    if let Some(handle) = ctx.data(|data| data.get_temp::<TextureHandle>(id)) {
        return Some(handle);
    }
    let image = rasterize(icon, px)?;
    let handle = ctx.load_texture(
        format!("icon:{}:{px}", icon.slug()),
        image,
        TextureOptions::LINEAR,
    );
    ctx.data_mut(|data| data.insert_temp(id, handle.clone()));
    Some(handle)
}

/// Paint `icon` centered at `center`, `size` logical pixels square, tinted.
pub fn paint_icon(
    painter: &egui::Painter,
    icon: Icon,
    center: Pos2,
    size: f32,
    tint: egui::Color32,
) {
    let px = (size * painter.ctx().pixels_per_point()).round().max(1.0) as u32;
    let Some(texture) = icon_texture(painter.ctx(), icon, px) else {
        return;
    };
    painter.image(
        texture.id(),
        Rect::from_center_size(center, egui::vec2(size, size)),
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        tint,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_rasterizes_to_a_visible_shape() {
        for icon in Icon::ALL {
            let image = rasterize(icon, 16)
                .unwrap_or_else(|| panic!("{:?} ('{}') failed to rasterize", icon, icon.slug()));
            assert_eq!(image.size, [16, 16]);
            assert!(
                image.pixels.iter().any(|pixel| pixel.a() > 0),
                "{:?} rasterized fully transparent — currentColor resolution broke",
                icon
            );
        }
    }
}
