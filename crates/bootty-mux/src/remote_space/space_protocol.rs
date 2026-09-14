use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::command::MuxCommand;

const MAX_COMMAND_PAYLOAD: usize = 1024 * 1024;
/// # Errors
/// Returns an error if command serialization fails.
pub fn encode_command(command: &MuxCommand) -> Result<String> {
    let bytes = serde_json::to_vec(command).context("encode remote Space command")?;
    if bytes.len() > MAX_COMMAND_PAYLOAD {
        bail!("remote Space command is too large")
    }
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
/// # Errors
/// Returns an error for invalid base64 or an invalid command payload.
pub fn decode_command(payload: &str) -> Result<MuxCommand> {
    if payload.len() > MAX_COMMAND_PAYLOAD * 2 {
        bail!("remote Space command is too large")
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote Space command")?;
    serde_json::from_slice(&bytes).context("parse remote Space command")
}
