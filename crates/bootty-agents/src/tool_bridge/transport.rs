//! Private Unix transport; no caller identity crosses this endpoint.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{self, BufReader, Write},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

use super::protocol::{MAX_TOOL_MESSAGE_BYTES, ToolProtocol};
use crate::{AgentCommandExecutor, tool_policy::ToolLease};
use bootty_host::private_stdio::read_message;
pub use bootty_host::private_stdio::tool_stdio;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    socket: PathBuf,
    token: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    token: String,
    request: Value,
}

pub(super) struct PrivateTransport {
    pub directory: PathBuf,
    pub connection: PathBuf,
    pub name: String,
    #[cfg(unix)]
    stopped: Arc<std::sync::atomic::AtomicBool>,
    lease: ToolLease,
}

impl PrivateTransport {
    pub(super) fn prepare(
        lease: ToolLease,
        commands: Arc<dyn AgentCommandExecutor>,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Self::prepare_unix(lease, commands)
        }
        #[cfg(not(unix))]
        {
            let _ = (lease, commands);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Agent tools require a private Unix transport",
            ))
        }
    }

    #[cfg(unix)]
    fn prepare_unix(lease: ToolLease, commands: Arc<dyn AgentCommandExecutor>) -> io::Result<Self> {
        use std::{
            os::unix::{
                fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _},
                net::UnixListener,
            },
            sync::atomic::{AtomicBool, Ordering},
        };
        let name = format!("bootty_{}", nonce()?);
        // macOS Unix sockets have a short path limit; identity/config paths can be much longer.
        let parent = if cfg!(target_os = "macos") {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let directory = parent.join(format!("bt-tool-{}", name.trim_start_matches("bootty_")));
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let cleanup = DirectoryCleanup(directory.clone());
        let socket = directory.join("tools.sock");
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let connection = directory.join("connection.json");
        let secret = Connection {
            socket,
            token: nonce()?,
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&connection)?;
        serde_json::to_writer(&mut file, &secret).map_err(io::Error::other)?;
        file.flush()?;
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = stopped.clone();
        let worker_lease = lease.clone();
        std::thread::spawn(move || {
            let _cleanup = cleanup;
            let protocol = ToolProtocol::new(worker_lease);
            while let Ok((mut stream, _)) = listener.accept() {
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let _ = serve_connection(&mut stream, &secret, &protocol, commands.as_ref());
            }
        });
        Ok(Self {
            directory,
            connection,
            name,
            stopped,
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
        #[cfg(unix)]
        {
            self.stopped
                .store(true, std::sync::atomic::Ordering::Release);
            // Wake accept without waiting on a command or joining a worker on the UI thread.
            let _ = std::os::unix::net::UnixStream::connect(self.directory.join("tools.sock"));
        }
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

#[cfg(unix)]
fn serve_connection(
    stream: &mut std::os::unix::net::UnixStream,
    secret: &Connection,
    protocol: &ToolProtocol,
    commands: &dyn AgentCommandExecutor,
) -> io::Result<()> {
    let timeout = Some(std::time::Duration::from_secs(5));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let bytes = read_message(
        &mut BufReader::new(&mut *stream),
        MAX_TOOL_MESSAGE_BYTES.saturating_add(1024),
    )?
    .ok_or_else(invalid_connection)?;
    let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| invalid_connection())?;
    if !same_secret(&envelope.token, &secret.token) {
        return Err(invalid_connection());
    }
    let bytes = serde_json::to_vec(&envelope.request).map_err(|_| invalid_connection())?;
    if let Some(response) = protocol.handle(&bytes, Instant::now(), commands) {
        serde_json::to_writer(&mut *stream, &response).map_err(io::Error::other)?;
        stream.write_all(b"\n")?;
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
