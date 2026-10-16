//! The native agent catalog on disk: an index file with the catalog's own state and its
//! conversations' order, and one file per conversation beside it, so a commit rewrites only the
//! conversations that changed.
//!
//! A commit writes changed conversations first, then the index, then removes the files of
//! conversations the index no longer names; an interrupted commit leaves the previous index
//! naming files that exist.
use std::{
    collections::{BTreeSet, HashMap},
    hash::{DefaultHasher, Hash as _, Hasher as _},
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::NativeSessionRecord;

/// The largest file the catalog writes or reads: the index, or one conversation.
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// The index key under which the catalog remembers what it last wrote of the index itself.
const INDEX: &str = "";

pub struct CatalogFiles {
    index: PathBuf,
    directory: PathBuf,
    /// What each file last held, by conversation, so unchanged ones aren't written again.
    written: Mutex<HashMap<String, u64>>,
}

impl CatalogFiles {
    #[must_use]
    pub fn new(index: &Path) -> Self {
        Self {
            index: index.to_path_buf(),
            directory: index.with_extension("d"),
            written: Mutex::default(),
        }
    }

    /// Reads the conversations the index names, in its order. A file that's gone is skipped,
    /// since no commit names a file before writing it.
    /// # Errors
    /// Returns a file too large to read, or one that doesn't hold a conversation.
    pub fn read_records(&self, ids: &[String]) -> Result<Vec<NativeSessionRecord>, String> {
        let mut written = HashMap::new();
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            let bytes = match std::fs::read(self.record_path(id)) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!("The native agent catalog lost conversation {id}");
                    continue;
                }
                Err(error) => return Err(error.to_string()),
            };
            if bytes.len() > MAX_FILE_BYTES {
                return Err("A native agent conversation exceeds 16 MiB".to_owned());
            }
            let record: NativeSessionRecord =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            if record.id != *id {
                return Err("Invalid native agent catalog identity".to_owned());
            }
            written.insert(id.clone(), digest(&bytes));
            records.push(record);
        }
        self.written().extend(written);
        Ok(records)
    }

    /// Writes the conversations among `records` that changed since they were last written, then
    /// `index`, then removes the files of conversations `records` no longer has. With `only`,
    /// just those conversations are checked for changes.
    /// # Errors
    /// Returns a file too large to write, or a failed write; files written before it stay.
    pub fn write(
        &self,
        records: &[NativeSessionRecord],
        only: Option<&BTreeSet<String>>,
        index: &[u8],
    ) -> Result<(), String> {
        if index.len() > MAX_FILE_BYTES {
            return Err("Native agent catalog exceeds 16 MiB".to_owned());
        }
        // Commits are serialized by the service; a failed one keeps what was written before it.
        let mut written = self.written().clone();
        for record in records
            .iter()
            .filter(|record| only.is_none_or(|only| only.contains(&record.id)))
        {
            let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
            if bytes.len() > MAX_FILE_BYTES {
                return Err("A native agent conversation exceeds 16 MiB".to_owned());
            }
            let digest = digest(&bytes);
            if written.get(&record.id) != Some(&digest) {
                std::fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
                crate::native_service::write_private(&self.record_path(&record.id), &bytes)?;
                written.insert(record.id.clone(), digest);
            }
        }
        let index_digest = digest(index);
        if written.get(INDEX) != Some(&index_digest) {
            crate::native_service::write_private(&self.index, index)?;
            written.insert(INDEX.to_owned(), index_digest);
        }
        let kept = records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<BTreeSet<_>>();
        let gone = written
            .keys()
            .filter(|id| !id.is_empty() && !kept.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for id in gone {
            match std::fs::remove_file(self.record_path(&id)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            written.remove(&id);
        }
        *self.written() = written;
        Ok(())
    }

    fn written(&self) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
        self.written
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A conversation's file: its identity with anything unusual in a file name escaped.
    fn record_path(&self, id: &str) -> PathBuf {
        let name = id
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                    char::from(byte).to_string()
                } else {
                    format!("%{byte:02x}")
                }
            })
            .collect::<String>();
        self.directory.join(format!("{name}.json"))
    }
}

fn digest(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}
