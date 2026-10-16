//! Seekable, bounded file reads on the file's owning host.
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

pub use transport::{serve, serve_descriptor};

/// Maximum body of one file response, independent of the source's total size.
pub const MAX_FILE_CHUNK: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileDescriptor {
    pub path: String,
    pub name: String,
    /// Observed file identity and metadata, not a text-save content digest.
    pub revision: String,
    pub len: u64,
}

/// How a source may change while it is read.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceChanges {
    /// Any change fails the read.
    #[default]
    Refused,
    /// An append-only log, such as a conversation transcript, may grow while it is read. Reads
    /// cover the length it had when it opened; a log that shrinks or whose end is rewritten
    /// still fails.
    Appended,
}

/// Bytes at the end of a log's opened length that must stay put while it is read.
const LOG_TAIL_BYTES: u64 = 64;

/// What an open source must still be, for every read from it.
struct SourceGuard {
    descriptor: FileDescriptor,
    changes: SourceChanges,
    /// The log's last bytes at its opened length; empty for a source that can't change.
    tail: Vec<u8>,
}
impl SourceGuard {
    /// Hold `file` to `descriptor`. A log takes its length as it opens, which can be longer
    /// than the descriptor saw.
    fn open(file: &File, descriptor: &FileDescriptor, changes: SourceChanges) -> Result<Self> {
        let descriptor = match changes {
            SourceChanges::Refused => {
                check_revision(file, descriptor)?;
                descriptor.clone()
            }
            SourceChanges::Appended => {
                let opened = describe(&descriptor.path, file)?;
                ensure!(opened.len >= descriptor.len, SOURCE_CHANGED);
                opened
            }
        };
        let tail = match changes {
            SourceChanges::Refused => Vec::new(),
            SourceChanges::Appended => log_tail(file, descriptor.len)?,
        };
        Ok(Self {
            descriptor,
            changes,
            tail,
        })
    }
    fn check(&self, file: &File) -> Result<()> {
        match self.changes {
            SourceChanges::Refused => check_revision(file, &self.descriptor),
            SourceChanges::Appended => {
                ensure!(
                    file.metadata()?.len() >= self.descriptor.len
                        && log_tail(file, self.descriptor.len)? == self.tail,
                    SOURCE_CHANGED
                );
                Ok(())
            }
        }
    }
}

/// The last bytes before `len`; callers seek before every read, so moving the cursor is safe.
fn log_tail(mut file: &File, len: u64) -> Result<Vec<u8>> {
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    let mut tail = vec![0; usize::try_from(len.saturating_sub(start))?];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut tail).context(SOURCE_CHANGED)?;
    Ok(tail)
}

const SOURCE_CHANGED: &str = "File source changed; reopen the file";

/// Cancels an open source, including a blocked remote pipe read.
#[derive(Clone)]
pub struct FileCancellation {
    cancelled: Arc<AtomicBool>,
    process: Option<ProcessTreeController>,
}
impl FileCancellation {
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
                "File source cancelled or timed out",
            ));
        }
        Ok(())
    }
}

enum Source {
    Local { file: File, guard: SourceGuard },
    Remote(Box<RemoteReader>),
}

/// Owns an open file or persistent SSH/WSL channel. Use on an IO/decode worker.
pub struct FileReader {
    source: Source,
    /// The source as opened; a log's length is the one it had then.
    descriptor: FileDescriptor,
    position: u64,
    cache_start: u64,
    cache: Vec<u8>,
    cancellation: FileCancellation,
}
impl FileReader {
    /// Open the descriptor on its captured host, rejecting a changed source.
    /// # Errors
    /// Returns invalid metadata, changed files, or host/transport setup errors.
    pub fn open(descriptor: &FileDescriptor, remote: Option<&RemoteHost>) -> Result<Self> {
        Self::open_with(descriptor, remote, SourceChanges::Refused)
    }

    /// Open an append-only log that may grow while it is read, such as a conversation
    /// transcript its agent is still writing. Reads cover the length it has as it opens.
    /// # Errors
    /// Returns invalid metadata, a log that shrank or was rewritten, or host/transport setup
    /// errors.
    pub fn open_log(descriptor: &FileDescriptor, remote: Option<&RemoteHost>) -> Result<Self> {
        Self::open_with(descriptor, remote, SourceChanges::Appended)
    }

    fn open_with(
        descriptor: &FileDescriptor,
        remote: Option<&RemoteHost>,
        changes: SourceChanges,
    ) -> Result<Self> {
        validate_descriptor(descriptor)?;
        let (source, opened, cancellation) = if let Some(remote) = remote {
            let (reader, opened) = RemoteReader::open(descriptor, changes, remote)?;
            let cancellation = reader.cancellation().clone();
            (Source::Remote(Box::new(reader)), opened, cancellation)
        } else {
            let file = open_file(&descriptor.path)?;
            let guard = SourceGuard::open(&file, descriptor, changes)?;
            let opened = guard.descriptor.clone();
            (
                Source::Local { file, guard },
                opened,
                FileCancellation {
                    cancelled: Arc::default(),
                    process: None,
                },
            )
        };
        Ok(Self {
            source,
            descriptor: opened,
            position: 0,
            cache_start: 0,
            cache: Vec::with_capacity(MAX_FILE_CHUNK),
            cancellation,
        })
    }
    /// Verify the original source still has its captured identity, even after a cached read.
    /// # Errors
    /// Returns a changed source or a closed remote reader.
    pub fn verify_source(&mut self) -> Result<()> {
        match &mut self.source {
            Source::Local { file, guard } => guard.check(file),
            Source::Remote(remote) => remote.read_range(0, &mut []),
        }
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
    pub fn cancellation(&self) -> FileCancellation {
        self.cancellation.clone()
    }
}
impl Read for FileReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.cancellation.check()?;
        if let Source::Local { file, guard } = &self.source {
            guard.check(file).map_err(io::Error::other)?;
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
                    .min(u64::try_from(MAX_FILE_CHUNK).map_err(io::Error::other)?),
            )
            .map_err(io::Error::other)?;
            self.cache.resize(length, 0);
            let result = match &mut self.source {
                Source::Local { file, guard } => {
                    read_range(file, guard, self.position, &mut self.cache)
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
            .ok_or_else(|| io::Error::other("Invalid file cache offset"))?;
        let count = bytes.read(output)?;
        self.position = self
            .position
            .saturating_add(u64::try_from(count).map_err(io::Error::other)?);
        Ok(count)
    }
}
impl Seek for FileReader {
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
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file seek"))?;
        Ok(self.position)
    }
}

fn validate_descriptor(descriptor: &FileDescriptor) -> Result<()> {
    ensure!(
        descriptor.path.len() <= 8192 && !descriptor.path.contains('\0'),
        "Invalid file path"
    );
    ensure!(
        descriptor.name.len() <= 8192 && descriptor.revision.len() <= 1024,
        "Invalid file metadata"
    );
    Ok(())
}

pub(crate) fn open_file(path: &str) -> Result<File> {
    ensure!(
        Path::new(path).is_absolute(),
        "File path must be absolute on its host"
    );
    // O_NONBLOCK prevents a concurrent replacement by a FIFO from hanging open.
    #[cfg(unix)]
    let file = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(not(unix))]
    let file = {
        ensure!(
            std::fs::metadata(path)?.is_file(),
            "File source must be a regular file"
        );
        File::open(path)?
    };
    ensure!(
        file.metadata()?.is_file(),
        "File source must be a regular file"
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

pub(crate) fn describe(path: &str, file: &File) -> Result<FileDescriptor> {
    let metadata = file.metadata()?;
    Ok(FileDescriptor {
        path: path.to_owned(),
        name: Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned(),
        revision: revision(&metadata)?,
        len: metadata.len(),
    })
}

fn check_revision(file: &File, descriptor: &FileDescriptor) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.len() == descriptor.len && revision(&metadata)? == descriptor.revision,
        SOURCE_CHANGED
    );
    Ok(())
}
fn read_range(file: &mut File, guard: &SourceGuard, offset: u64, bytes: &mut [u8]) -> Result<()> {
    let descriptor = &guard.descriptor;
    ensure!(
        bytes.len() <= MAX_FILE_CHUNK,
        "File range exceeds its bound"
    );
    ensure!(
        offset <= descriptor.len
            && u64::try_from(bytes.len())? <= descriptor.len.saturating_sub(offset),
        "File range is outside the file"
    );
    guard.check(file)?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(bytes)
        .context("File source ended before the requested range")?;
    guard.check(file)
}
