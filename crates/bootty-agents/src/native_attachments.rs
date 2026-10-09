use std::{
    fmt::Write as _,
    fs,
    io::{Cursor, Read as _},
    path::{Path, PathBuf},
};

use bootty_write::{NewFileMode, WriteTarget};
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};

pub const MAX_NATIVE_ATTACHMENT_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_NATIVE_ATTACHMENT_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NATIVE_ATTACHMENT_PREVIEW_BYTES: usize = 1024 * 1024;
const MAX_NATIVE_ATTACHMENT_IMAGE_BYTES_U64: u64 = 8 * 1024 * 1024;
pub const MAX_NATIVE_SESSION_ATTACHMENTS: usize = 128;
pub const MAX_NATIVE_SESSION_ATTACHMENT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_NATIVE_PROMPT_ATTACHMENTS: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeAttachmentKind {
    Image,
    File,
}

/// Public, host-issued identity and presentation metadata. The admitted path never crosses the
/// command or transcript boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAttachmentReference {
    pub id: String,
    pub kind: NativeAttachmentKind,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// Exact inline editor spans; older messages retain their separate attachment rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_ranges: Vec<std::ops::Range<usize>>,
    #[serde(default)]
    pub pixel_width: Option<u32>,
    #[serde(default)]
    pub pixel_height: Option<u32>,
}

impl NativeAttachmentReference {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.id.len() > 256
            || self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            || self.prompt_ranges.len() > 16
            || self.name.is_empty()
            || self.name.len() > 1024
            || self.name.chars().any(char::is_control)
            || self.mime_type.is_empty()
            || self.mime_type.len() > 128
            || self.mime_type.chars().any(char::is_control)
            || self.size_bytes > MAX_NATIVE_ATTACHMENT_FILE_BYTES
        {
            return Err("Invalid native attachment reference".to_owned());
        }
        match self.kind {
            NativeAttachmentKind::Image
                if self.size_bytes == 0
                    || self.size_bytes > MAX_NATIVE_ATTACHMENT_IMAGE_BYTES_U64
                    || self
                        .pixel_width
                        .is_none_or(|width| width == 0 || width > 8192)
                    || self
                        .pixel_height
                        .is_none_or(|height| height == 0 || height > 8192)
                    || u64::from(self.pixel_width.unwrap_or_default())
                        .saturating_mul(u64::from(self.pixel_height.unwrap_or_default()))
                        > 16 * 1024 * 1024 =>
            {
                return Err("Native image reference exceeds its bounded limits".to_owned());
            }
            NativeAttachmentKind::File
                if self.pixel_width.is_some() || self.pixel_height.is_some() =>
            {
                return Err("Native file references cannot contain image dimensions".to_owned());
            }
            _ => {}
        }
        Ok(())
    }
}

pub struct NativeAttachmentStore {
    root: PathBuf,
}

pub struct StoredNativeAttachment {
    pub(super) reference: NativeAttachmentReference,
    path: PathBuf,
}

pub struct ResolvedNativeAttachment {
    pub(super) reference: NativeAttachmentReference,
    pub(super) path: PathBuf,
    pub(super) image_png: Option<Vec<u8>>,
}

impl NativeAttachmentStore {
    pub(super) fn new(catalog_path: &Path) -> Self {
        let root = catalog_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("native-attachments");
        Self { root }
    }

    pub(super) fn prepare_session(&self, session_id: &str) -> Result<PathBuf, String> {
        let directory = self.session_directory(session_id)?;
        ensure_private_directory(&self.root)?;
        ensure_private_directory(&directory)?;
        Ok(directory)
    }

    pub(super) fn copy_session(
        &self,
        source: &str,
        target: &str,
        references: &[NativeAttachmentReference],
    ) -> Result<(), String> {
        let directory = self.prepare_session(target)?;
        let copied = (|| {
            for reference in references {
                let resolved = self.resolve(source, reference)?;
                let mut source = fs::File::open(&resolved.path).map_err(|e| e.to_string())?;
                if !source.metadata().map_err(|e| e.to_string())?.is_file() {
                    return Err("A source attachment is no longer a regular file".into());
                }
                let mut bytes = Vec::new();
                source
                    .by_ref()
                    .take(reference.size_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if u64::try_from(bytes.len()).ok() != Some(reference.size_bytes) {
                    return Err("A source attachment changed while creating the side chat".into());
                }
                write_private(
                    &directory.join(format!("{}.{}", reference.id, stored_extension(reference))),
                    &bytes,
                )?;
            }
            Ok(())
        })();
        if copied.is_err() {
            _ = self.remove_session(target);
        }
        copied
    }

    pub(super) fn import(
        &self,
        session_id: &str,
        source_path: &Path,
    ) -> Result<StoredNativeAttachment, String> {
        if !source_path.is_absolute() {
            return Err("Attachment source path must be absolute".to_owned());
        }
        let source_path = fs::canonicalize(source_path).map_err(|error| error.to_string())?;
        let metadata = fs::metadata(&source_path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_NATIVE_ATTACHMENT_FILE_BYTES {
            return Err("Attach a regular file of at most 50 MiB".to_owned());
        }
        let mut source = fs::File::open(&source_path).map_err(|error| error.to_string())?;
        let capacity = usize::try_from(metadata.len().min(MAX_NATIVE_ATTACHMENT_FILE_BYTES))
            .map_err(|_| "Attachment size exceeds this host's limits")?;
        let mut bytes = Vec::with_capacity(capacity);
        source
            .by_ref()
            .take(MAX_NATIVE_ATTACHMENT_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let byte_length =
            u64::try_from(bytes.len()).map_err(|_| "Attachment size exceeds this host's limits")?;
        if byte_length > MAX_NATIVE_ATTACHMENT_FILE_BYTES {
            return Err("Attach a regular file of at most 50 MiB".to_owned());
        }

        let original_name = sanitized_name(&source_path);
        let extension = extension(&source_path);
        let id = format!("native-att-{}", crate::terminal_session_id()?);
        let (reference, stored_bytes, stored_extension) = if is_supported_image(&extension) {
            let mut reader = ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|_| "Unsupported image attachment")?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(MAX_DECODE_BYTES_U64);
            reader.limits(limits);
            let image = reader
                .decode()
                .map_err(|_| "Unsupported or oversized image attachment")?;
            if u64::from(image.width()).saturating_mul(u64::from(image.height())) > 16 * 1024 * 1024
            {
                return Err("Image attachment exceeds the pixel limit".to_owned());
            }
            let mut encoded = Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, ImageFormat::Png)
                .map_err(|_| "Image attachment could not be encoded")?;
            let stored_bytes = encoded.into_inner();
            if stored_bytes.is_empty() || stored_bytes.len() > MAX_NATIVE_ATTACHMENT_IMAGE_BYTES {
                return Err("Image attachment exceeds 8 MiB".to_owned());
            }
            (
                NativeAttachmentReference {
                    id,
                    kind: NativeAttachmentKind::Image,
                    name: original_name,
                    mime_type: "image/png".to_owned(),
                    size_bytes: u64::try_from(stored_bytes.len())
                        .map_err(|_| "Attachment size exceeds this host's limits")?,
                    prompt_ranges: Vec::new(),
                    pixel_width: Some(image.width()),
                    pixel_height: Some(image.height()),
                },
                stored_bytes,
                "png".to_owned(),
            )
        } else {
            (
                NativeAttachmentReference {
                    id,
                    kind: NativeAttachmentKind::File,
                    name: original_name,
                    mime_type: mime_type(&extension).to_owned(),
                    size_bytes: byte_length,
                    prompt_ranges: Vec::new(),
                    pixel_width: None,
                    pixel_height: None,
                },
                bytes,
                safe_extension(&extension),
            )
        };
        reference.validate()?;

        let directory = self.prepare_session(session_id)?;
        let path = directory.join(format!("{}.{}", reference.id, stored_extension));
        if path.exists() {
            return Err("Native attachment identity already exists".to_owned());
        }
        write_private(&path, &stored_bytes)?;
        Ok(StoredNativeAttachment { reference, path })
    }

    pub(super) fn resolve(
        &self,
        session_id: &str,
        reference: &NativeAttachmentReference,
    ) -> Result<ResolvedNativeAttachment, String> {
        let path = self.reference_path(session_id, reference)?;
        let image_png = if reference.kind == NativeAttachmentKind::Image {
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            crate::NativePromptImage::from_host_png(
                crate::NativeImageReference {
                    id: reference.id.clone(),
                    pixel_width: reference.pixel_width.ok_or("Native image has no width")?,
                    pixel_height: reference.pixel_height.ok_or("Native image has no height")?,
                },
                bytes.clone(),
            )?;
            Some(bytes)
        } else {
            None
        };
        Ok(ResolvedNativeAttachment {
            reference: reference.clone(),
            path,
            image_png,
        })
    }

    pub(super) fn reference_path(
        &self,
        session_id: &str,
        reference: &NativeAttachmentReference,
    ) -> Result<PathBuf, String> {
        reference.validate()?;
        ensure_existing_private_directory(&self.root)?;
        let directory = self.session_directory(session_id)?;
        ensure_existing_private_directory(&directory)?;
        let path = self.session_directory(session_id)?.join(format!(
            "{}.{}",
            reference.id,
            stored_extension(reference)
        ));
        let metadata = fs::symlink_metadata(&path).map_err(|_| "Native attachment is missing")?;
        if !metadata.file_type().is_file() || metadata.len() != reference.size_bytes {
            return Err("Native attachment is unavailable or changed".to_owned());
        }
        Ok(path)
    }

    pub(super) fn preview(
        &self,
        session_id: &str,
        reference: &NativeAttachmentReference,
    ) -> Result<Vec<u8>, String> {
        let resolved = self.resolve(session_id, reference)?;
        let bytes = resolved
            .image_png
            .ok_or_else(|| "Attachment has no image preview".to_owned())?;
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| "Native image preview is unavailable")?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(MAX_DECODE_BYTES_U64);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|_| "Native image preview is unavailable")?;
        for dimension in [1600, 800, 400, 200] {
            let thumbnail = image.thumbnail(dimension, dimension);
            let mut output = Cursor::new(Vec::new());
            thumbnail
                .write_to(&mut output, ImageFormat::Png)
                .map_err(|_| "Native image preview could not be encoded")?;
            if output.get_ref().len() <= MAX_NATIVE_ATTACHMENT_PREVIEW_BYTES {
                return Ok(output.into_inner());
            }
        }
        Err("Native image preview exceeds 1 MiB".to_owned())
    }

    pub(super) fn remove_file(attachment: &StoredNativeAttachment) {
        let _ = fs::remove_file(&attachment.path);
    }

    pub(super) fn remove_session(&self, session_id: &str) -> Result<(), String> {
        let directory = self.session_directory(session_id)?;
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err("Native attachment storage is not a directory".to_owned());
            }
            Ok(_) => {}
        }
        ensure_existing_private_directory(&self.root)?;
        ensure_existing_private_directory(&directory)?;
        match fs::remove_dir_all(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn session_directory(&self, session_id: &str) -> Result<PathBuf, String> {
        if session_id.is_empty() || session_id.len() > 256 {
            return Err("Invalid native session attachment owner".to_owned());
        }
        let mut component = String::with_capacity(session_id.len().saturating_mul(2));
        for byte in session_id.bytes() {
            let _ = write!(&mut component, "{byte:02x}");
        }
        Ok(self.root.join(component))
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() {
        return Err("Native attachment storage is not a directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn ensure_existing_private_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "Native attachment is missing")?;
    if !metadata.file_type().is_dir() {
        return Err("Native attachment storage is not a directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Native attachment storage permissions are not private".to_owned());
        }
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    WriteTarget::resolve(path)
        .map_err(|error| error.into_io().to_string())?
        .lock()
        .map_err(|error| error.to_string())?
        .replace(bytes, NewFileMode::Private)
        .map_err(|error| error.into_io().to_string())?;
    Ok(())
}

fn sanitized_name(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || "Attachment".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let name = name
        .chars()
        .filter(|character| !character.is_control())
        .take(255)
        .collect::<String>();
    if name.is_empty() {
        "Attachment".to_owned()
    } else {
        name
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_supported_image(extension: &str) -> bool {
    matches!(extension, "png" | "jpg" | "jpeg" | "webp")
}

fn safe_extension(extension: &str) -> String {
    if !extension.is_empty()
        && extension.len() <= 16
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        extension.to_owned()
    } else {
        "bin".to_owned()
    }
}

fn stored_extension(reference: &NativeAttachmentReference) -> String {
    if reference.kind == NativeAttachmentKind::Image {
        "png".to_owned()
    } else {
        let extension = Path::new(&reference.name)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        safe_extension(&extension)
    }
}

fn mime_type(extension: &str) -> &'static str {
    match extension {
        "md" | "markdown" => "text/markdown",
        "txt" => "text/plain",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "rs" => "text/x-rust",
        "pdf" => "application/pdf",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

const MAX_DECODE_BYTES_U64: u64 = 64 * 1024 * 1024;
