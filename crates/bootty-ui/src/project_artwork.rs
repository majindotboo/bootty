//! Bounded project artwork discovery. File reads and image decoding stay on workers.
//! Unsupported vector/container assets keep the folder fallback until the host supplies bounded decoding.

use std::{
    collections::HashMap,
    io::{Cursor, Read as _},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bootty_config::config::RemoteConfig;
use bootty_host::{
    CancellableCommandRunner, CommandCancellation,
    files::{FileRequest, FileResponse},
    media::{MediaCancellation, MediaReader},
    remote::RemoteHost,
};
use gpui_kit::{AssetSource as _, RenderImage};
use image::{Frame, ImageReader};

pub const MAX_ARTWORK_BYTES: usize = 512 * 1024;
const MAX_ENTRIES: usize = 64;
const MAX_WORKERS: usize = 4;
const CACHE_TTL: Duration = Duration::from_mins(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const CANDIDATES: &[&str] = &[
    "icon.png",
    "logo.png",
    "assets/icon.png",
    "assets/logo.png",
    "public/favicon.png",
    "public/apple-touch-icon.png",
    "public/icon.png",
    "app/icon.png",
    "src/app/icon.png",
    "apps/web/public/favicon.png",
    "apps/web/public/apple-touch-icon.png",
    "resources/icon.png",
    "Resources/AppIcon.png",
    "src-tauri/icons/icon.png",
];
const ICON_SETS: &[&str] = &[
    "Assets.xcassets/AppIcon.appiconset/Contents.json",
    "Resources/Assets.xcassets/AppIcon.appiconset/Contents.json",
];

type CacheKey = (String, String, String, Option<String>);

struct Entry {
    updated: Instant,
    running: bool,
    image: Option<Arc<RenderImage>>,
    source: Arc<Mutex<Option<MediaCancellation>>>,
}

#[derive(Default)]
pub struct ProjectArtworkCache {
    entries: Arc<Mutex<HashMap<CacheKey, Entry>>>,
    retired: Arc<AtomicBool>,
    cancellation: CommandCancellation,
}

impl ProjectArtworkCache {
    /// Return cached artwork and schedule at most one refresh for this host/project.
    /// Missing and invalid images are cached too; they retain the caller's folder fallback.
    #[must_use]
    pub fn request(
        &self,
        scope: &str,
        cwd: &str,
        remote: Option<&RemoteConfig>,
        now: Instant,
        repaint: &Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Arc<RenderImage>> {
        self.request_with_icon(scope, cwd, remote, None, now, repaint)
    }

    /// A custom icon is a local asset explicitly chosen by the user, even for a remote project.
    #[must_use]
    pub fn request_with_icon(
        &self,
        scope: &str,
        cwd: &str,
        remote: Option<&RemoteConfig>,
        icon_path: Option<&str>,
        now: Instant,
        repaint: &Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Arc<RenderImage>> {
        if self.retired.load(Ordering::Acquire) {
            return None;
        }
        let remote_key = serde_json::to_string(&remote).ok()?;
        let key = (
            scope.to_owned(),
            cwd.to_owned(),
            remote_key,
            icon_path.map(str::to_owned),
        );
        let mut entries = self.entries.lock().ok()?;
        if self.retired.load(Ordering::Acquire) {
            return None;
        }
        let previous = entries.get(&key).and_then(|entry| entry.image.clone());
        if entries.get(&key).is_some_and(|entry| {
            entry.running || now.saturating_duration_since(entry.updated) < CACHE_TTL
        }) || entries.values().filter(|entry| entry.running).count() >= MAX_WORKERS
        {
            return previous;
        }
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&key) {
            let oldest = entries
                .iter()
                .filter(|(_, entry)| !entry.running)
                .min_by_key(|(_, entry)| entry.updated)
                .map(|(key, _)| key.clone())?;
            entries.remove(&oldest);
        }
        let source = Arc::new(Mutex::new(None));
        entries.insert(
            key.clone(),
            Entry {
                updated: now,
                running: true,
                image: previous.clone(),
                source: Arc::clone(&source),
            },
        );
        drop(entries);
        let cache = Arc::clone(&self.entries);
        let repaint = Arc::clone(repaint);
        let remote = remote.cloned().map(RemoteHost::new);
        let retired = Arc::clone(&self.retired);
        let cancellation = self.cancellation.clone();
        std::thread::spawn(move || {
            let deadline = now.checked_add(READ_TIMEOUT).unwrap_or(now);
            let runner = CancellableCommandRunner::with_deadline(cancellation, deadline);
            let image = if let Some(path) = &key.3 {
                read_artwork_file(path, None, &runner, &source, &retired)
                    .and_then(|bytes| decode_artwork(&bytes))
            } else {
                discover_project_artwork(&key.1, |path| {
                    if retired.load(Ordering::Acquire) {
                        return None;
                    }
                    read_artwork_file(path, remote.as_ref(), &runner, &source, &retired)
                })
            };
            if let Ok(mut entries) = cache.lock() {
                if retired.load(Ordering::Acquire) {
                    return;
                }
                if let Some(entry) = entries.get_mut(&key) {
                    entry.image = image;
                    entry.running = false;
                }
                drop(entries);
                if !retired.load(Ordering::Acquire) {
                    repaint();
                }
            }
        });
        previous
    }

    /// Stop host work and discard publications when the chrome owner goes away.
    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
        self.cancellation.cancel();
        if let Ok(mut entries) = self.entries.lock() {
            for entry in entries.values() {
                if let Ok(source) = entry.source.lock()
                    && let Some(source) = source.as_ref()
                {
                    source.cancel();
                }
            }
            entries.clear();
        }
    }
}

impl Drop for ProjectArtworkCache {
    fn drop(&mut self) {
        self.retire();
    }
}

/// Resolve repository, web and native application assets through the supplied host reader.
/// Every requested path is beneath `cwd`; discovery never follows a URL or runs project code.
#[must_use]
pub fn discover_project_artwork(
    cwd: &str,
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Option<Arc<RenderImage>> {
    let root = cwd;
    for manifest in ["Cargo.toml", "crates/bootty/Cargo.toml"] {
        if read_relative(root, manifest, &mut read).is_some_and(|bytes| is_bootty_manifest(&bytes))
        {
            return bootty_artwork();
        }
    }
    for relative in CANDIDATES {
        if let Some(image) =
            read_relative(root, relative, &mut read).and_then(|bytes| decode_artwork(&bytes))
        {
            return Some(image);
        }
    }
    let mut sets = ICON_SETS
        .iter()
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
    if let Some(name) = root
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
    {
        sets.push(format!(
            "{name}/Assets.xcassets/AppIcon.appiconset/Contents.json"
        ));
    }
    for set in sets {
        let Some(bytes) = read_relative(root, &set, &mut read) else {
            continue;
        };
        let Ok(contents) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let Some(images) = contents.get("images").and_then(serde_json::Value::as_array) else {
            continue;
        };
        let Some((parent, _)) = set.rsplit_once('/') else {
            continue;
        };
        for filename in images
            .iter()
            .take(16)
            .filter_map(|image| image.get("filename")?.as_str())
        {
            if filename.contains(['/', '\\']) || matches!(filename, "." | "..") {
                continue;
            }
            let relative = format!("{parent}/{filename}");
            if let Some(image) =
                read_relative(root, &relative, &mut read).and_then(|bytes| decode_artwork(&bytes))
            {
                return Some(image);
            }
        }
    }
    None
}

fn read_relative(
    root: &str,
    relative: &str,
    read: &mut impl FnMut(&str) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    if relative.contains('\\')
        || relative
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
    {
        return None;
    }
    let separator = if root.contains('\\') && !root.contains('/') {
        '\\'
    } else {
        '/'
    };
    let relative = relative.replace('/', &separator.to_string());
    let path = format!(
        "{}{separator}{relative}",
        root.trim_end_matches(['/', '\\'])
    );
    let bytes = read(&path)?;
    (bytes.len() <= MAX_ARTWORK_BYTES).then_some(bytes)
}

fn is_bootty_manifest(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|manifest| {
            manifest
                .get("package")?
                .get("name")?
                .as_str()
                .map(str::to_owned)
        })
        .is_some_and(|name| name == "bootty")
}

fn read_artwork_file(
    path: &str,
    remote: Option<&RemoteHost>,
    runner: &CancellableCommandRunner,
    cancellation: &Arc<Mutex<Option<MediaCancellation>>>,
    retired: &AtomicBool,
) -> Option<Vec<u8>> {
    let request = FileRequest::Read {
        path: path.to_owned(),
    };
    let response = remote
        .map_or_else(
            || request.execute(),
            |remote| request.execute_remote(remote, runner.clone()),
        )
        .ok()?;
    let bytes = match response {
        FileResponse::Document(file) => file.bytes().ok()?,
        FileResponse::Media(file) if file.len <= u64::try_from(MAX_ARTWORK_BYTES).ok()? => {
            let mut source = MediaReader::open(&file, remote).ok()?;
            let signal = source.cancellation();
            *cancellation.lock().ok()? = Some(signal.clone());
            if retired.load(Ordering::Acquire) {
                signal.cancel();
                *cancellation.lock().ok()? = None;
                return None;
            }
            let mut bytes = Vec::new();
            let loaded = source
                .by_ref()
                .take(u64::try_from(MAX_ARTWORK_BYTES).ok()?.saturating_add(1))
                .read_to_end(&mut bytes);
            *cancellation.lock().ok()? = None;
            loaded.ok()?;
            bytes
        }
        _ => return None,
    };
    (bytes.len() <= MAX_ARTWORK_BYTES).then_some(bytes)
}

fn decode_artwork(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    if bytes.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    decode_image(bytes, 1024, 8 * 1024 * 1024)
}

fn bootty_artwork() -> Option<Arc<RenderImage>> {
    static IMAGE: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            // The bundled 2048px application asset is trusted; repository files use smaller limits.
            let bytes = crate::assets::BoottyAssets
                .load("icons/bootty.png")
                .ok()
                .flatten()?;
            decode_image(&bytes, 2048, 32 * 1024 * 1024)
        })
        .clone()
}

fn decode_image(bytes: &[u8], max_extent: u32, max_alloc: u64) -> Option<Arc<RenderImage>> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_extent);
    limits.max_image_height = Some(max_extent);
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits);
    let mut pixels = reader.decode().ok()?.thumbnail(64, 64).into_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([Frame::new(pixels)])))
}
