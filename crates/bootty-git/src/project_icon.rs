//! Bounded project artwork discovery on the filesystem that owns the project.

use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ImageDecoder as _, ImageReader};
use serde::{Deserialize, Serialize};

use crate::CommandRunner;

pub const MAX_PROJECT_ICON_BYTES: usize = 512 * 1024;
const ICON_SIZE: u32 = 64;
const MAX_ICON_DIMENSION: u32 = 2048;

const ICON_STEMS: &[&str] = &[
    "public/apple-touch-icon",
    "apple-touch-icon",
    "public/favicon",
    "favicon",
    "app/icon",
    "src/app/icon",
    "public/icon",
    "assets/icon",
    "src/assets/icon",
    "static/favicon",
    "public/logo",
    "logo",
    "src-tauri/icons/icon",
    "icon",
    "crates/bootty/assets/bootty.icon/Assets/Image",
];
const ICON_EXTENSIONS: &[&str] = &["png", "webp", "ico", "svg"];

/// Validated, thumbnail-sized pixels ready for native rendering. Pixels use GPUI's BGRA order.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProjectIcon {
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Inspect conventional artwork paths. Call only from a filesystem worker.
#[must_use]
pub fn detect_project_icon(root: &Path) -> Option<ProjectIcon> {
    let root = root.canonicalize().ok()?;
    for path in candidates() {
        let Some(bytes) = read_icon_file(&root, &path) else {
            continue;
        };
        if let Some(icon) = decode_project_icon(&path, &bytes) {
            return Some(icon);
        }
    }
    None
}

/// Read through the injected execution host. Missing or unavailable hosts produce no artwork;
/// this function never consults the desktop's filesystem.
pub fn detect_project_icon_with_runner(
    root: &str,
    runner: &impl CommandRunner,
) -> Option<ProjectIcon> {
    // One bounded host round trip. The first four candidate files are enough for damaged-file
    // fallback; increase this budget only when projects need more alternate artwork sources.
    let script = format!(
        "root=$(cd -- \"$1\" && pwd -P) || exit 1; shift; count=0; for name do path=$root/$name; [ -f \"$path\" ] || continue; [ ! -L \"$path\" ] || continue; parent=$(dirname -- \"$path\"); parent=$(cd -- \"$parent\" && pwd -P) || continue; case \"$parent/\" in \"$root/\"*) ;; *) continue ;; esac; size=$(wc -c < \"$path\") || continue; [ \"$size\" -le {} ] || continue; printf '%s\\n' \"$name\"; head -c {} \"$path\" | base64 | tr -d '\\r\\n'; printf '\\n'; count=$((count + 1)); [ \"$count\" -lt 4 ] || break; done",
        MAX_PROJECT_ICON_BYTES,
        MAX_PROJECT_ICON_BYTES + 1,
    );
    let mut args = vec![
        "-c".to_owned(),
        script,
        "bootty-project-icon".to_owned(),
        root.to_owned(),
    ];
    args.extend(candidates());
    let output = runner.run("sh", &args).ok()?;
    if !output.success || output.stdout.len() > 4 * (MAX_PROJECT_ICON_BYTES * 4 / 3 + 256) {
        return None;
    }
    let mut lines = output.stdout.lines();
    while let (Some(source), Some(encoded)) = (lines.next(), lines.next()) {
        let Ok(bytes) = STANDARD.decode(encoded) else {
            continue;
        };
        if let Some(icon) = decode_project_icon(source, &bytes) {
            return Some(icon);
        }
    }
    None
}

/// Decode untrusted image bytes with explicit compressed, dimension, and allocation limits.
#[must_use]
pub fn decode_project_icon(source: &str, bytes: &[u8]) -> Option<ProjectIcon> {
    if bytes.is_empty() || bytes.len() > MAX_PROJECT_ICON_BYTES {
        return None;
    }
    if Path::new(source)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        return decode_svg_icon(source, bytes);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    if !matches!(
        reader.format()?,
        image::ImageFormat::Png | image::ImageFormat::WebP | image::ImageFormat::Ico
    ) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ICON_DIMENSION);
    limits.max_image_height = Some(MAX_ICON_DIMENSION);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || width > MAX_ICON_DIMENSION || height > MAX_ICON_DIMENSION {
        return None;
    }
    limits.reserve(decoder.total_bytes()).ok()?;
    decoder.set_limits(limits).ok()?;
    let artwork = image::DynamicImage::from_decoder(decoder).ok()?;
    let thumbnail = artwork
        .thumbnail(ICON_SIZE.min(width), ICON_SIZE.min(height))
        .to_rgba8();
    let (width, height) = thumbnail.dimensions();
    let mut bgra = thumbnail.into_raw();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(ProjectIcon {
        source: source.to_owned(),
        width,
        height,
        bgra,
    })
}

#[must_use]
pub fn project_monogram(name: &str) -> String {
    name.trim()
        .chars()
        .find(|character| character.is_alphanumeric())
        .map_or_else(
            || "?".to_owned(),
            |character| character.to_uppercase().collect(),
        )
}

fn candidates() -> impl Iterator<Item = String> {
    ICON_STEMS.iter().flat_map(|stem| {
        ICON_EXTENSIONS
            .iter()
            .map(move |extension| format!("{stem}.{extension}"))
    })
}

fn read_icon_file(root: &Path, relative: &str) -> Option<Vec<u8>> {
    let path = root.join(relative).canonicalize().ok()?;
    // Artwork symlinks must remain inside the project, including linked worktrees.
    if !path.starts_with(root) {
        return None;
    }
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > u64::try_from(MAX_PROJECT_ICON_BYTES).ok()? {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_PROJECT_ICON_BYTES.saturating_add(1)).ok()?)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_PROJECT_ICON_BYTES).then_some(bytes)
}

fn decode_svg_icon(source: &str, bytes: &[u8]) -> Option<ProjectIcon> {
    // Vector icons are self-contained. External and embedded images must not add filesystem
    // reads or unbounded nested image decoding to this host-scoped thumbnail operation.
    let options = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..resvg::usvg::Options::default()
    };
    let tree = resvg::usvg::Tree::from_str(std::str::from_utf8(bytes).ok()?, &options).ok()?;
    if tree.size().width() > 2048.0 || tree.size().height() > 2048.0 {
        return None;
    }
    let size = tree
        .size()
        .to_int_size()
        .scale_to(resvg::tiny_skia::IntSize::from_wh(ICON_SIZE, ICON_SIZE)?);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        f32::from(u16::try_from(size.width()).ok()?) / tree.size().width(),
        f32::from(u16::try_from(size.height()).ok()?) / tree.size().height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    if pixmap.pixels().iter().all(|pixel| pixel.alpha() == 0) {
        return None;
    }
    let bgra = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let pixel = pixel.demultiply();
            [pixel.blue(), pixel.green(), pixel.red(), pixel.alpha()]
        })
        .collect();
    Some(ProjectIcon {
        source: source.to_owned(),
        width: size.width(),
        height: size.height(),
        bgra,
    })
}
