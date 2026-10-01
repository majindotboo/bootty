//! Native project thumbnails shared by setup and session navigation.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
};

use bootty_git::ProjectIcon;
use gpui_kit::{
    AnyElement, Hsla, IntoElement as _, ParentElement as _, RenderImage, Styled as _,
    StyledImage as _, div, img, rems,
};

const MAX_CACHED_ARTWORK: usize = 128;
type ArtworkCache = VecDeque<(ProjectIcon, Arc<RenderImage>)>;
static CACHE: OnceLock<Mutex<ArtworkCache>> = OnceLock::new();

/// Render already-decoded project artwork. The size follows the application's rem scale.
#[must_use]
pub fn project_artwork(icon: &ProjectIcon, size_rem: f32) -> AnyElement {
    let Some(image) = project_artwork_image(icon) else {
        return gpui_kit::Empty.into_any_element();
    };
    img(image)
        .size(rems(size_rem))
        .object_fit(gpui_kit::ObjectFit::Contain)
        .into_any_element()
}

fn project_artwork_image(icon: &ProjectIcon) -> Option<Arc<RenderImage>> {
    // Remote catalogs are untrusted even when normal discovery has bounded their thumbnails.
    if icon.width == 0
        || icon.height == 0
        || icon.width > 64
        || icon.height > 64
        || u64::from(icon.width)
            .checked_mul(u64::from(icon.height))?
            .checked_mul(4)?
            != u64::try_from(icon.bgra.len()).ok()?
    {
        return None;
    }
    let mut cache = CACHE.get_or_init(Mutex::default).lock().ok()?;
    if let Some(index) = cache.iter().position(|(cached, _)| cached == icon) {
        let entry = cache.remove(index)?;
        let image = Arc::clone(&entry.1);
        cache.push_back(entry);
        return Some(image);
    }
    let pixels = image::ImageBuffer::from_raw(icon.width, icon.height, icon.bgra.clone())?;
    let image = Arc::new(RenderImage::new([image::Frame::new(pixels)]));
    if cache.len() >= MAX_CACHED_ARTWORK {
        cache.pop_front();
    }
    cache.push_back((icon.clone(), Arc::clone(&image)));
    drop(cache);
    Some(image)
}

/// Project identity when no validated artwork is available.
#[must_use]
pub fn project_monogram(
    name: &str,
    size_rem: f32,
    background: Hsla,
    foreground: Hsla,
) -> AnyElement {
    div()
        .size(rems(size_rem))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .bg(background)
        .text_color(foreground)
        .text_xs()
        .child(bootty_git::project_monogram(name))
        .into_any_element()
}
