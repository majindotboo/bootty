//! Bounded stdio forwarding into a private Unix endpoint.
#[cfg(unix)]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use serde_json::Value;
use std::{
    io::{self, BufRead, Write},
    path::Path,
};

#[cfg(unix)]
use std::{fs, io::BufReader, path::PathBuf};

pub const MAX_TOOL_MESSAGE_BYTES: usize = 1024 * 1024;
/// Response budget includes a validated PNG up to 8 MiB.
pub const MAX_TOOL_IMAGE_RESPONSE_BYTES: usize = 12 * 1024 * 1024;

#[cfg(unix)]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    socket: PathBuf,
    token: String,
}

#[cfg(unix)]
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    token: String,
    request: Value,
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
    #[cfg(unix)]
    {
        use std::os::unix::{fs::PermissionsExt as _, net::UnixStream};
        let metadata = fs::symlink_metadata(connection)?;
        let parent = connection.parent().ok_or_else(invalid_connection)?;
        let directory = fs::symlink_metadata(parent)?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.len() > 4096
            || !directory.is_dir()
            || directory.permissions().mode() & 0o077 != 0
        {
            return Err(invalid_connection());
        }
        let secret: Connection =
            serde_json::from_slice(&fs::read(connection)?).map_err(|_| invalid_connection())?;
        if secret.socket != parent.join("tools.sock") || secret.token.len() != 64 {
            return Err(invalid_connection());
        }
        while let Some(bytes) = read_message(input, MAX_TOOL_MESSAGE_BYTES)? {
            let request: Value =
                serde_json::from_slice(&bytes).map_err(|_| invalid_connection())?;
            let mut stream = UnixStream::connect(&secret.socket)?;
            let timeout = Some(std::time::Duration::from_secs(6));
            stream.set_read_timeout(timeout)?;
            stream.set_write_timeout(timeout)?;
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
    #[cfg(not(unix))]
    {
        let _ = (connection, input, output);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Agent tools require a private Unix transport",
        ))
    }
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
