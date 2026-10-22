//! Bounded stdio forwarding into a private local endpoint.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

pub const MAX_TOOL_MESSAGE_BYTES: usize = 1024 * 1024;
/// Response budget includes a validated PNG up to 8 MiB.
pub const MAX_TOOL_IMAGE_RESPONSE_BYTES: usize = 12 * 1024 * 1024;
/// The longest one tool call may take, including an app approval the user answers. MCP clients
/// commonly time tools out after 60 seconds.
pub const TOOL_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(55);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub socket: PathBuf,
    pub token: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub token: String,
    pub request: Value,
}

/// The transport for one private tool directory. Windows pipe names are local and unguessable;
/// rmux-ipc restricts both endpoints to the same Windows account.
/// # Errors
/// Rejects malformed directory names on Windows.
pub fn endpoint_path(directory: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        let nonce = directory
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("bt-tool-"))
            .filter(|nonce| {
                nonce.len() == 64
                    && nonce
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .ok_or_else(invalid_connection)?;
        Ok(PathBuf::from(format!(r"\\.\pipe\bootty-tool-{nonce}")))
    }
    #[cfg(not(windows))]
    Ok(directory.join("tools.sock"))
}

/// Forward MCP stdio from the hidden executable mode. The private connection contains the secret;
/// arguments and stdout contain only the MCP request/response, never that secret.
/// # Errors
/// Returns invalid/private-file, bounded input or retired transport errors.
pub fn tool_stdio(
    connection: &Path,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(connection)?;
    let parent = connection.parent().ok_or_else(invalid_connection)?;
    let directory = fs::symlink_metadata(parent)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 4096
        || !directory.is_dir()
        || directory.file_type().is_symlink()
    {
        return Err(invalid_connection());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 || directory.permissions().mode() & 0o077 != 0
        {
            return Err(invalid_connection());
        }
    }
    let secret: Connection =
        serde_json::from_slice(&fs::read(connection)?).map_err(|_| invalid_connection())?;
    if secret.socket != endpoint_path(parent)?
        || secret.token.len() != 64
        || !secret
            .token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_connection());
    }
    while let Some(bytes) = read_message(input, MAX_TOOL_MESSAGE_BYTES)? {
        let request: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_connection())?;
        let mut stream = rmux_ipc::connect_blocking(
            &rmux_ipc::LocalEndpoint::from_path(secret.socket.clone()),
            std::time::Duration::from_secs(5),
        )?;
        // The host answers within its own call deadline; the margin lets that answer, not
        // this read timeout, end a slow call.
        stream.set_read_timeout(Some(
            TOOL_CALL_TIMEOUT.saturating_add(std::time::Duration::from_secs(5)),
        ))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(6)))?;
        serde_json::to_writer(
            &mut stream,
            &Envelope {
                token: secret.token.clone(),
                request,
            },
        )
        .map_err(io::Error::other)?;
        stream.write_all(b"\n")?;
        if let Some(response) =
            read_message(&mut BufReader::new(stream), MAX_TOOL_IMAGE_RESPONSE_BYTES)?
        {
            output.write_all(&response)?;
            output.flush()?;
        }
    }
    Ok(())
}

/// Read one bounded newline-terminated message.
/// # Errors
/// Rejects truncated and oversized messages.
pub fn read_message(reader: &mut impl BufRead, maximum: usize) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(invalid_connection())
            };
        }
        let count = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |index| index.saturating_add(1));
        if bytes.len().saturating_add(count) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Tool message exceeds its bound",
            ));
        }
        let finished = chunk.get(count.saturating_sub(1)) == Some(&b'\n');
        bytes.extend_from_slice(chunk.get(..count).ok_or_else(invalid_connection)?);
        reader.consume(count);
        if finished {
            return Ok(Some(bytes));
        }
    }
}

fn invalid_connection() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Private tool connection is invalid or unavailable",
    )
}

pub mod relay;
