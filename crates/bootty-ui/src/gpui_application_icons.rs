//! Process-wide cache of app icons, keyed by local bundle id or provider artwork URL. Local
//! icons load through `computer.icons` on a registered sender; provider artwork loads over HTTPS.
//! Windows repaint when icons arrive.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use gpui_kit::{
    AnyElement, App, Global, Hsla, Image, ImageFormat, IntoElement, ParentElement as _,
    Styled as _, div, img, px,
};

use crate::gpui::IconSize;

#[derive(Default)]
struct ApplicationIcons {
    icons: BTreeMap<String, Arc<Image>>,
    /// Asked for once per process, whether or not the helper found an icon.
    requested: BTreeSet<String>,
    sender: Option<BoundAppCommandSender>,
}
impl Global for ApplicationIcons {}

/// Composers register their command sender; every sender reaches the same app mailbox.
pub fn register(sender: &BoundAppCommandSender, cx: &mut App) {
    let cache = cx.default_global::<ApplicationIcons>();
    if cache.sender.is_none() {
        cache.sender = Some(sender.clone());
    }
}

/// Start loading icons this process has not asked for yet. Without a registered sender the
/// generic app glyph stays.
pub fn load(bundle_ids: impl IntoIterator<Item = String>, cx: &mut App) {
    let cache = cx.default_global::<ApplicationIcons>();
    let Some(sender) = cache.sender.clone() else {
        return;
    };
    let missing: Vec<String> = bundle_ids
        .into_iter()
        .filter(|bundle_id| cache.requested.insert(bundle_id.clone()))
        .collect();
    for batch in missing.chunks(bootty_computer::MAX_ICON_REQUEST) {
        let Ok(arguments) = serde_json::to_string(batch) else {
            continue;
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let Ok(receiver) = sender.submit(
            CommandInvocation::new("computer.icons", vec![arguments], Caller::Internal),
            deadline,
            CommandCancellation::new(),
        ) else {
            continue;
        };
        cx.spawn(async move |cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            let Ok(CommandOutcome::Success { value, .. }) = outcome else {
                return;
            };
            let icons: BTreeMap<String, String> = value
                .get("icons")
                .cloned()
                .and_then(|icons| serde_json::from_value(icons).ok())
                .unwrap_or_default();
            cx.update(|cx| {
                let cache = cx.default_global::<ApplicationIcons>();
                for (bundle_id, png) in icons {
                    if let Ok(png) = STANDARD.decode(png) {
                        cache.icons.insert(
                            bundle_id,
                            Arc::new(Image::from_bytes(ImageFormat::Png, png)),
                        );
                    }
                }
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

// Provider logos are small; a larger body is not an icon.
const MAX_REMOTE_ICON_BYTES: u64 = 512 * 1024;

/// Load provider catalog artwork. Only these catalog URLs are fetched; Bootty has no general
/// remote image loading, so agent output cannot make it request arbitrary URLs.
pub fn load_remote(urls: impl IntoIterator<Item = String>, cx: &mut App) {
    let cache = cx.default_global::<ApplicationIcons>();
    let missing: Vec<String> = urls
        .into_iter()
        .filter(|url| url.starts_with("https://") && cache.requested.insert(url.clone()))
        .collect();
    for url in missing {
        cx.spawn(async move |cx| {
            let fetched = cx
                .background_executor()
                .spawn({
                    let url = url.clone();
                    async move { fetch_remote(&url) }
                })
                .await;
            let Some((format, bytes)) = fetched else {
                return;
            };
            cx.update(|cx| {
                cx.default_global::<ApplicationIcons>()
                    .icons
                    .insert(url, Arc::new(Image::from_bytes(format, bytes)));
                cx.refresh_windows();
            });
        })
        .detach();
    }
}

fn fetch_remote(url: &str) -> Option<(ImageFormat, Vec<u8>)> {
    let mut response = ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .call()
        .ok()?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_REMOTE_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).ok()? > MAX_REMOTE_ICON_BYTES {
        return None;
    }
    Some((image_format(&bytes)?, bytes))
}

/// Trust the bytes, not the server's content type.
fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF8") {
        Some(ImageFormat::Gif)
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(ImageFormat::Webp)
    } else {
        let head = bytes.get(..bytes.len().min(1024))?;
        String::from_utf8_lossy(head)
            .contains("<svg")
            .then_some(ImageFormat::Svg)
    }
}

/// The icon for a bundle id or artwork URL, or the generic app glyph until it loads. Both fill
/// the same square.
pub fn render(key: &str, size: IconSize, tint: Hsla, cx: &App) -> AnyElement {
    let icon = cx
        .try_global::<ApplicationIcons>()
        .and_then(|cache| cache.icons.get(key))
        .cloned();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(size.pixels()))
        .child(icon.map_or_else(
            || crate::gpui::sized_icon("app-window", size, tint),
            |icon| img(icon).size_full().into_any_element(),
        ))
        .into_any_element()
}
