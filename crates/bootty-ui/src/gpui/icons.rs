//! GPUI iconflow font loading and slug rendering.

use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use ab_glyph::{Font as _, FontRef, PxScale};
use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, RenderImage, Style as GpuiStyle, Styled, Window, img, px,
};
use iconflow::{Pack, Size, Style, try_icon};
use image::{Frame, ImageBuffer, Rgba as ImageRgba};
use num_traits::ToPrimitive as _;

use super::theme::{UI_ICON_MEDIUM, UI_ICON_SMALL, UI_ICON_XSMALL};

/// Standard icon boxes used by the shared chrome controls.
///
/// Keeping these sizes in one place avoids the slightly different 13/14/15/16px icon boxes that
/// made controls look uneven. Callers that need a custom size can continue using [`icon`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconSize {
    XSmall,
    Small,
    Medium,
}

impl IconSize {
    #[must_use]
    pub const fn pixels(self) -> f32 {
        match self {
            Self::XSmall => UI_ICON_XSMALL,
            Self::Small => UI_ICON_SMALL,
            Self::Medium => UI_ICON_MEDIUM,
        }
    }
}

/// Render an icon using one of the shared chrome icon boxes.
#[must_use]
pub fn sized_icon(slug: &str, size: IconSize, tint: Hsla) -> AnyElement {
    icon(slug, size.pixels(), tint)
}

/// Render an icon slug with its embedded font. Unknown slugs occupy no space.
#[must_use]
pub fn icon(slug: &str, size: f32, tint: Hsla) -> AnyElement {
    if slug == "bootty" {
        return img("icons/bootty.png")
            .size(px(normalize_size(size)))
            .into_any_element();
    }
    let Some(resolved) = resolve(slug) else {
        return gpui_kit::Empty.into_any_element();
    };
    IconElement {
        resolved,
        size: px(normalize_size(size)),
        tint,
    }
    .into_any_element()
}

/// A single icon glyph rasterized from its embedded font.
///
/// GPUI rejects font faces without an `m` glyph on macOS because ordinary text measurement relies
/// on it. Icon fonts intentionally omit that character, so rendering them through GPUI's text
/// fallback produces tofu. Rasterizing the requested outline keeps icons out of the UI text stack.
struct IconElement {
    resolved: ResolvedIcon,
    size: Pixels,
    tint: Hsla,
}

impl IntoElement for IconElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for IconElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Arc<RenderImage>>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = GpuiStyle::default();
        style.size.width = self.size.into();
        style.size.height = self.size.into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        rasterized_icon(self.resolved, f32::from(self.size), self.tint)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        image: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        if let Some(image) = image {
            let _ = window.paint_image(
                bounds,
                bounds,
                gpui_kit::Corners::default(),
                Arc::clone(image),
                0,
                false,
            );
        }
    }
}

/// Return whether `slug` resolves to an icon in the embedded icon inventory.
#[must_use]
pub fn has_icon(slug: &str) -> bool {
    resolve(slug).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ResolvedIcon {
    glyph: char,
    family: &'static str,
    bytes: &'static [u8],
}

fn resolve(slug: &str) -> Option<ResolvedIcon> {
    let (pack, slug) = if let Some((pack, slug)) = slug.split_once(':') {
        match pack {
            "bootstrap" => (Pack::Bootstrap, Cow::Borrowed(slug)),
            "lucide" => (Pack::Lucide, Cow::Borrowed(slug)),
            // Phosphor duotone needs two overlaid glyphs. Use the complete regular outline
            // while accepting the legacy suffix until iconflow exposes layered icons.
            "phosphor" => (Pack::Phosphor, Cow::Owned(slug.replace("-duotone", ""))),
            "tabler" => (Pack::Tabler, Cow::Borrowed(slug)),
            _ => return None,
        }
    } else {
        let (pack, slug) = match slug {
            "coffee-cup" => (Pack::Tabler, "coffee-off"),
            "coffee-cup-filled" => (Pack::Tabler, "coffee"),
            "openai" | "claude" | "anthropic" => (Pack::Bootstrap, slug),
            other => (Pack::Lucide, other),
        };
        (pack, Cow::Borrowed(slug))
    };
    let resolved = try_icon(pack, slug.as_ref(), Style::Regular, Size::Regular).ok()?;
    let asset = iconflow::fonts()
        .iter()
        .find(|asset| asset.family == resolved.family)?;
    Some(ResolvedIcon {
        glyph: char::from_u32(resolved.codepoint)?,
        family: resolved.family,
        bytes: asset.bytes,
    })
}

fn rasterized_icon(resolved: ResolvedIcon, size: f32, tint: Hsla) -> Option<Arc<RenderImage>> {
    const RASTER_SCALE: f32 = 2.0;
    type Key = (&'static str, char, u32, Hsla);
    static CACHE: OnceLock<Mutex<HashMap<Key, Arc<RenderImage>>>> = OnceLock::new();

    let key = (resolved.family, resolved.glyph, size.to_bits(), tint);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(image) = cache.lock().ok()?.get(&key) {
        return Some(Arc::clone(image));
    }

    let extent = (size * RASTER_SCALE).ceil().max(1.0).to_u32()?;
    let font = FontRef::try_from_slice(resolved.bytes).ok()?;
    let glyph = font
        .glyph_id(resolved.glyph)
        .with_scale(PxScale::from(size * RASTER_SCALE));
    let outlined = font.outline_glyph(glyph)?;
    let bounds = outlined.px_bounds();
    let glyph_width = bounds.width().ceil().max(0.0).to_u32()?;
    let glyph_height = bounds.height().ceil().max(0.0).to_u32()?;
    let offset_x = extent.saturating_sub(glyph_width) / 2;
    let offset_y = extent.saturating_sub(glyph_height) / 2;
    let color = tint.to_rgb();
    let len = usize::try_from(extent)
        .ok()?
        .checked_pow(2)?
        .checked_mul(4)?;
    let mut buffer = ImageBuffer::from_raw(extent, extent, vec![0; len])?;
    outlined.draw(|x, y, coverage| {
        let alpha = (color.a * coverage * 255.0)
            .round()
            .clamp(0.0, 255.0)
            .to_u8()
            .unwrap_or(0);
        let pixel = ImageRgba([
            (color.b * 255.0)
                .round()
                .clamp(0.0, 255.0)
                .to_u8()
                .unwrap_or(0),
            (color.g * 255.0)
                .round()
                .clamp(0.0, 255.0)
                .to_u8()
                .unwrap_or(0),
            (color.r * 255.0)
                .round()
                .clamp(0.0, 255.0)
                .to_u8()
                .unwrap_or(0),
            alpha,
        ]);
        if let Some((x, y)) = offset_x.checked_add(x).zip(offset_y.checked_add(y))
            && let Some(target) = buffer.get_pixel_mut_checked(x, y)
        {
            *target = pixel;
        }
    });
    let image = Arc::new(RenderImage::new([Frame::new(buffer)]));
    cache.lock().ok()?.insert(key, Arc::clone(&image));
    Some(image)
}

const fn normalize_size(size: f32) -> f32 {
    if size.is_finite() {
        size.max(1.0)
    } else {
        UI_ICON_MEDIUM
    }
}
