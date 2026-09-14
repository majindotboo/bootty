//! Streaming file copies. A destination is published only after complete, verified receipt.
use super::{Job, JobStatus};
use crate::{CancellableCommandRunner, CommandCancellation, remote::RemoteHost};
use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use std::{
    fs::{File, Metadata},
    io::{BufRead as _, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, atomic::Ordering},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Upload,
    Download,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferSpec {
    pub direction: TransferDirection,
    pub local_path: String,
    pub host_path: String,
    #[serde(default = "super::default_timeout")]
    pub timeout_seconds: u32,
}
impl TransferSpec {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            self.local_path.len() <= 8192
                && self.host_path.len() <= 8192
                && !self.local_path.contains('\0')
                && !self.host_path.contains('\0'),
            "Invalid transfer paths"
        );
        ensure!(
            Path::new(&self.local_path).is_absolute(),
            "Local transfer path must be absolute"
        );
        ensure!(!self.host_path.is_empty(), "Host transfer path is required");
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferProgress {
    pub spec: TransferSpec,
    pub bytes: u64,
    pub total: Option<u64>,
    pub phase: String,
    pub sha256: Option<String>,
}
impl TransferProgress {
    pub(super) fn new(spec: TransferSpec) -> Self {
        Self {
            spec,
            bytes: 0,
            total: None,
            phase: "preparing".to_owned(),
            sha256: None,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum WireRequest {
    Upload { path: String, bytes: u64 },
    Download { path: String },
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    bytes: u64,
    sha256: String,
}

fn source(path: &str) -> Result<(File, Metadata)> {
    ensure!(
        Path::new(path).is_absolute(),
        "Source path must be absolute on its host"
    );
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "Transfer source must be a regular file"
    );
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Transfer source must be a regular file");
    ensure!(
        metadata.len() <= 1024_u64.pow(4),
        "Transfer source exceeds the 1 TiB limit"
    );
    Ok((file, metadata))
}
fn destination(path: &str) -> Result<tempfile::NamedTempFile> {
    let path = Path::new(path);
    ensure!(
        path.is_absolute(),
        "Destination path must be absolute on its host"
    );
    ensure!(
        !path.try_exists()?,
        "Destination already exists; transfers never replace files"
    );
    tempfile::Builder::new()
        .prefix(".bootty-transfer-")
        .tempfile_in(
            path.parent()
                .context("Destination needs a parent directory")?,
        )
        .context("stage destination")
}
fn unchanged(file: &File, before: &Metadata) -> Result<()> {
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "Source changed during transfer; destination was not published"
    );
    Ok(())
}
fn publish(file: tempfile::NamedTempFile, path: &str) -> Result<()> {
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|error| error.error)
        .context("publish destination without replacing an existing file")?;
    Ok(())
}
fn copy_bytes(
    mut input: impl Read,
    mut output: impl Write,
    total: u64,
    mut progress: impl FnMut(u64) -> Result<()>,
) -> Result<Receipt> {
    let mut bytes = 0_u64;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024].into_boxed_slice();
    while bytes < total {
        progress(bytes)?;
        let size = usize::try_from(
            total
                .saturating_sub(bytes)
                .min(u64::try_from(buffer.len())?),
        )?;
        let writable = buffer.get_mut(..size).context("transfer buffer capacity")?;
        let size = input.read(writable)?;
        ensure!(size > 0, "Transfer ended before all bytes arrived");
        let chunk = writable
            .get(..size)
            .context("transfer read exceeded buffer")?;
        output.write_all(chunk)?;
        hash.update(chunk);
        bytes = bytes
            .checked_add(u64::try_from(size)?)
            .context("transfer byte count overflow")?;
    }
    progress(bytes)?;
    let mut sha256 = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(sha256, "{byte:02x}")?;
    }
    Ok(Receipt { bytes, sha256 })
}
fn line<T: serde::de::DeserializeOwned>(input: &mut impl std::io::BufRead) -> Result<T> {
    let mut line = String::new();
    let size = input.take(16 * 1024).read_line(&mut line)?;
    ensure!(
        size > 0 && size < 16 * 1024 && line.ends_with('\n'),
        "Incomplete transfer response; destination completion is unconfirmed"
    );
    serde_json::from_str(&line).context("decode transfer response")
}
fn write_line(value: &impl Serialize, output: &mut impl Write) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
fn progress(job: &Job, bytes: u64, total: u64, phase: &str) -> Result<()> {
    ensure!(!job.cancel.load(Ordering::Acquire), "Transfer cancelled");
    let mut state = job
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let progress = state
        .summary
        .transfer
        .as_mut()
        .context("job has no transfer progress")?;
    let changed = progress.total != Some(total)
        || progress.phase != phase
        || bytes == total
        || bytes.saturating_sub(progress.bytes) >= 256 * 1024;
    if changed {
        progress.bytes = bytes;
        progress.total = Some(total);
        phase.clone_into(&mut progress.phase);
    }
    drop(state);
    if changed {
        job.notify();
    }
    Ok(())
}
pub(super) fn run(job: &Arc<Job>, spec: &TransferSpec, remote: Option<&RemoteHost>) -> Result<()> {
    job.status(JobStatus::Transferring);
    let receipt = if let Some(remote) = remote {
        remote_copy(job, spec, remote)?
    } else {
        let (from, to) = match spec.direction {
            TransferDirection::Upload => (&spec.local_path, &spec.host_path),
            TransferDirection::Download => (&spec.host_path, &spec.local_path),
        };
        let (mut input, metadata) = source(from)?;
        let mut output = destination(to)?;
        let receipt = copy_bytes(&mut input, &mut output, metadata.len(), |bytes| {
            progress(job, bytes, metadata.len(), "copying")
        })?;
        unchanged(&input, &metadata)?;
        progress(job, receipt.bytes, receipt.bytes, "committing")?;
        publish(output, to)?;
        receipt
    };
    {
        let mut state = job
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let progress = state
            .summary
            .transfer
            .as_mut()
            .context("job has no transfer progress")?;
        progress.bytes = receipt.bytes;
        progress.total = Some(receipt.bytes);
        "complete".clone_into(&mut progress.phase);
        progress.sha256 = Some(receipt.sha256);
        drop(state);
    }
    job.status(JobStatus::Exited {
        code: Some(0),
        signal: None,
    });
    Ok(())
}
fn remote_copy(job: &Arc<Job>, spec: &TransferSpec, remote: &RemoteHost) -> Result<Receipt> {
    let cancelled = job.clone();
    let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
        CommandCancellation::default(),
        job.deadline,
        move || cancelled.cancel.load(Ordering::Acquire),
    );
    remote.ensure_daemon_with(&runner)?;
    ensure!(
        !job.cancel.load(Ordering::Acquire),
        "Transfer cancelled before launch"
    );
    let mut local_source = if spec.direction == TransferDirection::Upload {
        Some(source(&spec.local_path)?)
    } else {
        None
    };
    let request = match &local_source {
        Some((_, metadata)) => WireRequest::Upload {
            path: spec.host_path.clone(),
            bytes: metadata.len(),
        },
        None => WireRequest::Download {
            path: spec.host_path.clone(),
        },
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&request)?);
    let (program, args) = remote.proxy_command(
        crate::REMOTE_DAEMON_PROGRAM,
        &["transfer".to_owned(), payload],
    )?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        ProcessTreeChild::spawn_with_console_window(&mut command, ConsoleWindowBehavior::Suppress)?;
    job.control(child.controller())?;
    let mut input = child.child_mut().stdin.take().context("transfer input")?;
    let mut output = BufReader::new(child.child_mut().stdout.take().context("transfer output")?);
    let mut stderr = child
        .child_mut()
        .stderr
        .take()
        .context("transfer diagnostics")?;
    let errors = std::thread::Builder::new()
        .name("transfer-errors".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            while let Ok(size) = stderr.read(&mut buffer) {
                if size == 0 {
                    break;
                }
                let keep = size.min((64 * 1024_usize).saturating_sub(bytes.len()));
                bytes.extend(buffer.iter().take(keep));
            }
            bytes
        })?;
    let result = (|| {
        if let Some((file, metadata)) = &mut local_source {
            let sent = copy_bytes(&mut *file, &mut input, metadata.len(), |bytes| {
                progress(job, bytes, metadata.len(), "uploading")
            })?;
            unchanged(file, metadata)?;
            progress(job, sent.bytes, sent.bytes, "verifying remote destination")?;
            write_line(&sent, &mut input)?;
            drop(input);
            let received: Receipt = line(&mut output)?;
            ensure!(
                sent.bytes == received.bytes && sent.sha256 == received.sha256,
                "Remote destination checksum does not match"
            );
            Ok(received)
        } else {
            drop(input);
            let total: u64 = line(&mut output)?;
            ensure!(
                total <= 1024_u64.pow(4),
                "Remote file exceeds the 1 TiB limit"
            );
            let mut staged = destination(&spec.local_path)?;
            let received = copy_bytes(&mut output, &mut staged, total, |bytes| {
                progress(job, bytes, total, "downloading")
            })?;
            let sent: Receipt = line(&mut output)?;
            ensure!(
                sent.bytes == received.bytes && sent.sha256 == received.sha256,
                "Downloaded checksum does not match"
            );
            progress(job, total, total, "committing")?;
            publish(staged, &spec.local_path)?;
            Ok(received)
        }
    })();
    let _ = child.terminate();
    let _ = child.wait();
    let errors = errors.join().unwrap_or_default();
    result.with_context(||format!("Transfer transport: {}. If completion was interrupted, inspect the destination before retrying; an existing file is never replaced.",String::from_utf8_lossy(&errors)))
}

/// Dedicated binary stream: bounded metadata, exact byte count, checksum and final receipt.
/// # Errors
/// Returns invalid metadata, file, stream, checksum, or destination publication errors.
pub fn serve_transfer(payload: &str) -> Result<()> {
    ensure!(
        payload.len() <= 32 * 1024,
        "Transfer request exceeds its bound"
    );
    let request: WireRequest = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    match request {
        WireRequest::Upload { path, bytes } => {
            ensure!(bytes <= 1024_u64.pow(4), "Upload exceeds the 1 TiB limit");
            let mut staged = destination(&path)?;
            let received = copy_bytes(&mut input, &mut staged, bytes, |_| Ok(()))?;
            let sent: Receipt = line(&mut input)?;
            ensure!(
                sent.bytes == received.bytes && sent.sha256 == received.sha256,
                "Upload checksum mismatch"
            );
            let mut end = [0; 1];
            ensure!(input.read(&mut end)? == 0, "Unexpected bytes after upload");
            drop(input);
            publish(staged, &path)?;
            write_line(&received, &mut output)
        }
        WireRequest::Download { path } => {
            drop(input);
            let (mut file, metadata) = source(&path)?;
            write_line(&metadata.len(), &mut output)?;
            let sent = copy_bytes(&mut file, &mut output, metadata.len(), |_| Ok(()))?;
            unchanged(&file, &metadata)?;
            write_line(&sent, &mut output)
        }
    }
}
