//! Private local transport; no caller identity crosses this endpoint.

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use super::protocol::{MAX_TOOL_IMAGE_RESPONSE_BYTES, MAX_TOOL_MESSAGE_BYTES, ToolProtocol};
use crate::{AgentCommandExecutor, tool_policy::ToolLease};
pub use bootty_host::private_stdio::tool_stdio;
use bootty_host::private_stdio::{Connection, Envelope, endpoint_path};
use rmux_ipc::{LocalEndpoint, LocalListener, LocalStream};
use rmux_os::identity::IdentityResolver;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _},
    sync::Notify,
};

pub(super) struct PrivateTransport {
    pub directory: PathBuf,
    pub connection: PathBuf,
    pub name: String,
    stopped: Arc<AtomicBool>,
    wake: Arc<Notify>,
    lease: ToolLease,
}

impl PrivateTransport {
    pub(super) fn prepare(
        lease: ToolLease,
        commands: Arc<dyn AgentCommandExecutor>,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
        let name = format!("bootty_{}", nonce()?);
        // macOS Unix sockets have a short path limit; identity/config paths can be much longer.
        let parent = if cfg!(target_os = "macos") {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let directory = parent.join(format!("bt-tool-{}", name.trim_start_matches("bootty_")));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&directory)?;
        let cleanup = DirectoryCleanup(directory.clone());
        let socket = endpoint_path(&directory)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let listener = {
            let _entered = runtime.enter();
            LocalListener::bind(&LocalEndpoint::from_path(socket.clone()))?
        };
        #[cfg(unix)]
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let connection = directory.join("connection.json");
        let secret = Connection {
            socket,
            token: nonce()?,
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&connection)?;
        serde_json::to_writer(&mut file, &secret).map_err(io::Error::other)?;
        file.flush()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let worker_stop = stopped.clone();
        let worker_wake = wake.clone();
        let worker_lease = lease.clone();
        let user = IdentityResolver::current()?;
        std::thread::Builder::new()
            .name("agent-tools".into())
            .spawn(move || {
                let _cleanup = cleanup;
                let protocol = ToolProtocol::new(worker_lease);
                while !worker_stop.load(Ordering::Acquire) {
                    let accepted = runtime.block_on(async {
                        tokio::select! {
                            biased;
                            () = worker_wake.notified() => Ok(None),
                            connection = listener.accept() => connection.map(Some),
                        }
                    });
                    let Ok(Some((mut stream, peer))) = accepted else {
                        break;
                    };
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if peer.user != user {
                        continue;
                    }
                    let _ = serve_connection(
                        &runtime,
                        &mut stream,
                        &secret,
                        &protocol,
                        commands.as_ref(),
                    );
                }
            })?;
        Ok(Self {
            directory,
            connection,
            name,
            stopped,
            wake,
            lease,
        })
    }

    pub(super) fn remote_connection(&self, directory: &std::path::Path) -> Result<Vec<u8>, String> {
        let mut connection: Connection =
            serde_json::from_slice(&fs::read(&self.connection).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        connection.socket = directory.join("tools.sock");
        serde_json::to_vec(&connection).map_err(|error| error.to_string())
    }

    pub(super) fn stop(&self) {
        self.lease.revoke();
        self.stopped.store(true, Ordering::Release);
        // Waking the listener performs no blocking pipe connection on the UI thread.
        self.wake.notify_one();
    }
}

impl Drop for PrivateTransport {
    fn drop(&mut self) {
        self.stop();
    }
}

struct DirectoryCleanup(PathBuf);
impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn serve_connection(
    runtime: &tokio::runtime::Runtime,
    stream: &mut LocalStream,
    secret: &Connection,
    protocol: &ToolProtocol,
    commands: &dyn AgentCommandExecutor,
) -> io::Result<()> {
    let limit = MAX_TOOL_MESSAGE_BYTES.saturating_add(1024);
    let bytes = runtime.block_on(async {
        let mut bytes = Vec::new();
        let bounded =
            (&mut *stream).take(u64::try_from(limit.saturating_add(1)).map_err(io::Error::other)?);
        let mut reader = tokio::io::BufReader::new(bounded);
        tokio::time::timeout(Duration::from_secs(5), reader.read_until(b'\n', &mut bytes))
            .await
            .map_err(io::Error::other)??;
        if bytes.len() > limit || !bytes.ends_with(b"\n") {
            return Err(invalid_connection());
        }
        Ok(bytes)
    })?;
    let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| invalid_connection())?;
    if !same_secret(&envelope.token, &secret.token) {
        return Err(invalid_connection());
    }
    let bytes = serde_json::to_vec(&envelope.request).map_err(|_| invalid_connection())?;
    // Command execution may wait for a UI approval or use its own runtime. Keep it outside
    // this transport runtime; only socket I/O enters the reactor.
    if let Some(response) = protocol.handle(&bytes, Instant::now(), commands) {
        let mut bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_TOOL_IMAGE_RESPONSE_BYTES {
            return Err(io::Error::other("Tool response exceeds its bound"));
        }
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), stream.write_all(&bytes))
                .await
                .map_err(io::Error::other)??;
            // A named-pipe server must retain its handle until the client consumes the reply.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                rmux_ipc::wait_for_peer_close(stream),
            )
            .await;
            Ok::<_, io::Error>(())
        })?;
    }
    Ok(())
}

fn nonce() -> io::Result<String> {
    use std::fmt::Write as _;
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| io::Error::other("Tool authority randomness is unavailable"))?;
    let mut result = String::with_capacity(64);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(result)
}

fn same_secret(candidate: &str, expected: &str) -> bool {
    candidate.len() == expected.len()
        && candidate
            .bytes()
            .zip(expected.bytes())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

fn invalid_connection() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Private tool connection is invalid or unavailable",
    )
}
