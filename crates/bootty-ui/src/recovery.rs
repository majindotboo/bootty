//! Private bounded archives of rendered output, never a source of terminal input.
use anyhow::{Context as _, Result, ensure};
use bootty_agents::{AgentKind, AgentLaunch};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub const MAX_ARCHIVES: usize = 32;
pub const MAX_TEXT: usize = 256 * 1024;
const MAX_FILE: u64 = 512 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchivedAgent {
    pub provider: AgentKind,
    pub session: String,
    pub launch: AgentLaunch,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutputArchive {
    pub id: String,
    pub run: String,
    pub scope: String,
    pub session: String,
    pub pane: String,
    pub title: String,
    pub host: String,
    pub host_fingerprint: String,
    pub backend: String,
    pub saved_at_ms: u64,
    pub cols: u16,
    pub rows: u16,
    pub omitted_lines: u64,
    pub text: String,
    pub agent: Option<ArchivedAgent>,
}
impl OutputArchive {
    /// Validate archive identity, bounded metadata, output, and reusable agent arguments.
    ///
    /// # Errors
    /// Returns an error when any archive field violates these limits.
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        validate_id(&self.run)?;
        ensure!(
            self.text.len() <= MAX_TEXT,
            "archive output exceeds 256 KiB"
        );
        for value in [
            &self.scope,
            &self.session,
            &self.pane,
            &self.title,
            &self.host,
            &self.host_fingerprint,
            &self.backend,
        ] {
            ensure!(
                value.len() <= 4096 && !value.chars().any(char::is_control),
                "invalid archive metadata"
            );
        }
        if let Some(agent) = &self.agent {
            agent.launch.validate().map_err(anyhow::Error::msg)?;
            agent
                .launch
                .session_arguments(agent.provider, &agent.session, false)
                .map_err(anyhow::Error::msg)?;
            ensure!(
                agent.launch == agent.launch.retained(agent.provider),
                "archive contains non-reusable launch arguments"
            );
        }
        Ok(())
    }
    #[must_use]
    pub fn summary(&self) -> Self {
        let mut result = self.clone();
        result.text.clear();
        result
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct ArchiveListing {
    pub entries: Vec<OutputArchive>,
    pub warnings: Vec<String>,
}
pub struct ArchiveStore {
    directory: PathBuf,
    io: Mutex<()>,
    pub revision: AtomicU64,
}
impl ArchiveStore {
    #[must_use]
    pub const fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            io: Mutex::new(()),
            revision: AtomicU64::new(0),
        }
    }
    /// List recent archives and warnings about unreadable entries.
    ///
    /// # Errors
    /// Returns an error when the archive directory cannot be enumerated.
    pub fn list(&self) -> Result<ArchiveListing> {
        let _guard = self
            .io
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.read_listing()
    }
    fn read_listing(&self) -> Result<ArchiveListing> {
        let mut listing = ArchiveListing::default();
        let paths = match std::fs::read_dir(&self.directory) {
            Ok(paths) => paths,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
            Err(e) => return Err(e.into()),
        };
        for path in paths.take(1024) {
            let path = path?;
            if path.path().extension().is_none_or(|s| s != "json") {
                continue;
            }
            let id = path
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            match self.read(&id) {
                Ok(archive) => listing.entries.push(archive),
                Err(e) => listing.warnings.push(format!("{id}: {e}")),
            }
        }
        listing.entries.sort_by(|a, b| {
            b.saved_at_ms
                .cmp(&a.saved_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        if listing.entries.len() > MAX_ARCHIVES {
            listing.warnings.push(
                "Archive retention exceeds 32 entries; next save will prune older entries".into(),
            );
            listing.entries.truncate(MAX_ARCHIVES);
        }
        Ok(listing)
    }
    fn read(&self, id: &str) -> Result<OutputArchive> {
        validate_id(id)?;
        let path = self.directory.join(format!("{id}.json"));
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "archive is not a regular file"
        );
        let file = std::fs::File::open(path)?;
        ensure!(
            file.metadata()?.len() <= MAX_FILE,
            "archive exceeds 512 KiB"
        );
        let mut bytes = Vec::new();
        file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
        ensure!(
            u64::try_from(bytes.len())? <= MAX_FILE,
            "archive grew beyond limit"
        );
        let archive: OutputArchive = serde_json::from_slice(&bytes)?;
        archive.validate()?;
        ensure!(archive.id == id, "archive identity mismatch");
        Ok(archive)
    }
    /// Read and validate one archive.
    ///
    /// # Errors
    /// Rejects invalid IDs, non-files, oversized or malformed archives, and I/O failures.
    pub fn get(&self, id: &str) -> Result<OutputArchive> {
        let _guard = self
            .io
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.read(id)
    }
    /// Publish a validated archive and retain the newest entries.
    ///
    /// # Errors
    /// Returns an error for invalid or oversized archives, unsafe destinations, or failures
    /// while locking, publishing, or pruning archive files.
    pub fn save(&self, archive: &OutputArchive) -> Result<()> {
        archive.validate()?;
        let bytes = serde_json::to_vec(archive)?;
        ensure!(
            u64::try_from(bytes.len())? <= MAX_FILE,
            "serialized archive exceeds 512 KiB"
        );
        let _guard = self
            .io
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::fs::create_dir_all(&self.directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = self.directory.join(format!("{}.json", archive.id));
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(metadata.is_file(), "archive is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let target = bootty_write::WriteTarget::resolve(&path)
            .map_err(|error| anyhow::anyhow!("resolve archive target: {error:?}"))?;
        let locked = target
            .lock()
            .map_err(|error| anyhow::anyhow!("lock archive: {error:?}"))?;
        locked
            .replace(&bytes, bootty_write::NewFileMode::Private)
            .map_err(|error| anyhow::anyhow!("replace archive: {error:?}"))?;
        // Keep the newest 32, including the just-published entry. Never prune unrelated files.
        let mut records = Vec::new();
        for path in std::fs::read_dir(&self.directory)?.take(1024) {
            let path = path?;
            if path.path().extension().is_none_or(|s| s != "json") {
                continue;
            }
            let id = path
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            if let Ok(record) = self.read(&id) {
                records.push(record);
            }
        }
        records.sort_by(|a, b| {
            b.saved_at_ms
                .cmp(&a.saved_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        for record in records.into_iter().skip(MAX_ARCHIVES) {
            std::fs::remove_file(self.directory.join(format!("{}.json", record.id)))?;
        }
        self.revision.fetch_add(1, Ordering::Release);
        // Keep this archive locked until retention and its revision are published.
        drop(locked);
        Ok(())
    }
    /// Delete one archive by its validated ID.
    ///
    /// # Errors
    /// Returns an error for invalid IDs or failed removal.
    pub fn delete(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        let _guard = self
            .io
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::fs::remove_file(self.directory.join(format!("{id}.json")))
            .context("delete archive")?;
        self.revision.fetch_add(1, Ordering::Release);
        Ok(())
    }
    /// Export an archive as plain text without overwriting an existing file.
    ///
    /// # Errors
    /// Rejects relative paths, invalid archives, existing destinations, and I/O failures.
    pub fn export(&self, id: &str, path: &Path) -> Result<()> {
        ensure!(path.is_absolute(), "export needs an absolute local path");
        let archive = self.get(id)?;
        let mut file = tempfile::NamedTempFile::new_in(path.parent().context("export parent")?)?;
        write!(
            file,
            "Previous session — {}\nHost: {}\nCaptured: {} (Unix milliseconds)\nGeometry: {} × {}\nOmitted rows: {}\n\n{}",
            archive.title,
            archive.host,
            archive.saved_at_ms,
            archive.cols,
            archive.rows,
            archive.omitted_lines,
            archive.text
        )?;
        file.as_file().sync_all()?;
        file.persist_noclobber(path).map_err(|e| e.error)?;
        Ok(())
    }
}
fn validate_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid archive ID"
    );
    Ok(())
}
#[must_use]
pub fn fingerprint(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}
