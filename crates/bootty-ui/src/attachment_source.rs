//! Temporary files behind an attachment until its import finishes.
use image::{ImageFormat, ImageReader};
use std::{
    io::{Cursor, Write as _},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub enum AttachmentTemporary {
    Clipboard(Arc<tempfile::NamedTempFile>),
}

/// Smaller text remains editable; larger pastes use the ordinary file import path.
pub(crate) enum ClipboardAttachment {
    Image(Vec<u8>),
    Text(String),
}

impl ClipboardAttachment {
    pub(crate) fn from_entry(entry: &gpui_kit::ClipboardEntry) -> Result<Option<Self>, String> {
        match entry {
            gpui_kit::ClipboardEntry::Image(image) => {
                if image.bytes().len() > bootty_agents::MAX_NATIVE_ATTACHMENT_IMAGE_BYTES {
                    return Err("Image exceeds 8 MB".into());
                }
                Ok(Some(Self::Image(image.bytes().to_vec())))
            }
            gpui_kit::ClipboardEntry::String(text) if text.text().len() >= 32 * 1024 => {
                if text.text().len() > 50 * 1024 * 1024 {
                    return Err("File exceeds 50 MB".into());
                }
                Ok(Some(Self::Text(text.text().clone())))
            }
            _ => Ok(None),
        }
    }

    pub(crate) const fn is_image(&self) -> bool {
        matches!(self, Self::Image(_))
    }

    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Image(_) => "Pasted image",
            Self::Text(_) => "Pasted text",
        }
    }

    pub(crate) fn write(self) -> Result<tempfile::NamedTempFile, String> {
        match self {
            Self::Image(bytes) => clipboard_png(&bytes),
            Self::Text(text) => {
                if text.len() > 50 * 1024 * 1024 {
                    return Err("File exceeds 50 MB".into());
                }
                let mut file = tempfile::Builder::new()
                    .prefix("pasted-text-")
                    .suffix(".txt")
                    .tempfile()
                    .map_err(|_| "Could not attach the text")?;
                file.write_all(text.as_bytes())
                    .map_err(|_| "Could not attach the text")?;
                Ok(file)
            }
        }
    }
}

fn clipboard_png(bytes: &[u8]) -> Result<tempfile::NamedTempFile, String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Image exceeds 8 MB".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Unsupported image")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| "Unsupported image")?;
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| "Could not read the image")?;
    if png.get_ref().len() > 8 * 1024 * 1024 {
        return Err("Image exceeds 8 MB".into());
    }
    let mut file = tempfile::Builder::new()
        .prefix("Screenshot-")
        .suffix(".png")
        .tempfile()
        .map_err(|_| "Could not attach the image")?;
    file.write_all(png.get_ref())
        .map_err(|_| "Could not attach the image")?;
    Ok(file)
}
