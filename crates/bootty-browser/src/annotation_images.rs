use std::{fs, io::Read as _, path::Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{Annotation, AnnotationError, AnnotationStore};

const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 16 * 1024;
// Includes unreferenced objects from interrupted commits; grow this only with an owned cleanup policy.
const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;

/// An immutable store-owned image; this key is never a caller-supplied filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationImage {
    pub id: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl AnnotationImage {
    pub(crate) fn validate(&self) -> Result<(), AnnotationError> {
        if self.id.len() != 64
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.pixel_width == 0
            || self.pixel_height == 0
            || self.pixel_width > 1600
            || self.pixel_height > 1600
        {
            return Err(AnnotationError::Invalid);
        }
        Ok(())
    }
}

/// A finite rectangle in page CSS pixels or desktop points, according to its named field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl AnnotationRect {
    /// Rejects empty, unbounded and non-finite geometry from page callbacks.
    /// # Errors
    /// Returns invalid metadata for any rectangle outside the coordinate bound.
    pub fn validate(&self) -> Result<(), AnnotationError> {
        if ![self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f64::is_finite)
            || self.width <= 0.0
            || self.height <= 0.0
            || self.x.abs() > 1_000_000.0
            || self.y.abs() > 1_000_000.0
            || (self.x + self.width).abs() > 1_000_000.0
            || (self.y + self.height).abs() > 1_000_000.0
        {
            return Err(AnnotationError::Invalid);
        }
        Ok(())
    }

    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }

    #[must_use]
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then_some(Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }
}

/// Untrusted page geometry; the host independently checks document, window and recipient.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationCaptureContext {
    pub document: String,
    pub viewport: AnnotationRect,
    pub selection: AnnotationRect,
}

impl AnnotationCaptureContext {
    /// Recheck that a page callback describes the exact persisted Region or Drawing.
    /// Element geometry remains page-derived and is guarded by its unique live selector.
    /// # Errors
    /// Rejects invalid context, malformed drawings and changed selection bounds.
    pub fn validate_selection(
        &self,
        selection: Option<&crate::AnnotationSelection>,
    ) -> Result<(), AnnotationError> {
        self.validate()?;
        let expected = match selection {
            None => return Ok(()),
            Some(crate::AnnotationSelection::Region {
                x,
                y,
                width,
                height,
            }) => AnnotationRect {
                x: f64::from(*x),
                y: f64::from(*y),
                width: f64::from(*width),
                height: f64::from(*height),
            },
            Some(crate::AnnotationSelection::Drawing { points }) => {
                if !(2..=512).contains(&points.len())
                    || !points.windows(2).any(|pair| pair.first() != pair.last())
                {
                    return Err(AnnotationError::Invalid);
                }
                let Some([first_x, first_y]) = points.first() else {
                    return Err(AnnotationError::Invalid);
                };
                let (left, top, right, bottom) = points.iter().fold(
                    (*first_x, *first_y, *first_x, *first_y),
                    |(left, top, right, bottom), [x, y]| {
                        (left.min(*x), top.min(*y), right.max(*x), bottom.max(*y))
                    },
                );
                AnnotationRect {
                    x: f64::from(left),
                    y: f64::from(top),
                    width: f64::from(right.saturating_sub(left).max(1)),
                    height: f64::from(bottom.saturating_sub(top).max(1)),
                }
            }
        };
        if self.selection != expected {
            return Err(AnnotationError::Invalid);
        }
        Ok(())
    }

    /// # Errors
    /// Rejects invalid document tokens, coordinates and selections outside the visible page.
    pub fn validate(&self) -> Result<(), AnnotationError> {
        self.viewport.validate()?;
        self.selection.validate()?;
        if self.document.len() != 32
            || !self.document.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.viewport.x < 0.0
            || self.viewport.y < 0.0
            || self.viewport.intersection(&self.selection).is_none()
        {
            return Err(AnnotationError::Invalid);
        }
        Ok(())
    }
}

/// Capture-time transforms. `source` describes actual snapped pixels, never the requested crop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationImageGeometry {
    pub viewport: AnnotationRect,
    pub selection: AnnotationRect,
    pub crop: AnnotationRect,
    pub requested_source: AnnotationRect,
    pub source: AnnotationRect,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl AnnotationImageGeometry {
    fn validate(&self) -> Result<(), AnnotationError> {
        for rect in [
            &self.viewport,
            &self.selection,
            &self.crop,
            &self.requested_source,
            &self.source,
        ] {
            rect.validate()?;
        }
        if self.viewport.intersection(&self.selection).is_none()
            || !self.viewport.contains(&self.crop)
            || !self.requested_source.contains(&self.source)
            || self.pixel_width == 0
            || self.pixel_height == 0
            || self.pixel_width > 1600
            || self.pixel_height > 1600
        {
            return Err(AnnotationError::Invalid);
        }
        Ok(())
    }
}

impl AnnotationStore {
    /// Privately commit immutable image bytes before publishing an annotation reference.
    /// # Errors
    /// Rejects oversized PNGs, mismatched dimensions, unsafe aliases and a full local image store.
    pub fn commit_image(
        &self,
        png: &[u8],
        geometry: &AnnotationImageGeometry,
    ) -> Result<(AnnotationImage, Vec<bootty_write::CommitOutcome>), AnnotationError> {
        geometry.validate()?;
        validate_png(png, geometry.pixel_width, geometry.pixel_height)?;
        let metadata = serde_json::to_vec(geometry)?;
        if metadata.len() > MAX_METADATA_BYTES {
            return Err(AnnotationError::Limit);
        }
        let image = AnnotationImage {
            id: image_id(png, &metadata),
            pixel_width: geometry.pixel_width,
            pixel_height: geometry.pixel_height,
        };
        let directory = self.path.with_file_name("browser-annotation-images");
        private_directory(&directory)?;
        let mut used = 0_u64;
        for (index, entry) in fs::read_dir(&directory)?.enumerate() {
            if index >= 1024 {
                return Err(AnnotationError::Limit);
            }
            used = used
                .checked_add(entry?.metadata()?.len())
                .ok_or(AnnotationError::Limit)?;
        }
        let mut outcomes = Vec::new();
        for (extension, bytes) in [("png", png), ("json", metadata.as_slice())] {
            let path = directory.join(format!("{}.{}", image.id, extension));
            if fs::symlink_metadata(&path).is_ok() {
                if read_private(&path, bytes.len())? != bytes {
                    return Err(AnnotationError::Invalid);
                }
                continue;
            }
            used = used
                .checked_add(u64::try_from(bytes.len()).map_err(|_| AnnotationError::Limit)?)
                .filter(|used| *used <= MAX_STORE_BYTES)
                .ok_or(AnnotationError::Limit)?;
            let target = bootty_write::WriteTarget::resolve(&path)
                .map_err(|error| AnnotationError::Io(error.into_io()))?
                .lock()?;
            outcomes.push(
                target
                    .create(bytes, bootty_write::NewFileMode::Private)
                    .map_err(|error| AnnotationError::Io(error.into_io()))?,
            );
        }
        Ok((image, outcomes))
    }

    /// Resolve only an exact persisted annotation associated with the captured conversation.
    /// # Errors
    /// Refuses stale versions, detached notes and missing, corrupt or substituted image objects.
    pub fn load_image(
        &self,
        annotation: &Annotation,
        conversation: &str,
    ) -> Result<Vec<u8>, AnnotationError> {
        annotation.validate()?;
        let target = bootty_write::WriteTarget::resolve(&self.path)
            .map_err(|error| AnnotationError::Io(error.into_io()))?
            .lock()?;
        if !annotation.is_attached_to(conversation) || !self.load()?.contains(annotation) {
            return Err(AnnotationError::Conflict);
        }
        let bytes = self.image_bytes(annotation.image.as_ref().ok_or(AnnotationError::Missing)?)?;
        drop(target);
        Ok(bytes)
    }

    pub(crate) fn image_bytes(&self, image: &AnnotationImage) -> Result<Vec<u8>, AnnotationError> {
        image.validate()?;
        let directory = self.path.with_file_name("browser-annotation-images");
        if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
            return Err(AnnotationError::Invalid);
        }
        let png = read_private(
            &directory.join(format!("{}.png", image.id)),
            MAX_IMAGE_BYTES,
        )?;
        let metadata = read_private(
            &directory.join(format!("{}.json", image.id)),
            MAX_METADATA_BYTES,
        )?;
        let geometry: AnnotationImageGeometry = serde_json::from_slice(&metadata)?;
        geometry.validate()?;
        if image_id(&png, &metadata) != image.id
            || (geometry.pixel_width, geometry.pixel_height)
                != (image.pixel_width, image.pixel_height)
        {
            return Err(AnnotationError::Invalid);
        }
        validate_png(&png, image.pixel_width, image.pixel_height)?;
        Ok(png)
    }
}

fn validate_png(png: &[u8], width: u32, height: u32) -> Result<(), AnnotationError> {
    if png.len() > MAX_IMAGE_BYTES || !png.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        return Err(AnnotationError::Invalid);
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1600);
    limits.max_image_height = Some(1600);
    limits.max_alloc = Some(32 * 1024 * 1024);
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| AnnotationError::Invalid)?;
    if (decoded.width(), decoded.height()) != (width, height) {
        return Err(AnnotationError::Invalid);
    }
    Ok(())
}

fn image_id(png: &[u8], metadata: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut hash = Sha256::new();
    hash.update(b"browser-annotation-image-v1\0");
    hash.update(metadata);
    hash.update(png);
    let mut id = String::with_capacity(64);
    for byte in hash.finalize() {
        _ = write!(id, "{byte:02x}");
    }
    id
}

fn read_private(path: &Path, maximum: usize) -> Result<Vec<u8>, AnnotationError> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(AnnotationError::Invalid);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(
            u64::try_from(maximum)
                .map_err(|_| AnnotationError::Limit)?
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(AnnotationError::Limit);
    }
    Ok(bytes)
}

fn private_directory(path: &Path) -> Result<(), AnnotationError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_dir()) {
        return Err(AnnotationError::Invalid);
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
