use std::io::Read;

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_host::{
    CommandRunner, SystemCommandRunner,
    remote::{REMOTE_DAEMON_PROGRAM, RemoteHost, remote_daemon_failure},
};
use serde::{Deserialize, Serialize};

use crate::{
    backend::{MuxBackend, PaneCapture, PaneInput, PaneText},
    command::MuxCommand,
};

const MAX_COMMAND_PAYLOAD: usize = 1024 * 1024;
/// A 1 MiB control request, JSON-escaped once more.
const MAX_PANE_REQUEST: u64 = 8 * 1024 * 1024;

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

/// One pane operation a remote daemon runs with its own backend implementation.
///
/// It travels on the daemon's stdin as JSON, since a paste can be longer than a remote command
/// line. The daemon answers with the JSON of `Option<PaneText>`: `null` for input, the text for a
/// capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneRequest {
    Input { pane: String, input: PaneInput },
    Capture { pane: String, capture: PaneCapture },
}

impl PaneRequest {
    #[must_use]
    pub fn pane(&self) -> &str {
        match self {
            Self::Input { pane, .. } | Self::Capture { pane, .. } => pane,
        }
    }

    /// # Errors
    /// Returns the backend's input or capture error.
    pub fn run(&self, backend: &dyn MuxBackend) -> Result<Option<PaneText>> {
        match self {
            Self::Input { pane, input } => backend.send_pane_input(pane, input).map(|()| None),
            Self::Capture { pane, capture } => backend.capture_pane(pane, *capture).map(Some),
        }
    }

    /// Read one request the way a daemon receives it on stdin.
    /// # Errors
    /// Returns an error for an oversized, unreadable, or malformed request.
    pub fn read(reader: impl Read) -> Result<Self> {
        let mut bytes = Vec::new();
        reader
            .take(MAX_PANE_REQUEST + 1)
            .read_to_end(&mut bytes)
            .context("read remote pane request")?;
        if u64::try_from(bytes.len())? > MAX_PANE_REQUEST {
            bail!("remote pane request is too large")
        }
        serde_json::from_slice(&bytes).context("parse remote pane request")
    }

    /// Run this request through the remote daemon command `args`, which reads it from stdin.
    /// # Errors
    /// Returns daemon installation, transport, or backend errors, and a reply that does not
    /// answer the request. A daemon that predates the command reports it as unknown.
    pub(crate) fn send(&self, remote: &RemoteHost, args: &[String]) -> Result<Option<PaneText>> {
        remote.ensure_daemon()?;
        let (program, args) = remote.proxy_command(REMOTE_DAEMON_PROGRAM, args)?;
        let request = serde_json::to_vec(self).context("encode remote pane request")?;
        let output = SystemCommandRunner.run_with_input(&program, &args, request)?;
        if !output.success {
            bail!("{}", remote_daemon_failure(remote.host(), &output.stderr));
        }
        let reply: Option<PaneText> =
            serde_json::from_str(output.stdout.trim()).context("decode remote pane reply")?;
        match (self, &reply) {
            (Self::Input { .. }, None) | (Self::Capture { .. }, Some(_)) => Ok(reply),
            _ => bail!("the remote daemon did not answer the pane request"),
        }
    }
}
