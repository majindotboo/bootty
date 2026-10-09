use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub(super) const LOCAL_ALPN: &[u8] = b"bootty-relay-1";
pub(super) const ALPN: &[u8] = b"bootty-remote-1";
pub(super) const SERVER_NAME: &str = "bootty.remote";
pub(super) const MAX_FRAME: usize = 1024 * 1024;
pub(super) const CHUNK: usize = 32 * 1024;
pub(super) const PROBE: u8 = 8;
pub(super) const INPUT: u8 = 1;
pub(super) const RESIZE: u8 = 2;
pub(super) const EOF: u8 = 3;
pub(super) const STDOUT: u8 = 4;
pub(super) const STDERR: u8 = 5;
pub(super) const EXIT: u8 = 6;
pub(super) const CANCEL: u8 = 7;
pub(super) type RemoteReader = Box<dyn AsyncRead + Unpin + Send>;
pub(super) type RemoteWriter = Box<dyn AsyncWrite + Unpin + Send>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteTerminalSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteProcessRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub terminal: Option<RemoteTerminalSize>,
}

impl RemoteProcessRequest {
    pub(super) fn validate(&self) -> Result<()> {
        let size = self
            .args
            .iter()
            .try_fold(self.program.len(), |size, arg| size.checked_add(arg.len()))
            .and_then(|size| size.checked_add(self.cwd.as_ref().map_or(0, String::len)));
        // Reserve JSON escaping and field overhead within the one-MiB request frame.
        if self.args.len() > CHUNK || size.is_none_or(|size| size > (MAX_FRAME - 1024) / 6) {
            bail!("remote execution request exceeds its limit");
        }
        if self.program.is_empty() || self.program.contains('\0') {
            bail!("remote program is empty or contains NUL");
        }
        if self.args.iter().any(|arg| arg.contains('\0'))
            || self.cwd.as_ref().is_some_and(|cwd| cwd.contains('\0'))
        {
            bail!("remote argument or directory contains NUL");
        }
        if self
            .terminal
            .is_some_and(|size| size.cols == 0 || size.rows == 0)
        {
            bail!("remote terminal dimensions must be positive");
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RemoteOutput {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
}

pub(super) async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    kind: u8,
    bytes: &[u8],
) -> Result<()> {
    if bytes.len() > MAX_FRAME {
        bail!("remote frame exceeds its limit");
    }
    writer.write_u8(kind).await?;
    writer.write_u32(u32::try_from(bytes.len())?).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub(super) async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<(u8, Vec<u8>)> {
    let kind = reader.read_u8().await.context("remote stream ended")?;
    let len = usize::try_from(reader.read_u32().await?)?;
    if len > MAX_FRAME {
        bail!("remote frame exceeds its limit");
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    Ok((kind, bytes))
}

pub(super) async fn write_json<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> Result<()> {
    write_frame(writer, 0, &serde_json::to_vec(value)?).await
}

pub(super) async fn read_json<T: serde::de::DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<T> {
    let (kind, bytes) = read_frame(reader).await?;
    if kind != 0 {
        bail!("expected a remote request");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
