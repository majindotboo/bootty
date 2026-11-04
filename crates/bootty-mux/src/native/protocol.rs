//! Transport-neutral native mux wire protocol. All integers are big endian.
use crate::{
    command::{MuxCommand, PaneInput},
    snapshot::{MuxPaneLayout, MuxSnapshot},
};
use anyhow::{Result, ensure};
use bootty_terminal::{
    SessionLaunchConfig,
    geometry::TerminalGeometry,
    terminal_capture::{CaptureOptions, TerminalCapture},
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub const WIRE_VERSION: u32 = 6;
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
pub const FRAME_UPDATE: u8 = 5;
pub const IMAGE_CONTENT: u8 = 6;
pub const CONTROL: u8 = 1;
pub const CAPABILITIES: &[&str] = &[
    "topology-events",
    "server-frames",
    "pane-input",
    "capture",
    "respawn",
    "geometry-events",
    "server-identity",
    "configure",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("native mux wire version mismatch: client {client}, server {server}")]
pub struct IncompatibleVersion {
    pub client: u32,
    pub server: u32,
}

impl IncompatibleVersion {
    /// # Errors
    /// Returns the peer versions when the wire contracts differ.
    pub fn validate(client: u32, server: u32) -> Result<()> {
        if client != server {
            return Err(Self { client, server }.into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VtPolicy {
    pub colors: bootty_config::config::ColorConfig,
    pub cursor: bootty_terminal::terminal_engine::TerminalCursorConfig,
    pub features: bootty_terminal::terminal_engine::TerminalFeatureConfig,
    pub max_scrollback: usize,
}
impl Default for VtPolicy {
    fn default() -> Self {
        Self {
            colors: bootty_config::config::ColorConfig::default(),
            cursor: bootty_terminal::terminal_engine::TerminalCursorConfig::default(),
            features: bootty_terminal::terminal_engine::TerminalFeatureConfig::default(),
            max_scrollback: bootty_terminal::TerminalSessionConfig::default().max_scrollback,
        }
    }
}
impl VtPolicy {
    pub(super) fn live(&self) -> bootty_terminal::terminal_engine::TerminalLiveConfig {
        bootty_terminal::terminal_engine::TerminalLiveConfig {
            colors: crate::terminal_config::terminal_color_config(&self.colors),
            cursor: self.cursor,
            features: self.features,
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub workspace: String,
    #[serde(default)]
    pub launch: Option<SessionLaunchConfig>,
    #[serde(default)]
    pub vt: Option<VtPolicy>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Welcome {
    #[serde(default)]
    pub server_id: String,
    pub generation: u64,
    pub version: u32,
    pub capabilities: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PaneCommand {
    Capture {
        options: CaptureOptions,
        checkpoint: bool,
    },
    RestoreHistory(String),
    CurrentDirectory,
    LiveConfig(bootty_terminal::terminal_engine::TerminalLiveConfig),
    Prompt(Option<(u64, String, bool)>),
    Key(bootty_terminal::terminal_input_model::KeyInput),
    Mouse(bootty_terminal::terminal_input_model::MouseInput),
    Paste(String),
    Focus(bool),
    MouseWheel {
        input: bootty_terminal::terminal_input_model::MouseInput,
        delta: usize,
    },
    ScrollDelta(isize),
    ScrollTo(usize),
    SelectionBegin(bootty_terminal::terminal_engine::TerminalSelectionEvent),
    SelectionUpdate(bootty_terminal::terminal_engine::TerminalSelectionEvent),
    SelectionEnd(Option<bootty_terminal::terminal_engine::TerminalSelectionEvent>),
    EnterCopyMode,
    CopyMode(bootty_terminal::terminal_engine::TerminalCopyModeAction),
    Search {
        query: String,
        direction: bootty_terminal::terminal_engine::TerminalSearchDirection,
        options: bootty_terminal::terminal_search::TerminalSearchOptions,
    },
    FormatSelection(bootty_terminal::terminal_engine::TerminalSelectionFormat),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PaneResult {
    Captured(TerminalCapture),
    Directory(Option<String>),
    Prompt(bootty_terminal::shell_prompt::PromptSnapshot),
    Ok,
    Search(bool),
    Selection(#[serde(with = "bootty_terminal::wire_bytes::optional")] Option<Vec<u8>>),
    CopyMode(bootty_terminal::terminal_engine::TerminalCopyModeOutcome),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PaneError {
    Overloaded,
    Unavailable(String),
    CommandFailed(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Configure {
        launch: Option<SessionLaunchConfig>,
        vt: Option<VtPolicy>,
    },
    Snapshot,
    Execute(MuxCommand),
    Prepare(MuxCommand),
    Commit,
    Abort,
    Input {
        pane: String,
        input: PaneInput,
    },
    Resize {
        pane: String,
        size: TerminalGeometry,
    },
    Capture {
        pane: String,
        options: CaptureOptions,
    },
    CaptureCheckpoint {
        pane: String,
        options: CaptureOptions,
    },
    Respawn {
        pane: String,
        launch: SessionLaunchConfig,
        history: String,
    },
    AttachFrames {
        pane: String,
    },
    PaneCommand {
        pane: String,
        request_id: u64,
        command: PaneCommand,
    },
    Detach,
    SetLayout {
        session: String,
        window: String,
        layout: MuxPaneLayout,
    },
}
impl Request {
    pub(crate) const fn mutates(&self) -> bool {
        !matches!(
            self,
            Self::Snapshot
                | Self::Capture { .. }
                | Self::CaptureCheckpoint { .. }
                | Self::AttachFrames { .. }
                | Self::Detach
        )
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LaunchFailure(pub String);
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    pub generation: u64,
    pub revision: u64,
    pub snapshot: MuxSnapshot,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    PaneReply {
        request_id: u64,
        result: std::result::Result<PaneResult, PaneError>,
    },
    PaneEffect(bootty_terminal::terminal_side_effect::TerminalSideEffect),
    Snapshot(TopologySnapshot),
    Changed(TopologySnapshot),
    Captured(TerminalCapture),
    CheckpointCaptured(crate::session_snapshot::SessionPaneCapture),
    Ok,
    Error(String),
    LaunchFailed(String),
    IncompatibleVersion(IncompatibleVersion),
    Exited,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub kind: u8,
    pub bytes: Vec<u8>,
}

/// Retains partial headers/payloads across socket timeouts and idle checks.
#[derive(Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}
impl FrameReader {
    /// # Errors
    /// Returns EOF, invalid lengths or a stream failure. `None` means an idle timeout.
    pub fn poll(&mut self, reader: &mut impl Read) -> Result<Option<Frame>> {
        loop {
            if let Some(header) = self.buffer.get(..4) {
                let length = u32::from_be_bytes(header.try_into()?);
                let payload = payload_length(length)?;
                let total = payload.saturating_add(5);
                if self.buffer.len() >= total {
                    let kind = *self
                        .buffer
                        .get(4)
                        .ok_or_else(|| anyhow::anyhow!("missing mux frame kind"))?;
                    let bytes = self
                        .buffer
                        .get(5..total)
                        .ok_or_else(|| anyhow::anyhow!("missing mux payload"))?
                        .to_vec();
                    self.buffer.drain(..total);
                    return Ok(Some(Frame { kind, bytes }));
                }
            }
            let mut bytes = [0; 8192];
            match reader.read(&mut bytes) {
                Ok(0) => anyhow::bail!("native mux stream closed"),
                Ok(n) => self.buffer.extend_from_slice(
                    bytes
                        .get(..n)
                        .ok_or_else(|| anyhow::anyhow!("invalid mux read length"))?,
                ),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}
impl Frame {
    /// # Errors
    /// Returns a serialization failure.
    pub fn json(value: &impl Serialize) -> Result<Self> {
        Ok(Self {
            kind: CONTROL,
            bytes: serde_json::to_vec(value)?,
        })
    }
    /// # Errors
    /// Rejects a non-control frame or invalid JSON.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        ensure!(
            self.kind == CONTROL,
            "expected mux control frame, got {}",
            self.kind
        );
        Ok(serde_json::from_slice(&self.bytes)?)
    }
    fn length(&self) -> Result<u32> {
        let length = self
            .bytes
            .len()
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("mux frame length overflow"))?;
        ensure!(
            length <= MAX_FRAME_BYTES,
            "mux frame exceeds {MAX_FRAME_BYTES} bytes"
        );
        Ok(u32::try_from(length)?)
    }
}
fn payload_length(length: u32) -> Result<usize> {
    let length = usize::try_from(length)?;
    ensure!(
        (1..=MAX_FRAME_BYTES).contains(&length),
        "invalid mux frame length {length}"
    );
    Ok(length.saturating_sub(1))
}
/// # Errors
/// Returns an invalid frame or stream read failure.
pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<Frame> {
    let length = payload_length(reader.read_u32().await?)?;
    let kind = reader.read_u8().await?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok(Frame { kind, bytes })
}
/// # Errors
/// Returns an oversized frame or stream write failure.
pub async fn write(writer: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> Result<()> {
    writer.write_u32(frame.length()?).await?;
    writer.write_u8(frame.kind).await?;
    writer.write_all(&frame.bytes).await?;
    writer.flush().await?;
    Ok(())
}
/// # Errors
/// Returns an invalid frame or stream read failure.
pub fn read_blocking(reader: &mut impl Read) -> Result<Frame> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let length = payload_length(u32::from_be_bytes(header))?;
    let mut kind = [0];
    reader.read_exact(&mut kind)?;
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Frame {
        kind: kind[0],
        bytes,
    })
}

/// # Errors
/// Returns an oversized frame or stream write failure.
pub fn write_blocking(writer: &mut impl Write, frame: &Frame) -> Result<()> {
    writer.write_all(&frame.length()?.to_be_bytes())?;
    writer.write_all(&[frame.kind])?;
    writer.write_all(&frame.bytes)?;
    writer.flush()?;
    Ok(())
}
