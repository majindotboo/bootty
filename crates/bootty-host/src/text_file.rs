//! Local UTF-8 file reads and conflict-checked atomic replacement for editor callers.

use std::{
    fs, io,
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
pub fn save_text_file(path: impl AsRef<Path>, contents: &str) -> Result<TextFileSaveOutcome> {
    save_text_file_inner(path.as_ref(), None, contents)
}

/// Atomically replace a text file only when it still contains the bytes loaded into the editor.
/// The target is locked before checking, so the comparison and replacement share the writer's
/// cross-process critical section.
pub fn save_text_file_if_unchanged(
    path: impl AsRef<Path>,
    original_contents: &str,
    contents: &str,
) -> Result<TextFileSaveOutcome> {
    save_text_file_inner(path.as_ref(), Some(original_contents.as_bytes()), contents)
}

fn save_text_file_inner(
    path: &Path,
    expected: Option<&[u8]>,
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
        let current = match fs::read(target.path()) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read text file {} for save", path.display()));
            }
        };
        let unchanged = current
            .as_deref()
            .map_or(expected.is_empty(), |current| current == expected);
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
