//! Local UTF-8 file reads and conflict-checked atomic replacement for editor callers.

use std::{
    fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use bootty_write::{CommitOutcome, NewFileMode, WriteTarget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedTextFile {
    pub path: PathBuf,
    pub contents: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextFileSaveOutcome {
    pub durability_warning: Option<String>,
}

/// Load UTF-8 text. A missing target is a new empty document.
/// # Errors
/// Returns file reading or UTF-8 decoding errors.
pub fn load_text_file(path: impl AsRef<Path>) -> Result<LoadedTextFile> {
    let path = path.as_ref();
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("read text file {}", path.display()));
        }
    };
    Ok(LoadedTextFile {
        path: path.to_path_buf(),
        contents,
    })
}

/// Atomically replace a text file while preserving an existing target's permissions and symlink.
/// # Errors
/// Returns directory, lock, or atomic replacement errors.
pub fn save_text_file(path: impl AsRef<Path>, contents: &str) -> Result<TextFileSaveOutcome> {
    save_text_file_inner(path.as_ref(), None, contents)
}

/// Atomically replace a text file only when it still contains the bytes loaded into the editor.
///
/// The target is locked before checking, so the comparison and replacement share the writer's
/// cross-process critical section.
/// # Errors
/// Returns a revision conflict or directory, lock, or atomic replacement errors.
pub fn save_text_file_if_unchanged(
    path: impl AsRef<Path>,
    original_contents: &str,
    contents: &str,
) -> Result<TextFileSaveOutcome> {
    save_text_file_inner(
        path.as_ref(),
        Some(ExpectedRevision::Contents(original_contents.as_bytes())),
        contents,
    )
}

/// Compare a bounded document's digest under the same lease as its atomic replacement.
/// # Errors
/// Returns a revision conflict, oversized current file, or directory, lock, or replacement errors.
pub fn save_text_file_if_digest(
    path: impl AsRef<Path>,
    digest: &str,
    max_bytes: usize,
    contents: &str,
) -> Result<TextFileSaveOutcome> {
    save_text_file_inner(
        path.as_ref(),
        Some(ExpectedRevision::Digest { digest, max_bytes }),
        contents,
    )
}

enum ExpectedRevision<'a> {
    Contents(&'a [u8]),
    Digest { digest: &'a str, max_bytes: usize },
}

fn save_text_file_inner(
    path: &Path,
    expected: Option<ExpectedRevision<'_>>,
    contents: &str,
) -> Result<TextFileSaveOutcome> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("prepare text file directory {}", parent.display()))?;
    }
    let target = WriteTarget::resolve(path)
        .map_err(bootty_write::ResolveTargetError::into_io)
        .with_context(|| format!("resolve text file {}", path.display()))?
        .lock()
        .with_context(|| format!("lock text file {}", path.display()))?;
    if let Some(expected) = expected {
        let limit = match expected {
            ExpectedRevision::Contents(bytes) => bytes.len(),
            ExpectedRevision::Digest { max_bytes, .. } => max_bytes,
        };
        match fs::metadata(target.path()) {
            Ok(metadata) if !metadata.is_file() => {
                anyhow::bail!("only regular files can be saved as text")
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let current = match fs::File::open(target.path()) {
            Ok(file) => {
                if !file.metadata()?.is_file() {
                    anyhow::bail!("only regular files can be saved as text");
                }
                let mut bytes = Vec::new();
                file.take(u64::try_from(limit.saturating_add(1))?)
                    .read_to_end(&mut bytes)?;
                Some(bytes)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read text file {} for save", path.display()));
            }
        };
        let unchanged = match expected {
            ExpectedRevision::Contents(expected) => current
                .as_deref()
                .map_or(expected.is_empty(), |current| current == expected),
            ExpectedRevision::Digest { digest, max_bytes } => {
                current.as_deref().is_some_and(|current| {
                    current.len() <= max_bytes
                        && crate::install::checksum(current).eq_ignore_ascii_case(digest)
                })
            }
        };
        if !unchanged {
            return Err(anyhow::anyhow!(
                "text file {} changed on disk since it was opened",
                path.display()
            ));
        }
    }
    let outcome = target
        .replace(contents.as_bytes(), NewFileMode::Private)
        .map_err(|error| {
            let phase = error.phase();
            anyhow::anyhow!("{phase} text file {}: {}", path.display(), error.into_io())
        })?;
    drop(target);
    Ok(TextFileSaveOutcome {
        durability_warning: match outcome {
            CommitOutcome::Confirmed => None,
            CommitOutcome::CommittedWithDurabilityWarning(error) => Some(format!(
                "{} was replaced, but its directory could not be synced: {error}",
                path.display()
            )),
        },
    })
}
