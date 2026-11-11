use super::protocol::WIRE_VERSION;
use anyhow::{Context as _, Result};
use bootty_config::ApplicationIdentity;
use bootty_os::ipc::{BlockingLocalStream, LocalEndpoint, LocalListener};
#[cfg(unix)]
use std::fs::File;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const SERVER_FLAG: &str = "--native-mux-server";
/// # Errors
/// Returns an account or endpoint lookup failure.
pub fn endpoint(identity: ApplicationIdentity) -> Result<LocalEndpoint> {
    endpoint_for_namespace(identity.namespace())
}
fn endpoint_for_namespace(namespace: &str) -> Result<LocalEndpoint> {
    Ok(bootty_os::ipc::endpoint_for_label(&format!(
        "{namespace}-mux-v{WIRE_VERSION}"
    ))?)
}
pub struct Binding {
    pub listener: LocalListener,
    #[cfg(unix)]
    lease: File,
}
/// How often a live server marks its lease in use. Age-based temp cleaners delete files untouched
/// for days even while locked (macOS `tmp_cleaner` after 3), so this stays well under that.
pub const LEASE_REFRESH: Duration = Duration::from_hours(1);
impl Binding {
    /// Mark the lease file in use. A cleaner that deleted it would let the next server start take
    /// this endpoint and orphan every live pane.
    /// # Errors
    /// Returns the timestamp write failure.
    pub fn refresh_lease(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        self.lease.set_modified(std::time::SystemTime::now())?;
        Ok(())
    }
    /// # Errors
    /// Returns an accept or peer credential failure.
    pub async fn accept(
        &self,
    ) -> std::io::Result<(bootty_os::ipc::LocalStream, bootty_os::ipc::PeerIdentity)> {
        self.listener.accept().await
    }
}
/// # Errors
/// Returns an invalid endpoint directory, lease or listener failure.
pub fn bind(endpoint: &LocalEndpoint) -> Result<Option<Binding>> {
    #[cfg(unix)]
    let lease = {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        let parent = endpoint
            .as_path()
            .parent()
            .context("mux endpoint has no parent")?;
        std::fs::create_dir_all(parent)?;
        let metadata = std::fs::symlink_metadata(parent)?;
        anyhow::ensure!(
            metadata.is_dir()
                && metadata.uid() == bootty_os::identity::UserIdentity::current()?.uid(),
            "mux runtime directory belongs to another account"
        );
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        let lease = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(endpoint.as_path().with_extension("lock"))?;
        match lease.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        match std::fs::remove_file(endpoint.as_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        lease
    };
    let listener = match LocalListener::bind(endpoint) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Ok(None),
        Err(e) => {
            #[cfg(windows)]
            {
                let probe = endpoint.clone();
                let live = std::thread::spawn(move || {
                    bootty_os::ipc::connect_blocking(&probe, Duration::from_millis(200)).is_ok()
                })
                .join()
                .map_err(|_| anyhow::anyhow!("native endpoint probe stopped"))?;
                if live {
                    return Ok(None);
                }
            }
            return Err(anyhow::Error::new(e).context("bind native mux local endpoint"));
        }
    };
    Ok(Some(Binding {
        listener,
        #[cfg(unix)]
        lease,
    }))
}
fn sidecar_supports_mux(binary: &std::path::Path) -> bool {
    use bootty_host::{CancellableCommandRunner, CommandCancellation, CommandRunner};
    let Some(program) = binary.to_str() else {
        return false;
    };
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(1)) else {
        return false;
    };
    CancellableCommandRunner::with_deadline(CommandCancellation::default(), deadline)
        .run(program, &["--native-mux-version".to_owned()])
        .is_ok_and(|output| output.success && output.stdout.trim() == WIRE_VERSION.to_string())
}

fn spawn(identity: ApplicationIdentity, namespace: &str) -> Result<()> {
    let executable = std::env::current_exe()?;
    let sidecar = executable.with_file_name(if cfg!(windows) {
        "bootty-daemon.exe"
    } else {
        "bootty-daemon"
    });
    let binary = std::env::var_os("BOOTTY_DAEMON_BINARY").map_or_else(
        || {
            if sidecar.is_file() && sidecar_supports_mux(&sidecar) {
                sidecar
            } else {
                executable
            }
        },
        PathBuf::from,
    );
    let mut command = Command::new(binary);
    command
        .arg(SERVER_FLAG)
        .arg(match identity {
            ApplicationIdentity::Production => "bootty",
            ApplicationIdentity::Development => "bootty-dev",
        })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if identity == ApplicationIdentity::Development {
        command.env(
            bootty_config::identity::DEVELOPMENT_NAMESPACE_ENV,
            namespace,
        );
    }
    let mut child = bootty_os::daemon::spawn_hidden(&mut command)?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
/// Connect-or-start with injected time and retry waiting for deterministic startup tests.
/// # Errors
/// Returns a launch, connection or deadline failure.
pub fn connect_or_start<T>(
    mut connect: impl FnMut() -> Result<T>,
    start: impl FnOnce() -> Result<()>,
    now: impl Fn() -> Instant,
    mut wait: impl FnMut(Duration),
    timeout: Duration,
) -> Result<T> {
    let deadline = now()
        .checked_add(timeout)
        .context("mux startup deadline overflow")?;
    if let Ok(stream) = connect() {
        return Ok(stream);
    }
    start()?;
    loop {
        match connect() {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                if now() >= deadline {
                    return Err(error.context("native mux server startup deadline expired"));
                }
            }
        }
        wait(Duration::from_millis(20));
    }
}
pub(super) fn connect(identity: ApplicationIdentity) -> Result<BlockingLocalStream> {
    connect_namespace(identity, identity.namespace())
}
fn connect_namespace(
    identity: ApplicationIdentity,
    namespace: &str,
) -> Result<BlockingLocalStream> {
    let endpoint = endpoint_for_namespace(namespace)?;
    connect_or_start(
        || {
            bootty_os::ipc::connect_blocking(&endpoint, Duration::from_millis(200))
                .map_err(Into::into)
        },
        || spawn(identity, namespace),
        Instant::now,
        std::thread::sleep,
        Duration::from_secs(5),
    )
}
/// Handle the hidden mode before parsing either executable's ordinary CLI.
/// # Errors
/// Returns identity or server startup failures.
pub fn run_embedded() -> Result<bool> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--native-mux-bridge") {
        let namespace = args
            .get(1)
            .context("native mux bridge requires a namespace")?;
        return bridge(namespace).map(|()| true);
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--native-mux-version")
    {
        println!("{WIRE_VERSION}");
        return Ok(true);
    }
    if args.first().is_none_or(|arg| arg != SERVER_FLAG) {
        return Ok(false);
    }
    let identity = args
        .get(1)
        .and_then(|arg| ApplicationIdentity::parse(arg))
        .context("native mux server requires an application identity")?;
    super::server::run(identity)?;
    Ok(true)
}

#[cfg(unix)]
fn bridge(namespace: &str) -> Result<()> {
    let identity =
        ApplicationIdentity::parse_namespace(namespace).context("invalid mux namespace")?;
    let socket = connect_namespace(identity, namespace)?;
    socket.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let socket = tokio::net::UnixStream::from_std(socket)?;
        let (mut read, mut write) = socket.into_split();
        let mut input = tokio::io::stdin();
        let mut output = tokio::io::stdout();
        tokio::select! {
            result = tokio::io::copy(&mut input, &mut write) => { result?; },
            result = tokio::io::copy(&mut read, &mut output) => { result?; },
        }
        Ok(())
    });
    // Tokio stdin uses a blocking reader. A remote EOF must not wait for another
    // client input byte before the bridge process can exit.
    runtime.shutdown_background();
    result
}
#[cfg(not(unix))]
fn bridge(_namespace: &str) -> Result<()> {
    // Limit: remote Windows needs an asynchronous authenticated named-pipe client.
    anyhow::bail!("native remote mux bridge is unsupported on this target")
}

/// The daemon supplies the local IPC seam; the host transport never owns topology.
#[must_use]
pub fn remote_connector() -> bootty_host::remote_link::MuxConnector {
    std::sync::Arc::new(|namespace| Box::pin(connect_remote(namespace)))
}
#[cfg(unix)]
async fn connect_remote(namespace: String) -> Result<bootty_host::remote_link::MuxStream> {
    let identity =
        ApplicationIdentity::parse_namespace(&namespace).context("invalid mux namespace")?;
    let socket =
        tokio::task::spawn_blocking(move || connect_namespace(identity, &namespace)).await??;
    socket.set_nonblocking(true)?;
    Ok(bootty_host::remote_link::MuxStream::new(
        tokio::net::UnixStream::from_std(socket)?,
    ))
}
#[cfg(not(unix))]
fn connect_remote(
    _namespace: String,
) -> std::future::Ready<Result<bootty_host::remote_link::MuxStream>> {
    // Limit: remote Windows needs an asynchronous authenticated named-pipe client.
    std::future::ready(Err(anyhow::anyhow!(
        "native remote mux bridge is unsupported on this target"
    )))
}
