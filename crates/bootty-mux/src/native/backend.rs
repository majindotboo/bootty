//! Client facade. Only the server owns mutable native topology and pane processes.
use super::protocol::TopologySnapshot;
use super::protocol::{self, Frame, Hello, Request, Response, WIRE_VERSION, Welcome};
use crate::{command::MuxCommand, snapshot::MuxSnapshot};
use anyhow::{Context as _, Result, ensure};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub type StreamShutdown = Arc<dyn Fn() + Send + Sync>;
pub type SplitDuplex = (Box<dyn Read + Send>, Box<dyn Write + Send>, StreamShutdown);

pub trait Duplex: Read + Write + Send {
    /// # Errors
    /// Returns a transport split failure.
    fn split(self: Box<Self>) -> Result<SplitDuplex>;
    /// # Errors
    /// Returns the transport timeout configuration failure.
    fn read_timeout(&mut self, timeout: std::time::Duration) -> Result<()>;
}
struct LocalConnection(bootty_os::ipc::BlockingLocalStream);
impl Read for LocalConnection {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Write for LocalConnection {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
impl Duplex for LocalConnection {
    fn split(self: Box<Self>) -> Result<SplitDuplex> {
        #[cfg(unix)]
        {
            self.0.set_read_timeout(None)?;
            let writer = self.0.try_clone()?;
            let closer = self.0.try_clone()?;
            Ok((
                Box::new(self.0),
                Box::new(writer),
                Arc::new(move || {
                    let _ = closer.shutdown(std::net::Shutdown::Both);
                }),
            ))
        }
        #[cfg(windows)]
        {
            let (runtime, stream) = self.0.into_parts();
            Box::new(super::stream::AsyncConnection::new(
                Arc::new(runtime),
                stream,
            ))
            .split()
        }
    }
    fn read_timeout(&mut self, timeout: std::time::Duration) -> Result<()> {
        self.0.set_read_timeout(Some(timeout))?;
        Ok(())
    }
}
pub type Connector = Arc<dyn Fn() -> Result<Box<dyn Duplex>> + Send + Sync>;

pub type IdentityVerifier = Arc<dyn Fn(&str) -> Result<()> + Send + Sync>;

type Persist = Box<dyn FnOnce() -> Result<()> + Send>;
enum Job {
    Request {
        request: Request,
        deadline: Instant,
        expected: Option<u64>,
        reply: mpsc::Sender<Result<Response>>,
    },
    Persisted {
        command: MuxCommand,
        deadline: Instant,
        cancellation: bootty_control::CommandCancellation,
        expected: Option<u64>,
        persist: Persist,
        reply: mpsc::Sender<Result<TopologySnapshot>>,
    },
}
#[derive(Debug, thiserror::Error)]
#[error("native mux request expired before execution")]
pub struct RequestExpired;
#[derive(Debug, thiserror::Error)]
#[error("native mux request canceled before execution")]
pub struct RequestCanceled;
#[derive(Debug, thiserror::Error)]
#[error("native mux server changed; refresh the target before retrying")]
pub struct ServerChanged;
struct Client {
    identity_verifier: Option<IdentityVerifier>,
    retry: super::attachment::ReconnectSchedule,
    jobs: mpsc::Sender<Job>,
    connector: Connector,
    workspace: String,
    launch: Mutex<Option<bootty_terminal::SessionLaunchConfig>>,
    vt: Mutex<Option<super::protocol::VtPolicy>>,
    latest: Mutex<Option<std::result::Result<TopologySnapshot, String>>>,
    watching: AtomicBool,
    ready: Mutex<Option<std::result::Result<(), String>>>,
    ready_changed: Condvar,
    repaints: Mutex<Vec<Weak<dyn Fn() + Send + Sync>>>,
    generation: AtomicU64,
    connected: AtomicBool,
}
#[derive(Clone)]
pub struct NativeBackend {
    client: Arc<Client>,
}
fn clients() -> &'static Mutex<HashMap<PathBuf, Weak<Client>>> {
    static CLIENTS: OnceLock<Mutex<HashMap<PathBuf, Weak<Client>>>> = OnceLock::new();
    CLIENTS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn connectors() -> &'static Mutex<HashMap<PathBuf, Connector>> {
    static CONNECTORS: OnceLock<Mutex<HashMap<PathBuf, Connector>>> = OnceLock::new();
    CONNECTORS.get_or_init(|| Mutex::new(HashMap::new()))
}
impl NativeBackend {
    /// Select the transport at the host boundary, before opening this workspace.
    /// # Errors
    /// Rejects replacement while a workspace client is live.
    pub fn configure_connector(workspace: &Path, connector: Connector) -> Result<()> {
        ensure!(
            clients()
                .lock()
                .map_err(|_| anyhow::anyhow!("native client registry lock poisoned"))?
                .get(workspace)
                .and_then(Weak::upgrade)
                .is_none(),
            "native workspace client is already open"
        );
        connectors()
            .lock()
            .map_err(|_| anyhow::anyhow!("native connector registry lock poisoned"))?
            .insert(workspace.to_path_buf(), connector);
        Ok(())
    }
    #[must_use]
    pub fn new() -> Self {
        Self::for_workspace(Path::new(""))
    }
    #[must_use]
    pub fn for_workspace(workspace: &Path) -> Self {
        let mut clients = clients()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(workspace).and_then(Weak::upgrade) {
            return Self { client };
        }
        if let Some(connector) = connectors()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(workspace)
        {
            let backend = Self::with_connector(workspace, Arc::clone(connector));
            clients.insert(workspace.to_path_buf(), Arc::downgrade(&backend.client));
            return backend;
        }
        let identity = bootty_config::ApplicationIdentity::for_process();
        let backend = Self::with_connector(
            workspace,
            Arc::new(move || {
                #[cfg_attr(
                    unix,
                    expect(
                        unused_mut,
                        reason = "Windows timeout setters require a mutable stream"
                    )
                )]
                let mut stream = super::local::connect(identity)?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
                stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
                Ok(Box::new(LocalConnection(stream)))
            }),
        );
        clients.insert(workspace.to_path_buf(), Arc::downgrade(&backend.client));
        backend
    }
    #[must_use]
    pub fn with_connector(workspace: &Path, connector: Connector) -> Self {
        Self::with_identity_verifier(workspace, connector, None)
    }
    /// The verifier commits or validates identity before launch data is sent.
    #[must_use]
    pub fn with_identity_verifier(
        workspace: &Path,
        connector: Connector,
        identity_verifier: Option<IdentityVerifier>,
    ) -> Self {
        Self::with_reconnect_schedule(
            workspace,
            connector,
            identity_verifier,
            super::attachment::ReconnectSchedule::default(),
        )
    }
    #[must_use]
    pub fn with_reconnect_schedule(
        workspace: &Path,
        connector: Connector,
        identity_verifier: Option<IdentityVerifier>,
        retry: super::attachment::ReconnectSchedule,
    ) -> Self {
        let (jobs, incoming) = mpsc::channel();
        let remote = identity_verifier.is_some();
        let client = Arc::new(Client {
            identity_verifier,
            retry,
            jobs,
            connector,
            workspace: workspace.to_string_lossy().into_owned(),
            launch: Mutex::new(Some({
                let mut launch = bootty_terminal::SessionLaunchConfig::default();
                if !remote {
                    launch.capture_environment();
                }
                launch
            })),
            vt: Mutex::new(None),
            latest: Mutex::new(None),
            watching: AtomicBool::new(false),
            ready: Mutex::new(None),
            ready_changed: Condvar::new(),
            repaints: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            connected: AtomicBool::new(false),
        });
        let owner = Arc::downgrade(&client);
        thread::spawn(move || {
            while let Ok(job) = incoming.recv() {
                let Some(client) = owner.upgrade() else {
                    break;
                };
                job.run(&Self { client });
            }
        });
        Self { client }
    }
    #[must_use]
    pub(crate) fn for_remote(
        workspace: &Path,
        scope: crate::controller::SpaceId,
        space_tag: &str,
        remote: &bootty_config::config::SshRemoteConfig,
    ) -> Self {
        let key = workspace.join(format!("remote-{}-{remote:?}", scope.persistence_value()));
        let mut clients = clients()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(&key).and_then(Weak::upgrade) {
            return Self { client };
        }
        let config = workspace.to_path_buf();
        let verify: IdentityVerifier = Arc::new(move |server_id| {
            crate::repository::WorkspaceRepository::pin_remote_server(&config, scope, server_id)
        });
        let backend = Self::with_identity_verifier(
            Path::new(&format!("space:{space_tag}")),
            super::remote::connector(
                remote.clone(),
                format!("space:{space_tag}"),
                Arc::clone(&verify),
            ),
            Some(verify),
        );
        clients.insert(key, Arc::downgrade(&backend.client));
        backend
    }
    pub(super) fn check_generation(&self, expected: Option<u64>) -> Result<()> {
        if expected.is_some_and(|expected| Some(expected) != self.peer_generation()) {
            return Err(ServerChanged.into());
        }
        Ok(())
    }
    pub(crate) fn peer_generation(&self) -> Option<u64> {
        self.client
            .connected
            .load(Ordering::Acquire)
            .then(|| self.client.generation.load(Ordering::Acquire))
    }
    pub(crate) fn launch_config(&self) -> bootty_terminal::SessionLaunchConfig {
        self.client
            .launch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_default()
    }
    pub fn configure_vt(&self, policy: super::protocol::VtPolicy) {
        *self
            .client
            .vt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(policy);
    }
    pub fn configure_launch(&self, mut launch: bootty_terminal::SessionLaunchConfig) {
        if self.client.identity_verifier.is_none() {
            launch.capture_environment();
        } else {
            launch.base_environment = None;
        }
        *self
            .client
            .launch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(launch);
    }
    /// # Errors
    /// Returns transport or handshake failures.
    pub fn connect(&self) -> Result<Box<dyn Duplex>> {
        let mut stream = (self.client.connector)()?;
        protocol::write_blocking(
            &mut stream,
            &Frame::json(&Hello {
                version: WIRE_VERSION,
                workspace: self.client.workspace.clone(),
                vt: None,
                launch: None,
            })?,
        )?;
        let frame = protocol::read_blocking(&mut stream)?;
        if let Ok(Response::Error(error)) = frame.decode::<Response>() {
            anyhow::bail!(error);
        }
        if let Ok(Response::IncompatibleVersion(error)) = frame.decode::<Response>() {
            return Err(error.into());
        }
        let welcome: Welcome = frame.decode()?;
        validate_welcome(&welcome, self.client.identity_verifier.as_ref())?;
        protocol::write_blocking(
            &mut stream,
            &Frame::json(&Request::Configure {
                launch: self
                    .client
                    .launch
                    .lock()
                    .map_err(|_| anyhow::anyhow!("native launch policy poisoned"))?
                    .clone(),
                vt: self
                    .client
                    .vt
                    .lock()
                    .map_err(|_| anyhow::anyhow!("native VT policy poisoned"))?
                    .clone(),
            })?,
        )?;
        ensure!(
            matches!(Self::response(&mut stream)?, Response::Ok),
            "native mux configuration rejected"
        );
        if self
            .client
            .generation
            .swap(welcome.generation, Ordering::AcqRel)
            != welcome.generation
        {
            *self
                .client
                .latest
                .lock()
                .map_err(|_| anyhow::anyhow!("native snapshot lock poisoned"))? = None;
        }
        self.client.connected.store(true, Ordering::Release);
        Ok(stream)
    }
    /// # Errors
    /// Returns the server's typed wire error or a transport failure. Mutations are never retried.
    pub fn request(&self, request: &Request) -> Result<Response> {
        self.submit_request(request.clone())
            .recv()
            .context("native mux client worker stopped")?
    }
    #[must_use]
    pub fn submit_request(&self, request: Request) -> mpsc::Receiver<Result<Response>> {
        self.submit_request_for_generation(request, self.peer_generation())
    }
    pub(crate) fn submit_request_for_generation(
        &self,
        request: Request,
        expected: Option<u64>,
    ) -> mpsc::Receiver<Result<Response>> {
        let (reply, rx) = mpsc::channel();
        let Some(deadline) = Instant::now().checked_add(Duration::from_secs(30)) else {
            let _ = reply.send(Err(RequestExpired.into()));
            return rx;
        };
        if let Err(error) = self.client.jobs.send(Job::Request {
            request,
            deadline,
            expected,
            reply,
        }) {
            error.0.fail("native mux client worker stopped");
        }
        rx
    }
    pub(crate) fn submit_persisted(
        &self,
        command: MuxCommand,
        persist: impl FnOnce() -> Result<()> + Send + 'static,
        deadline: Instant,
        cancellation: bootty_control::CommandCancellation,
        expected: Option<u64>,
    ) -> mpsc::Receiver<Result<TopologySnapshot>> {
        let (reply, rx) = mpsc::channel();
        if let Err(error) = self.client.jobs.send(Job::Persisted {
            command,
            persist: Box::new(persist),
            deadline,
            cancellation,
            expected,
            reply,
        }) {
            error.0.fail("native mux client worker stopped");
        }
        rx
    }
    fn request_direct(&self, request: &Request, expected: Option<u64>) -> Result<Response> {
        let mut request = request.clone();
        if let Request::Respawn { launch, .. } = &mut request {
            if self.client.identity_verifier.is_none() {
                launch.capture_environment();
            } else {
                launch.base_environment = None;
            }
        }
        let mut stream = self.connect()?;
        if request.mutates() {
            self.check_generation(expected)?;
        }
        protocol::write_blocking(&mut stream, &Frame::json(&request)?)?;
        let response = Self::response(&mut stream)?;
        if let Response::Snapshot(snapshot) = &response {
            self.client.publish(snapshot.clone())?;
        }
        Ok(response)
    }
    pub(super) fn response(stream: &mut Box<dyn Duplex>) -> Result<Response> {
        loop {
            let response: Response = protocol::read_blocking(stream)?.decode()?;
            match response {
                Response::Changed(_) => {}
                Response::Error(error) => anyhow::bail!(error),
                Response::LaunchFailed(error) => {
                    return Err(super::protocol::LaunchFailure(error).into());
                }
                response => return Ok(response),
            }
        }
    }
    /// # Errors
    /// Returns a connection or server snapshot error.
    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        match self.request(&Request::Snapshot)? {
            Response::Snapshot(snapshot) => Ok(snapshot.snapshot),
            _ => anyhow::bail!("native mux returned no topology snapshot"),
        }
    }
    /// Consume pushed topology without waiting on the daemon on the UI thread.
    /// # Errors
    /// Returns an observed disconnect or protocol failure.
    pub fn latest_snapshot(
        &self,
        repaint: &crate::controller::RepaintHandle,
    ) -> Result<Option<TopologySnapshot>> {
        {
            let mut repaints = self
                .client
                .repaints
                .lock()
                .map_err(|_| anyhow::anyhow!("native repaint registry lock poisoned"))?;
            repaints.retain(|callback| callback.strong_count() > 0);
            let weak = Arc::downgrade(repaint);
            if !repaints.iter().any(|callback| callback.ptr_eq(&weak)) {
                repaints.push(weak);
            }
        }
        self.start_watcher();
        let latest = self
            .client
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux snapshot lock poisoned"))?;
        latest.clone().transpose().map_err(anyhow::Error::msg)
    }
    fn start_watcher(&self) {
        if self.client.watching.swap(true, Ordering::AcqRel) {
            return;
        }
        *self
            .client
            .ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let weak = Arc::downgrade(&self.client);
        thread::spawn(move || {
            let mut backoff = Duration::from_millis(100);
            while weak.strong_count() > 0 {
                let result = (|| -> Result<()> {
                    let backend = Self {
                        client: weak.upgrade().context("native mux client closed")?,
                    };
                    let mut stream = backend.connect()?;
                    stream.read_timeout(Duration::from_millis(200))?;
                    backend.client.mark_ready(Ok(()));
                    protocol::write_blocking(&mut stream, &Frame::json(&Request::Snapshot)?)?;
                    drop(backend);
                    let mut reader = protocol::FrameReader::default();
                    while weak.strong_count() > 0 {
                        let Some(frame) = reader.poll(&mut stream)? else {
                            continue;
                        };
                        match frame.decode::<Response>()? {
                            Response::Snapshot(snapshot) | Response::Changed(snapshot) => {
                                let Some(client) = weak.upgrade() else {
                                    break;
                                };
                                client.publish(snapshot)?;
                            }
                            Response::Error(error) => anyhow::bail!(error),
                            _ => anyhow::bail!("invalid native topology event"),
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result
                    && let Some(client) = weak.upgrade()
                {
                    let error = format!("native mux disconnected: {error:#}");
                    if let Ok(mut latest) = client.latest.lock() {
                        *latest = Some(Err(error.clone()));
                    }
                    client.mark_ready(Err(error));
                    client.connected.store(false, Ordering::Release);
                    client.repaint();
                    let wait = Arc::clone(&client.retry.wait);
                    drop(client);
                    wait(backoff);
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(3));
                } else {
                    break;
                }
            }
        });
    }
    fn await_watcher(&self, deadline: Instant) -> Result<()> {
        self.start_watcher();
        let mut ready = self
            .client
            .ready
            .lock()
            .map_err(|_| anyhow::anyhow!("native readiness lock poisoned"))?;
        while ready.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RequestExpired.into());
            }
            let (next, wait) = self
                .client
                .ready_changed
                .wait_timeout(ready, remaining)
                .map_err(|_| anyhow::anyhow!("native readiness lock poisoned"))?;
            ready = next;
            if wait.timed_out() && ready.is_none() {
                return Err(RequestExpired.into());
            }
        }
        ready
            .as_ref()
            .context("native client readiness missing")?
            .clone()
            .map_err(anyhow::Error::msg)
    }

    /// # Errors
    /// Rejects invalid targets and failed server operations.
    pub fn execute(&self, command: MuxCommand) -> Result<()> {
        self.request(&Request::Execute(command)).map(|_| ())
    }
    fn execute_persisted_direct(
        &self,
        command: MuxCommand,
        persist: impl FnOnce() -> Result<()>,
        expected: Option<u64>,
    ) -> Result<TopologySnapshot> {
        let mut stream = self.connect()?;
        self.check_generation(expected)?;
        protocol::write_blocking(&mut stream, &Frame::json(&Request::Prepare(command))?)?;
        match Self::response(&mut stream) {
            Ok(Response::Ok) => {}
            Err(error) if error.is::<super::protocol::LaunchFailure>() => {
                persist()?;
                return Err(error);
            }
            Err(error) => return Err(error),
            Ok(_) => anyhow::bail!("native mux did not prepare command"),
        }

        if let Err(error) = persist() {
            let _ = protocol::write_blocking(&mut stream, &Frame::json(&Request::Abort)?);
            return Err(error);
        }
        protocol::write_blocking(&mut stream, &Frame::json(&Request::Commit)?)?;
        match Self::response(&mut stream)
            .context("native mux commit outcome unknown; reconcile the server snapshot")?
        {
            Response::Snapshot(snapshot) => {
                self.client.publish(snapshot.clone())?;
                Ok(snapshot)
            }
            _ => anyhow::bail!("native mux commit returned no snapshot"),
        }
    }
}
impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    fn mark_ready(&self, value: std::result::Result<(), String>) {
        let mut ready = self
            .ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *ready = Some(value);
        drop(ready);
        self.ready_changed.notify_all();
    }
    fn repaint(&self) {
        let callbacks: Vec<_> = self
            .repaints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for callback in callbacks {
            callback();
        }
    }
    fn publish(&self, snapshot: TopologySnapshot) -> Result<()> {
        if snapshot.generation != self.generation.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux snapshot lock poisoned"))?;
        let changed = latest.as_ref().is_none_or(|current| {
            current.as_ref().map_or(true, |current| {
                current.revision <= snapshot.revision
                    && (current.revision != snapshot.revision
                        || current.snapshot != snapshot.snapshot)
            })
        });
        if changed {
            *latest = Some(Ok(snapshot));
        }
        drop(latest);
        if changed {
            self.repaint();
        }
        Ok(())
    }
}

impl Job {
    fn run(self, backend: &NativeBackend) {
        let deadline = match &self {
            Self::Request { deadline, .. } | Self::Persisted { deadline, .. } => *deadline,
        };
        if Instant::now() >= deadline {
            self.fail_error(RequestExpired.into());
            return;
        }
        if matches!(&self, Self::Persisted { cancellation, .. } if cancellation.is_cancelled()) {
            self.fail_error(RequestCanceled.into());
            return;
        }
        if let Err(error) = backend.await_watcher(deadline) {
            self.fail_error(error);
            return;
        }
        match self {
            Self::Request {
                request,
                deadline,
                expected,
                reply,
            } => {
                let result = if Instant::now() >= deadline {
                    Err(RequestExpired.into())
                } else {
                    backend.request_direct(&request, expected)
                };
                let _ = reply.send(result);
            }
            Self::Persisted {
                command,
                deadline,
                cancellation,
                expected,
                persist,
                reply,
            } => {
                let result = if Instant::now() >= deadline {
                    Err(RequestExpired.into())
                } else if !cancellation.try_start() {
                    Err(RequestCanceled.into())
                } else {
                    backend.execute_persisted_direct(command, persist, expected)
                };
                let _ = reply.send(result);
            }
        }
    }
    fn fail(self, message: &str) {
        self.fail_error(anyhow::anyhow!(message.to_owned()));
    }
    fn fail_error(self, error: anyhow::Error) {
        match self {
            Self::Request { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Persisted { reply, .. } => {
                let _ = reply.send(Err(error));
            }
        }
    }
}

pub(super) fn validate_welcome(
    welcome: &Welcome,
    verifier: Option<&IdentityVerifier>,
) -> Result<()> {
    protocol::IncompatibleVersion::validate(WIRE_VERSION, welcome.version)?;
    for capability in [
        "server-frames",
        "pane-input",
        "topology-events",
        "configure",
    ] {
        ensure!(
            welcome.capabilities.iter().any(|value| value == capability),
            "native mux server lacks required capability: {capability}"
        );
    }
    if let Some(verify) = verifier {
        ensure!(
            welcome
                .capabilities
                .iter()
                .any(|value| value == "server-identity"),
            "remote mux lacks stable server identity capability"
        );
        verify(&welcome.server_id)?;
    }
    Ok(())
}
