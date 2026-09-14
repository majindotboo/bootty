//! Checked clipboard-image transfer through the existing SSH daemon transport.

use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::install::checksum;
use anyhow::{Context, Result, ensure};

use crate::{CommandRunner, REMOTE_DAEMON_PROGRAM, remote::RemoteHost, require_success};

pub const MAX_CLIPBOARD_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Upload on a worker. The remote path becomes usable only after byte-count and digest checks.
/// # Errors
/// Returns image validation, file, daemon, transport, or remote receipt errors.
pub fn upload_clipboard_image(
    remote: &RemoteHost,
    path: &Path,
    runner: &impl CommandRunner,
) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_CLIPBOARD_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    validate_image(&bytes)?;
    let length = bytes.len().to_string();
    let digest = checksum(&bytes);
    remote.ensure_daemon_with(runner)?;
    let (program, args) = remote.proxy_command(
        REMOTE_DAEMON_PROGRAM,
        &["clipboard-upload".to_owned(), length, digest],
    )?;
    let output = runner.run_with_input(&program, &args, bytes)?;
    let stdout = require_success(&program, &args, output).context("upload clipboard image")?;
    let path: String = serde_json::from_str(stdout.trim()).context("read uploaded image path")?;
    ensure!(
        !path.is_empty() && !path.chars().any(char::is_control),
        "remote returned an invalid image path"
    );
    Ok(path)
}

/// Receive one complete image. Errors and truncated streams drop the private temporary file.
/// # Errors
/// Returns an error for invalid size or digest, incomplete input, or failed temporary file writes.
pub fn receive_clipboard_image(
    input: impl Read,
    expected_length: u64,
    expected_digest: &str,
    directory: &Path,
) -> Result<PathBuf> {
    ensure!(
        expected_length > 0 && expected_length <= MAX_CLIPBOARD_IMAGE_BYTES,
        "clipboard image exceeds the 64 MiB limit"
    );
    ensure!(
        expected_digest.len() == 64 && expected_digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid image digest"
    );
    let mut bytes = Vec::new();
    input
        .take(expected_length.saturating_add(1))
        .read_to_end(&mut bytes)
        .context("receive clipboard image")?;
    ensure!(
        u64::try_from(bytes.len())? == expected_length,
        "clipboard image transfer was incomplete"
    );
    validate_image(&bytes)?;
    ensure!(
        checksum(&bytes).eq_ignore_ascii_case(expected_digest),
        "clipboard image checksum mismatch"
    );
    let mut file = tempfile::Builder::new()
        .prefix("bootty-clipboard-")
        .suffix(".png")
        .tempfile_in(directory)?;
    file.write_all(&bytes).context("stage clipboard image")?;
    file.as_file().sync_all().context("flush clipboard image")?;
    let (_, path) = file.keep().context("retain clipboard image")?;
    Ok(path)
}

fn validate_image(bytes: &[u8]) -> Result<()> {
    ensure!(
        u64::try_from(bytes.len())? <= MAX_CLIPBOARD_IMAGE_BYTES,
        "clipboard image exceeds the 64 MiB limit"
    );
    ensure!(
        bytes.starts_with(PNG_SIGNATURE),
        "clipboard image is not a PNG"
    );
    Ok(())
}
