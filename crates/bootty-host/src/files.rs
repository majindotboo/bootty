//! Bounded, host-local filesystem operations shared by the app and remote daemon.

use std::{
    fs,
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
};

use anyhow::{Context as _, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::{
    CommandRunner, SystemCommandRunner,
    remote::{RemoteCommandRunner, RemoteHost},
    text_file::save_text_file_if_digest,
};

/// Base64 keeps a complete document below the control protocol's 1 MiB request/response limit.
/// Larger files need a streaming editor transport rather than larger UI-owned buffers.
pub const MAX_DOCUMENT_BYTES: usize = 512 * 1024;
pub const FILE_WIRE_LIMIT: usize = 900 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const DIRECTORY_PAGE_SIZE: usize = 200;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileRequest {
    Resolve {
        path: String,
        base: Option<String>,
    },
    List {
        path: String,
        offset: usize,
    },
    Read {
        path: String,
    },
    Save {
        path: String,
        expected_digest: String,
        content_base64: String,
    },
    Format {
        path: String,
        content_base64: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub is_symlink: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DirectoryPage {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<FileEntry>,
    pub next_offset: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FileSnapshot {
    pub path: String,
    pub name: String,
    pub digest: String,
    pub content_base64: String,
}

impl FileSnapshot {
    /// Decode bounded text document bytes.
    /// # Errors
    /// Returns invalid base64 or file size errors.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        decode_file_bytes(&self.content_base64)
    }

    /// # Errors
    /// Returns an error for invalid base64, non-UTF-8 text, binary content, or an oversized document.
    pub fn contents(&self) -> Result<String> {
        decode_document(&self.content_base64)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileResponse {
    Location {
        path: String,
        is_directory: bool,
    },
    Directory(DirectoryPage),
    Document(FileSnapshot),
    Media(crate::media::MediaDescriptor),
    Saved {
        digest: String,
        durability_warning: Option<String>,
    },
    Formatted {
        content_base64: String,
    },
}

impl FileRequest {
    /// # Errors
    /// Returns path, filesystem, revision conflict, document validation, or transport limit errors.
    pub fn execute(&self) -> Result<FileResponse> {
        let response = match self {
            Self::Resolve { path, base } => resolve_location(path, base.as_deref())?,
            Self::List { path, offset } => FileResponse::Directory(list_directory(path, *offset)?),
            Self::Read { path } => read_document(path)?,
            Self::Save {
                path,
                expected_digest,
                content_base64,
            } => {
                require_absolute(path)?;
                if expected_digest.len() != 64
                    || !expected_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    bail!("expected a SHA-256 file revision");
                }
                let contents = decode_document(content_base64)?;
                let outcome =
                    save_text_file_if_digest(path, expected_digest, MAX_DOCUMENT_BYTES, &contents)?;
                FileResponse::Saved {
                    digest: crate::install::checksum(contents.as_bytes()),
                    durability_warning: outcome.durability_warning,
                }
            }
            Self::Format {
                path,
                content_base64,
            } => {
                let path = require_absolute(path)?;
                let contents = decode_document(content_base64)?;
                let formatter = formatter_for_path(path)
                    .context("no formatter is available for this file type")?;
                let (program, args) = match formatter {
                    Formatter::Rustfmt => (
                        "rustfmt",
                        vec![
                            "--emit".into(),
                            "stdout".into(),
                            "--edition".into(),
                            "2024".into(),
                        ],
                    ),
                    Formatter::Taplo => ("taplo", vec!["fmt".into(), "-".into()]),
                    Formatter::Prettier => (
                        "prettier",
                        vec![
                            "--stdin-filepath".into(),
                            path.to_string_lossy().into_owned(),
                        ],
                    ),
                };
                let output = SystemCommandRunner
                    .run_with_input(program, &args, contents.into_bytes())
                    .with_context(|| format!("run {program} on the document host"))?;
                if !output.success {
                    bail!("{program} failed: {}", output.stderr.trim());
                }
                FileResponse::Formatted {
                    content_base64: encode_document(&output.stdout)?,
                }
            }
        };
        if serde_json::to_vec(&response)?.len() > FILE_WIRE_LIMIT {
            bail!("file response exceeds the transport limit");
        }
        Ok(response)
    }

    /// # Errors
    /// Returns request size, remote execution, response size, or decoding errors.
    pub fn execute_remote(
        &self,
        remote: &RemoteHost,
        runner: impl CommandRunner,
    ) -> Result<FileResponse> {
        let input = serde_json::to_vec(self)?;
        if input.len() > FILE_WIRE_LIMIT {
            bail!("file request exceeds the transport limit");
        }
        let runner = RemoteCommandRunner::new(remote.clone(), runner);
        let output =
            runner.run_with_input(crate::REMOTE_DAEMON_PROGRAM, &["file".to_owned()], input)?;
        if !output.success {
            bail!("remote file operation failed: {}", output.stderr.trim());
        }
        if output.stdout.len() > FILE_WIRE_LIMIT {
            bail!("remote file response exceeds the transport limit");
        }
        serde_json::from_str(&output.stdout).context("decode remote file response")
    }
}

/// Whether Bootty knows a formatter for this document type. The formatter must also be installed
/// on the document host; adding a language here requires a stdin/stdout formatter command below.
#[must_use]
pub fn can_format(path: &Path) -> bool {
    formatter_for_path(path).is_some()
}

enum Formatter {
    Rustfmt,
    Taplo,
    Prettier,
}

fn formatter_for_path(path: &Path) -> Option<Formatter> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some(Formatter::Rustfmt),
        "toml" => Some(Formatter::Taplo),
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "json" | "jsonc" | "md" | "markdown"
        | "html" | "htm" | "css" | "yaml" | "yml" => Some(Formatter::Prettier),
        _ => None,
    }
}

/// # Errors
/// Returns input, payload limit, decoding, or requested filesystem operation errors.
pub fn receive_file_request(mut input: impl std::io::Read) -> Result<FileResponse> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(u64::try_from(FILE_WIRE_LIMIT + 1)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > FILE_WIRE_LIMIT {
        bail!("file request exceeds the transport limit");
    }
    serde_json::from_slice::<FileRequest>(&bytes)
        .context("decode file request")?
        .execute()
}

/// # Errors
/// Returns an error if the document contains binary content or exceeds the editor limit.
pub fn encode_document(contents: &str) -> Result<String> {
    validate_document(contents)?;
    Ok(STANDARD.encode(contents.as_bytes()))
}

/// # Errors
/// Returns invalid base64, non-UTF-8, binary content, or document size errors.
pub fn decode_document(encoded: &str) -> Result<String> {
    let contents = String::from_utf8(decode_file_bytes(encoded)?)
        .context("only UTF-8 documents can be edited")?;
    validate_document(&contents)?;
    Ok(contents)
}

fn decode_file_bytes(encoded: &str) -> Result<Vec<u8>> {
    if encoded.len() > MAX_DOCUMENT_BYTES.div_ceil(3).saturating_mul(4) {
        bail!("file previews are limited to 512 KiB");
    }
    let bytes = STANDARD.decode(encoded).context("decode file bytes")?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        bail!("file previews are limited to 512 KiB");
    }
    Ok(bytes)
}

fn validate_document(contents: &str) -> Result<()> {
    if contents.len() > MAX_DOCUMENT_BYTES {
        bail!("documents are limited to 512 KiB");
    }
    if contents.contains('\0') {
        bail!("binary files cannot be edited as text");
    }
    Ok(())
}

fn require_absolute(path: &str) -> Result<&Path> {
    let path = Path::new(path);
    if !path.is_absolute() {
        bail!("file operations require an absolute path on the destination host");
    }
    Ok(path)
}

fn path_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("the file path is not UTF-8")
}

fn read_document(path: &str) -> Result<FileResponse> {
    let file_path = require_absolute(path)?;
    let mut file = crate::media::open_file(path)?;
    let mut header = Vec::with_capacity(32);
    (&mut file).take(32).read_to_end(&mut header)?;
    if let Some(kind) = crate::media::kind(&header) {
        return Ok(FileResponse::Media(crate::media::describe(
            path, &file, kind,
        )?));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(MAX_DOCUMENT_BYTES + 1)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        bail!("text documents are limited to 512 KiB");
    }
    validate_document(std::str::from_utf8(&bytes).context("only UTF-8 documents can be edited")?)?;
    let snapshot = FileSnapshot {
        path: path.to_owned(),
        name: file_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned(),
        digest: crate::install::checksum(&bytes),
        content_base64: STANDARD.encode(bytes),
    };
    Ok(FileResponse::Document(snapshot))
}

fn list_directory(path: &str, offset: usize) -> Result<DirectoryPage> {
    // Keep the route the user browsed. Canonicalizing a symlink here aliases its
    // children with another tree branch and makes Parent leave that branch.
    let path = require_absolute(path)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        if entries.len() == MAX_DIRECTORY_ENTRIES {
            bail!("directory exceeds 100,000 entries");
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        let is_symlink = file_type.is_symlink();
        let is_directory = file_type.is_dir() || (is_symlink && entry.path().is_dir());
        entries.push(FileEntry {
            name: entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("directory contains a non-UTF-8 filename"))?,
            path: path_string(&entry.path())?,
            is_directory,
            is_symlink,
        });
    }
    entries.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.cmp(&right.name))
    });
    let count = entries.len();
    let mut page = DirectoryPage {
        path: path_string(path)?,
        parent: path.parent().map(path_string).transpose()?,
        entries: Vec::new(),
        next_offset: None,
    };
    let mut budget = serde_json::to_vec(&page)?.len().saturating_add(1024);
    for entry in entries.into_iter().skip(offset).take(DIRECTORY_PAGE_SIZE) {
        let size = serde_json::to_vec(&entry)?.len().saturating_add(1);
        if budget.saturating_add(size) > FILE_WIRE_LIMIT {
            break;
        }
        budget = budget.saturating_add(size);
        page.entries.push(entry);
    }
    let next = offset.saturating_add(page.entries.len());
    if next < count {
        page.next_offset = Some(next);
    }
    if page.entries.is_empty() && offset < count {
        bail!("directory entry exceeds the transport limit");
    }
    Ok(page)
}

/// Stable file-host namespace, without persisting SSH arguments or credentials.
/// # Errors
/// Returns an error if the remote configuration cannot be serialized for its namespace digest.
pub fn host_identity(remote: Option<&bootty_config::config::RemoteConfig>) -> Result<String> {
    let Some(remote) = remote else {
        return Ok("local".to_owned());
    };
    let kind = if matches!(remote, bootty_config::config::RemoteConfig::Ssh(_)) {
        "ssh"
    } else {
        "wsl"
    };
    Ok(format!(
        "{kind}:{}",
        crate::install::checksum(&serde_json::to_vec(remote)?)
    ))
}

/// Resolve on the executing host; relative paths never inherit the daemon's own cwd.
fn resolve_location(value: &str, base: Option<&str>) -> Result<FileResponse> {
    anyhow::ensure!(
        !value.is_empty() && !value.chars().any(char::is_control),
        "file link is empty or contains control characters"
    );
    let path = if value.starts_with("file://") {
        let url = url::Url::parse(value).context("invalid file URL")?;
        anyhow::ensure!(
            url.host_str().is_none_or(|host| host == "localhost"),
            "file URL names a different host"
        );
        url.to_file_path()
            .map_err(|()| anyhow::anyhow!("file URL is not a path on this host"))?
    } else if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .context("host has no home directory")?;
        std::path::PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or(""))
    } else {
        std::path::PathBuf::from(value)
    };
    let path = if path.is_absolute() {
        path
    } else {
        let base = base.context("relative file links require the pane's directory")?;
        require_absolute(base)?;
        Path::new(base).join(path)
    };
    let path = fs::canonicalize(path).context("file link does not exist on this host")?;
    let metadata = fs::metadata(&path).context("inspect file link")?;
    anyhow::ensure!(
        metadata.is_file() || metadata.is_dir(),
        "file link is not a regular file or directory"
    );
    Ok(FileResponse::Location {
        path: path.to_str().context("file link is not UTF-8")?.to_owned(),
        is_directory: metadata.is_dir(),
    })
}
