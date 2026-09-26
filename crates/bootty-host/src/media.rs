//! Seekable, bounded media reads on the file's owning host.
mod transport;

use std::{
    fs::{File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::remote::RemoteHost;
use anyhow::{Context as _, Result, ensure};
use rmux_os::process_tree::ProcessTreeController;
use serde::{Deserialize, Serialize};
use transport::RemoteReader;

pub use transport::serve;

/// Maximum body of one media response, independent of the source's total size.
pub const MAX_MEDIA_CHUNK: usize = 1024 * 1024;

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

/// Cancels an open source, including a blocked remote pipe read.
#[derive(Clone)]
pub struct MediaCancellation {
    cancelled: Arc<AtomicBool>,
    process: Option<ProcessTreeController>,
}
impl MediaCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(process) = &self.process {
            let _ = process.terminate();
        }
    }
    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Media source cancelled or timed out",
            ));
        }
        Ok(())
    }
}

enum Source {
    Local(File),
    Remote(Box<RemoteReader>),
}

/// Owns an open file or persistent SSH/WSL channel. Use on an IO/decode worker.
pub struct MediaReader {
    source: Source,
    descriptor: MediaDescriptor,
    position: u64,
    cache_start: u64,
    cache: Vec<u8>,
    cancellation: MediaCancellation,
}
impl MediaReader {
    /// Open the descriptor on its captured host, rejecting a changed source.
    /// # Errors
    /// Returns invalid metadata, changed files, or host/transport setup errors.
    pub fn open(descriptor: &MediaDescriptor, remote: Option<&RemoteHost>) -> Result<Self> {
        validate_descriptor(descriptor)?;
        let (source, cancellation) = if let Some(remote) = remote {
            let reader = RemoteReader::open(descriptor, remote)?;
            let cancellation = reader.cancellation().clone();
            (Source::Remote(Box::new(reader)), cancellation)
        } else {
            let file = open_file(&descriptor.path)?;
            check_revision(&file, descriptor)?;
            (
                Source::Local(file),
                MediaCancellation {
                    cancelled: Arc::default(),
                    process: None,
                },
            )
        };
        Ok(Self {
            source,
            descriptor: descriptor.clone(),
            position: 0,
            cache_start: 0,
            cache: Vec::with_capacity(MAX_MEDIA_CHUNK),
            cancellation,
        })
    }
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.descriptor.len
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[must_use]
    pub fn cancellation(&self) -> MediaCancellation {
        self.cancellation.clone()
    }
}
impl Read for MediaReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.cancellation.check()?;
        if let Source::Local(file) = &self.source {
            check_revision(file, &self.descriptor).map_err(io::Error::other)?;
        }
        if output.is_empty() || self.position >= self.len() {
            return Ok(0);
        }
        let cached = self
            .position
            .checked_sub(self.cache_start)
            .and_then(|offset| usize::try_from(offset).ok())
            .filter(|offset| *offset < self.cache.len());
        let offset = if let Some(offset) = cached {
            offset
        } else {
            let length = usize::try_from(
                self.len()
                    .saturating_sub(self.position)
                    .min(u64::try_from(MAX_MEDIA_CHUNK).map_err(io::Error::other)?),
            )
            .map_err(io::Error::other)?;
            self.cache.resize(length, 0);
            let result = match &mut self.source {
                Source::Local(file) => {
                    read_range(file, &self.descriptor, self.position, &mut self.cache)
                }
                Source::Remote(remote) => remote.read_range(self.position, &mut self.cache),
            };
            if let Err(error) = result {
                self.cache.clear();
                self.cancellation.cancel();
                return Err(io::Error::other(error));
            }
            self.cache_start = self.position;
            0
        };
        let mut bytes = self
            .cache
            .get(offset..)
            .ok_or_else(|| io::Error::other("Invalid media cache offset"))?;
        let count = bytes.read(output)?;
        self.position = self
            .position
            .saturating_add(u64::try_from(count).map_err(io::Error::other)?);
        Ok(count)
    }
}
impl Seek for MediaReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.cancellation.check()?;
        let position = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.len()).saturating_add(i128::from(offset)),
            SeekFrom::Current(offset) => {
                i128::from(self.position).saturating_add(i128::from(offset))
            }
        };
        self.position = u64::try_from(position)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid media seek"))?;
        Ok(self.position)
    }
}

fn validate_descriptor(descriptor: &MediaDescriptor) -> Result<()> {
    ensure!(
        descriptor.path.len() <= 8192 && !descriptor.path.contains('\0'),
        "Invalid media path"
    );
    ensure!(
        descriptor.name.len() <= 8192 && descriptor.revision.len() <= 1024,
        "Invalid media metadata"
    );
    Ok(())
}

pub(crate) fn open_file(path: &str) -> Result<File> {
    ensure!(
        Path::new(path).is_absolute(),
        "Media path must be absolute on its host"
    );
    // O_NONBLOCK prevents a concurrent replacement by a FIFO from hanging open.
    #[cfg(unix)]
    let file = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(not(unix))]
    let file = {
        ensure!(
            std::fs::metadata(path)?.is_file(),
            "Media source must be a regular file"
        );
        File::open(path)?
    };
    ensure!(
        file.metadata()?.is_file(),
        "Media source must be a regular file"
    );
    Ok(file)
}

fn revision(metadata: &Metadata) -> Result<String> {
    // Metadata detects ordinary edits; strict immutable snapshots need filesystem snapshots/copies.
    let value = format!(
        "{}:{:?}:{:?}",
        metadata.len(),
        metadata.modified()?,
        metadata.created().ok()
    );
    #[cfg(unix)]
    let value = {
        use std::os::unix::fs::MetadataExt as _;
        format!(
            "{value}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        )
    };
    Ok(crate::install::checksum(value.as_bytes()))
}

pub(crate) fn describe(path: &str, file: &File, kind: MediaKind) -> Result<MediaDescriptor> {
    let metadata = file.metadata()?;
    Ok(MediaDescriptor {
        path: path.to_owned(),
        name: Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned(),
        revision: revision(&metadata)?,
        len: metadata.len(),
        kind,
    })
}

fn check_revision(file: &File, descriptor: &MediaDescriptor) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.len() == descriptor.len && revision(&metadata)? == descriptor.revision,
        "Media source changed; reopen the file"
    );
    Ok(())
}
fn read_range(
    file: &mut File,
    descriptor: &MediaDescriptor,
    offset: u64,
    bytes: &mut [u8],
) -> Result<()> {
    ensure!(
        bytes.len() <= MAX_MEDIA_CHUNK,
        "Media range exceeds its bound"
    );
    ensure!(
        offset <= descriptor.len
            && u64::try_from(bytes.len())? <= descriptor.len.saturating_sub(offset),
        "Media range is outside the file"
    );
    check_revision(file, descriptor)?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(bytes)
        .context("Media source ended before the requested range")?;
    check_revision(file, descriptor)
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
