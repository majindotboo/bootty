use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

pub const REMOTE_DAEMON_PROGRAM: &str = "bootty-daemon";
pub const REMOTE_DAEMON_PROTOCOL_VERSION: &str = "10";
pub fn remote_exec_program() -> &'static str {
    static PROGRAM: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!(
            "./.bootty/bin/bootty-daemon-{REMOTE_DAEMON_PROTOCOL_VERSION}-{}.exe",
            env!("CARGO_PKG_VERSION")
        )
    });
    &PROGRAM
}
pub const REMOTE_EXEC_SUBCOMMAND: &str = "remote-exec";
pub const REMOTE_PING_SUBCOMMAND: &str = "remote-ping";
const MAX_REMOTE_COMMAND_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) terminal: bool,
}

pub fn proxy_command_line(program: &str, args: &[String], terminal: bool) -> Result<String> {
    let args = proxy_command_args(program, args, terminal)?;
    Ok(format!("{} {}", remote_exec_program(), args.join(" ")))
}

pub fn proxy_command_args(program: &str, args: &[String], terminal: bool) -> Result<Vec<String>> {
    let payload = serde_json::to_vec(&RemoteCommand {
        program: program.to_owned(),
        args: args.to_vec(),
        terminal,
    })
    .context("encode remote command")?;
    Ok(vec![
        REMOTE_EXEC_SUBCOMMAND.to_owned(),
        URL_SAFE_NO_PAD.encode(payload),
    ])
}

pub fn decode_remote_command(payload: &str) -> Result<RemoteCommand> {
    if payload.len() > MAX_REMOTE_COMMAND_PAYLOAD {
        bail!("remote command payload is too large")
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("decode remote command payload")?;
    let command: RemoteCommand =
        serde_json::from_slice(&bytes).context("parse remote command payload")?;
    if command.program.is_empty() {
        bail!("remote command program cannot be empty")
    }
    Ok(command)
}

/// # Errors
/// Returns invalid protocol payload, command setup, or execution errors.
pub fn run_remote_command(payload: &str) -> Result<i32> {
    let command = decode_remote_command(payload)?;
    let program = if command.program == REMOTE_DAEMON_PROGRAM {
        std::env::current_exe().context("resolve Bootty daemon executable")?
    } else {
        PathBuf::from(&command.program)
    };
    let mut child = Command::new(program);
    child.args(&command.args);
    if command.terminal {
        // Host execution does not depend on the terminal engine. The terminal adapter may add
        // a vendored terminfo path when it launches an attach client.
        child.env("TERM", "xterm-256color");
    }
    let status = child
        .status()
        .with_context(|| format!("run remote command {}", command.program))?;
    Ok(status.code().unwrap_or(1))
}
