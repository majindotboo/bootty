//! Seekable media reads reuse the owning host's bounded file reader.
use crate::{
    file_reader::{FileDescriptor, FileReader},
    remote::RemoteHost,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

pub use crate::file_reader::FileCancellation as MediaCancellation;
pub use crate::file_reader::MAX_FILE_CHUNK as MAX_MEDIA_CHUNK;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MediaDescriptor {
    pub path: String,
    pub name: String,
    /// Observed file identity and metadata, not a text-save content digest.
    pub revision: String,
    pub len: u64,
    #[serde(rename = "media_kind")]
    pub kind: MediaKind,
}

impl From<&MediaDescriptor> for FileDescriptor {
    fn from(value: &MediaDescriptor) -> Self {
        Self {
            path: value.path.clone(),
            name: value.name.clone(),
            revision: value.revision.clone(),
            len: value.len,
        }
    }
}

pub struct MediaReader(FileReader);
impl MediaReader {
    /// # Errors
    /// Returns invalid metadata, changed files, or host/transport setup errors.
    pub fn open(descriptor: &MediaDescriptor, remote: Option<&RemoteHost>) -> Result<Self> {
        FileReader::open(&descriptor.into(), remote).map(Self)
    }
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.0.len()
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    #[must_use]
    pub fn cancellation(&self) -> MediaCancellation {
        self.0.cancellation()
    }
}
impl Read for MediaReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.0.read(output)
    }
}
impl Seek for MediaReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}

/// Preserve the media wire endpoint for existing callers.
/// # Errors
/// Returns malformed requests, changed files, or stream errors.
pub fn serve(payload: &str, input: impl io::BufRead, output: impl io::Write) -> Result<()> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    anyhow::ensure!(
        payload.len() <= 32 * 1024,
        "Media request exceeds its bound"
    );
    let descriptor: MediaDescriptor = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    crate::file_reader::serve_descriptor(&(&descriptor).into(), input, output)
}

pub(crate) fn open_file(path: &str) -> Result<File> {
    let path = std::fs::canonicalize(path)?;
    crate::file_reader::open_file(
        path.to_str()
            .ok_or_else(|| anyhow::anyhow!("Media path is not UTF-8"))?,
    )
}
pub(crate) fn describe(path: &str, file: &File, kind: MediaKind) -> Result<MediaDescriptor> {
    let canonical = std::fs::canonicalize(path)?;
    let descriptor = crate::file_reader::describe(
        canonical
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Media path is not UTF-8"))?,
        file,
    )?;
    Ok(MediaDescriptor {
        path: descriptor.path,
        name: descriptor.name,
        revision: descriptor.revision,
        len: descriptor.len,
        kind,
    })
}

pub(crate) fn kind(header: &[u8]) -> Option<MediaKind> {
    if header.starts_with(b"\x89PNG\r\n\x1a\n")
        || header.starts_with(b"\xff\xd8\xff")
        || header.starts_with(b"GIF87a")
        || header.starts_with(b"GIF89a")
        || (header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP"))
    {
        Some(MediaKind::Image)
    } else if header.get(4..8) == Some(b"ftyp")
        || header.starts_with(b"\x1a\x45\xdf\xa3")
        || (header.starts_with(b"RIFF") && header.get(8..12) == Some(b"AVI "))
        || (matches!(header.get(4..8), Some(b"moov" | b"mdat" | b"wide")))
    {
        Some(MediaKind::Video)
    } else {
        None
    }
}
