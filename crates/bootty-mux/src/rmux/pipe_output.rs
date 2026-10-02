//! Shared live pane bytes through pipe-pane, with independent bounded readers.

use std::{
    collections::hash_map::DefaultHasher,
    fs::{File, OpenOptions},
    hash::{Hash, Hasher},
    io::{self, Read as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rmux_ipc::{LocalEndpoint, LocalListener};
use rmux_proto::{PaneTarget, PipePaneRequest, Request, Response};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{backend::rmux_request, bridge::bootty_daemon_binary};

pub(super) const PIPE_HELPER_FLAG: &str = "--__bootty-pane-pipe";
const CHUNK_BYTES: usize = 16 * 1024;
const INPUT_BACKLOG_CHUNKS: usize = 64;
// A stalled renderer may retain 16 MiB before it disconnects and recovers. Raise
// this only if supported image bursts routinely exceed that reader backlog.
const READER_BACKLOG_CHUNKS: usize = 1024;

#[cfg(unix)]
type PipeStream = tokio::net::UnixStream;
#[cfg(windows)]
type PipeStream = rmux_ipc::WindowsPipeClient;

pub(super) struct PipeOutput {
    stream: PipeStream,
}

impl PipeOutput {
    pub(super) async fn open(target: PaneTarget, pane_id: &str) -> Result<Self> {
        let mut hash = DefaultHasher::new();
        super::local::endpoint_path()?.hash(&mut hash);
        pane_id.hash(&mut hash);
        let label = format!("bp-{:016x}", hash.finish());
        let endpoint = rmux_ipc::endpoint_for_label(&label)?;
        #[cfg(unix)]
        let lock_path = endpoint.as_path().with_extension("lock");
        #[cfg(windows)]
        let lock_path = std::env::temp_dir().join(format!("{label}.lock"));
        let lock = tokio::task::spawn_blocking(move || lock_initialization(&lock_path)).await??;
        match connect(&endpoint).await {
            Ok(stream) => return Ok(Self { stream }),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) => {}
            Err(error) => return Err(error.into()),
        }
        // Only the initialization lock holder can replace a dead relay.
        #[cfg(unix)]
        if endpoint.as_path().exists() {
            std::fs::remove_file(endpoint.as_path())?;
        }
        let command = helper_command(bootty_daemon_binary()?, endpoint.as_path());
        let response = rmux_request(Request::PipePane(PipePaneRequest {
            target,
            stdin: false,
            stdout: true,
            once: false,
            command: Some(command),
        }))
        .await?;
        ensure!(
            matches!(response, Response::PipePane(_)),
            "start pane output pipe: {response:?}"
        );
        // This bounds helper startup, not live reads or the image size.
        let stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match connect(&endpoint).await {
                    Ok(stream) => break Ok::<_, anyhow::Error>(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        break Err(error.into());
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .context("pane output helper did not start")??;
        drop(lock);
        Ok(Self { stream })
    }

    pub(super) async fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buffer).await
    }
}

#[cfg(unix)]
fn helper_command(binary: &Path, endpoint: &Path) -> String {
    format!(
        "{} {PIPE_HELPER_FLAG} {}",
        bootty_host::shell_quote(&binary.to_string_lossy()),
        bootty_host::shell_quote(&endpoint.to_string_lossy()),
    )
}

#[cfg(windows)]
fn helper_command(binary: &Path, endpoint: &Path) -> String {
    use base64::Engine as _;
    // An encoded launcher works from both cmd and PowerShell pane profiles.
    let script = format!(
        "& '{}' {PIPE_HELPER_FLAG} '{}'",
        binary.to_string_lossy().replace('\'', "''"),
        endpoint.to_string_lossy().replace('\'', "''"),
    );
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn lock_initialization(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.lock()?;
    Ok(file)
}

async fn connect(endpoint: &LocalEndpoint) -> std::io::Result<PipeStream> {
    #[cfg(unix)]
    let mut stream = tokio::net::UnixStream::connect(endpoint.as_path()).await?;
    #[cfg(windows)]
    let mut stream = rmux_ipc::connect_windows_pipe(endpoint.as_pipe_name()).await?;
    ensure_ready(&mut stream).await?;
    Ok(stream)
}

async fn ensure_ready(stream: &mut PipeStream) -> std::io::Result<()> {
    if stream.read_u8().await? == 0 {
        Ok(())
    } else {
        Err(std::io::Error::other("invalid pane output handshake"))
    }
}

/// Entry point used by Bootty's pipe-pane child, also by the app when no sidecar is present.
///
/// # Errors
/// Returns an error if the local endpoint or the pipe cannot be opened or read.
pub fn run_pipe_helper(path: PathBuf) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(relay(LocalEndpoint::from_path(path)));
    // Stdin belongs to this short-lived helper. A parked stdin read must not hold
    // the process alive after its last reader disconnects.
    runtime.shutdown_background();
    result
}

async fn relay(endpoint: LocalEndpoint) -> Result<()> {
    let listener = LocalListener::bind(&endpoint)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(endpoint.as_path(), std::fs::Permissions::from_mode(0o600))?;
    }
    let _cleanup = EndpointCleanup(endpoint);
    let mut readers: Vec<(u64, tokio::sync::mpsc::Sender<Arc<[u8]>>)> = Vec::new();
    let mut writers = tokio::task::JoinSet::new();
    let mut next_reader = 0_u64;
    // Drain the daemon pipe on one reader thread instead of scheduling every
    // read through Tokio's shared blocking pool. The bounded channel retains
    // at most one MiB while the relay is handling subscribers.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(INPUT_BACKLOG_CHUNKS);
    std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buffer = vec![0; CHUNK_BYTES];
        loop {
            let bytes = match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => buffer
                    .get(..count)
                    .map(Arc::<[u8]>::from)
                    .ok_or_else(|| io::Error::other("invalid pipe read length")),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => Err(error),
            };
            let failed = bytes.is_err();
            if input_tx.blocking_send(bytes).is_err() || failed {
                break;
            }
        }
    });
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let (mut read, mut write) = tokio::io::split(stream);
                write.write_u8(0).await?;
                let id = next_reader;
                next_reader = next_reader.checked_add(1).context("pane reader id exhausted")?;
                let (chunks_tx, mut chunks_rx) = tokio::sync::mpsc::channel::<Arc<[u8]>>(READER_BACKLOG_CHUNKS);
                readers.push((id, chunks_tx));
                writers.spawn(async move {
                    tokio::select! {
                        _ = read.read_u8() => {},
                        () = async {
                            while let Some(bytes) = chunks_rx.recv().await {
                                if write.write_all(&bytes).await.is_err() { break; }
                            }
                        } => {},
                    }
                    id
                });
            }
            Some(result) = writers.join_next() => {
                let id = result?;
                readers.retain(|(reader, _)| *reader != id);
                if readers.is_empty() { break; }
            }
            bytes = input_rx.recv(), if !readers.is_empty() => {
                let Some(bytes) = bytes else {
                    // EOF follows every queued byte. Close the queues and let
                    // subscribers receive their tails before closing IPC.
                    readers.clear();
                    let _ = tokio::time::timeout(Duration::from_secs(5), async {
                        while writers.join_next().await.is_some() {}
                    }).await;
                    break;
                };
                let bytes = bytes?;
                // Never let a slow subscriber stall the shared daemon pipe. An
                // overfull subscriber gets EOF instead of silently missing bytes.
                readers.retain(|(_, writer)| writer.try_send(Arc::clone(&bytes)).is_ok());
                if readers.is_empty() { break; }
            }
        }
    }
    Ok(())
}

struct EndpointCleanup(LocalEndpoint);

impl Drop for EndpointCleanup {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = std::fs::remove_file(self.0.as_path());
    }
}
