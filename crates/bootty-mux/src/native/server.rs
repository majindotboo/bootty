//! Native topology and process owner. Connections are projections and input sources.
#![expect(
    clippy::significant_drop_tightening,
    reason = "Topology and staged pane handles remain under one lock through reservation and publication; references are not independent guards"
)]

use super::protocol::TopologySnapshot;
use super::{
    protocol::{self, Frame, Hello, Request, Response, WIRE_VERSION, Welcome},
    server_pane::{Pane, PaneCommand, PaneHost, PaneLauncher, local_launcher},
    state::NativeMuxState,
    subscriber::Subscriber,
};
use crate::command::MuxCommand;
use anyhow::{Context as _, Result, ensure};
use bootty_terminal::{SessionLaunchConfig, geometry::TerminalGeometry};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Notify, mpsc},
};

pub const DEFAULT_SIZE: TerminalGeometry = TerminalGeometry {
    cols: 100,
    rows: 30,
    cell_width: 10,
    cell_height: 22,
};
struct Workspace {
    topology: NativeMuxState,
    panes: HashMap<String, Pane>,
    subscribers: Vec<Subscriber>,
    launch: SessionLaunchConfig,
    vt: tokio::sync::watch::Sender<super::protocol::VtPolicy>,
    generation: u64,
    revision: u64,
    pending_change: Arc<AtomicBool>,
    reservation: Option<usize>,
    prepared: Option<(NativeMuxState, HashMap<String, Pane>)>,
}
impl Workspace {
    fn new(generation: u64) -> Self {
        Self {
            topology: NativeMuxState::new(generation),
            panes: HashMap::new(),
            subscribers: Vec::new(),
            launch: SessionLaunchConfig::default(),
            vt: tokio::sync::watch::channel(super::protocol::VtPolicy::default()).0,
            generation,
            revision: 0,
            pending_change: Arc::new(AtomicBool::new(false)),
            reservation: None,
            prepared: None,
        }
    }
    fn prepare_topology(
        &self,
        command: &MuxCommand,
        cwd: Option<String>,
    ) -> Result<NativeMuxState> {
        let mut next = self.topology.clone();
        next.execute(command)?;
        let snapshot = next.snapshot();
        if let Some(cwd) = cwd {
            for anchor in snapshot
                .sessions
                .iter()
                .flat_map(|session| &session.windows)
                .flat_map(|window| &window.panes)
            {
                if let Some(id) = anchor.pane_id.as_deref()
                    && !self.panes.contains_key(id)
                {
                    next.set_pane_cwd(id, cwd.clone().into())?;
                }
            }
        }
        Ok(next)
    }
    fn snapshot(&self) -> TopologySnapshot {
        let mut snapshot = self.topology.snapshot();
        for session in &mut snapshot.sessions {
            for window in &mut session.windows {
                if !self
                    .topology
                    .window_has_custom_name(&session.id, &window.id)
                    && let Some(title) = window
                        .anchor
                        .pane_id
                        .as_ref()
                        .and_then(|id| self.panes.get(id))
                        .and_then(Pane::title)
                {
                    window.name = title;
                }
            }
        }
        TopologySnapshot {
            generation: self.generation,
            revision: self.revision,
            snapshot,
        }
    }
    fn publish(&mut self) -> Result<TopologySnapshot> {
        self.revision = self
            .revision
            .checked_add(1)
            .context("native topology revision exhausted")?;
        let snapshot = self.snapshot();
        let frame = Frame::json(&Response::Changed(snapshot.clone()))?;
        self.subscribers
            .retain(|subscriber| subscriber.push(frame.clone()));
        Ok(snapshot)
    }
}
pub enum ServerEvent {
    PaneExited {
        workspace: String,
        pane: String,
        generation: u64,
    },
    TopologyChanged {
        workspace: String,
        pending: Arc<AtomicBool>,
    },
}
pub type ServerEvents = mpsc::UnboundedReceiver<ServerEvent>;

pub struct MuxServer {
    server_id: String,
    generation: u64,
    workspaces: Mutex<HashMap<String, Workspace>>,
    launcher: PaneLauncher,
    clock: Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
    exits: mpsc::UnboundedSender<ServerEvent>,
    clients: AtomicUsize,
    next_client: AtomicUsize,
    idle: Notify,
    topology_ready: Notify,
}
// Own unpublished panes and the reservation until installation transfers ownership.
struct StagedPanes<'a> {
    server: &'a MuxServer,
    scope: &'a str,
    client: usize,
    panes: HashMap<String, Pane>,
    installed: bool,
}
impl Drop for StagedPanes<'_> {
    fn drop(&mut self) {
        if self.installed {
            return;
        }
        for pane in self.panes.values() {
            pane.stop();
        }
        // Cleanup must also work during unwinding after a poisoned lock.
        let mut workspaces = self
            .server
            .workspaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(workspace) = workspaces.get_mut(self.scope)
            && workspace.reservation == Some(self.client)
        {
            workspace.reservation = None;
            self.server.topology_ready.notify_waiters();
        }
    }
}
impl MuxServer {
    /// # Errors
    /// Returns an OS entropy failure when minting the server generation.
    pub fn new(launcher: PaneLauncher) -> Result<(Arc<Self>, ServerEvents)> {
        Self::with_server_id(
            launcher,
            format!(
                "{:032x}",
                u128::from(getrandom::u64()?).wrapping_shl(64) | u128::from(getrandom::u64()?)
            ),
        )
    }
    /// # Errors
    /// Returns an empty identity or OS entropy failure.
    pub fn with_server_id(
        launcher: PaneLauncher,
        server_id: String,
    ) -> Result<(Arc<Self>, ServerEvents)> {
        Self::with_server_id_and_clock(launcher, server_id, Arc::new(std::time::Instant::now))
    }
    /// # Errors
    /// Returns an empty identity or OS entropy failure.
    pub fn with_server_id_and_clock(
        launcher: PaneLauncher,
        server_id: String,
        clock: Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
    ) -> Result<(Arc<Self>, ServerEvents)> {
        ensure!(!server_id.is_empty(), "empty native mux server identity");
        let (exits, rx) = mpsc::unbounded_channel();
        Ok((
            Arc::new(Self {
                server_id,
                generation: getrandom::u64().context("mint native server generation")?,
                workspaces: Mutex::new(HashMap::new()),
                launcher,
                clock,
                exits,
                clients: AtomicUsize::new(0),
                next_client: AtomicUsize::new(1),
                idle: Notify::new(),
                topology_ready: Notify::new(),
            }),
            rx,
        ))
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Workspace>>> {
        self.workspaces
            .lock()
            .map_err(|_| anyhow::anyhow!("native mux server lock poisoned"))
    }
    fn pane(&self, scope: &str, id: &str) -> Result<Pane> {
        self.lock()?
            .get(scope)
            .and_then(|w| w.panes.get(id))
            .cloned()
            .context("native pane is unavailable")
    }
    fn source_directory(&self, scope: &str, command: &MuxCommand) -> Result<(Option<String>, u64)> {
        let (source, revision) = {
            let mut workspaces = self.lock()?;
            let workspace = workspaces
                .entry(scope.to_owned())
                .or_insert_with(|| Workspace::new(self.generation));
            let source = match command {
                MuxCommand::SplitPane {
                    session_id,
                    pane_id,
                    ..
                }
                | MuxCommand::CreatePane {
                    session_id,
                    pane_id,
                    cwd: None,
                    ..
                } => {
                    let id = workspace
                        .topology
                        .source_pane_id(session_id, pane_id.as_deref())?;
                    Some(
                        workspace
                            .panes
                            .get(id)
                            .context("native pane unavailable")?
                            .clone(),
                    )
                }
                _ => None,
            };
            (source, workspace.revision)
        };
        // Actor queues and replies must never hold the workspace mutex.
        let cwd = if let Some(source) = source {
            let (reply, response) = tokio::sync::oneshot::channel();
            source
                .tx
                .blocking_send(PaneCommand::CurrentDirectory(reply))
                .context("native pane worker stopped")?;
            response
                .blocking_recv()
                .context("native pane worker stopped")?
                .map_err(anyhow::Error::msg)?
        } else {
            None
        };
        Ok((cwd, revision))
    }
    fn prepare(&self, scope: &str, command: &MuxCommand, client: usize) -> Result<bool> {
        let (cwd, revision) = self.source_directory(scope, command)?;
        let mut workspaces = self.lock()?;
        let workspace = workspaces
            .entry(scope.to_owned())
            .or_insert_with(|| Workspace::new(self.generation));
        if workspace.reservation.is_some() || workspace.revision != revision {
            return Ok(false);
        }
        let next = workspace.prepare_topology(command, cwd)?;
        let snapshot = next.snapshot();
        let existing: std::collections::HashSet<_> = workspace.panes.keys().cloned().collect();
        let launch_config = workspace.launch.clone();
        let policy = workspace.vt.subscribe();
        let pending_change = Arc::clone(&workspace.pending_change);
        // Reserve this scope, but never hold the global mutex across PTY launch
        // or worker readiness. Other scopes and existing panes remain usable.
        workspace.reservation = Some(client);
        drop(workspaces);
        let mut staged = StagedPanes {
            server: self,
            scope,
            client,
            panes: HashMap::new(),
            installed: false,
        };
        {
            for session in &snapshot.sessions {
                for (window_index, window) in session.windows.iter().enumerate() {
                    for (pane_index, anchor) in window.panes.iter().enumerate() {
                        let id = anchor
                            .pane_id
                            .as_deref()
                            .context("native pane identity missing")?;
                        if existing.contains(id) {
                            continue;
                        }
                        let mut launch = launch_config.clone();
                        launch.working_directory = anchor.cwd.as_ref().map(Into::into);
                        launch.pane_id = Some(id.to_owned());
                        launch.command = command_argv(command);
                        launch.command_is_argv = true;
                        let exits = self.exits.clone();
                        let scope = scope.to_owned();
                        let pane_id = id.to_owned();
                        let scope_for_change = scope.clone();
                        let saved = match &command {
                            MuxCommand::RestoreSession { snapshot, .. } => snapshot
                                .windows
                                .get(window_index)
                                .and_then(|w| w.panes.get(pane_index)),
                            _ => None,
                        };
                        let size =
                            saved
                                .filter(|p| p.cols > 0 && p.rows > 0)
                                .map_or(DEFAULT_SIZE, |p| TerminalGeometry {
                                    cols: p.cols,
                                    rows: p.rows,
                                    ..DEFAULT_SIZE
                                });
                        let history = saved
                            .map(|p| {
                                bootty_terminal::terminal_history::styled_history_bytes(&p.text)
                            })
                            .transpose()?
                            .unwrap_or_default();
                        let pane = Pane::spawn(
                            size,
                            &launch,
                            &self.launcher,
                            history,
                            policy.clone(),
                            move |generation| {
                                let _ = exits.send(ServerEvent::PaneExited {
                                    workspace: scope,
                                    pane: pane_id,
                                    generation,
                                });
                            },
                            PaneHost {
                                clock: Arc::clone(&self.clock),
                                changed: self
                                    .change_notifier(scope_for_change, Arc::clone(&pending_change)),
                            },
                        )
                        .map_err(|error| super::protocol::LaunchFailure(format!("{error:#}")))?;
                        staged.panes.insert(id.to_owned(), pane);
                    }
                }
            }
            self.install_prepared(scope, client, next, &mut staged.panes)?;
            staged.installed = true;
        }
        Ok(true)
    }
    fn install_prepared(
        &self,
        scope: &str,
        client: usize,
        next: NativeMuxState,
        added: &mut HashMap<String, Pane>,
    ) -> Result<()> {
        let mut workspaces = self.lock()?;
        let workspace = workspaces
            .get_mut(scope)
            .context("native workspace unavailable")?;
        ensure!(
            workspace.reservation == Some(client),
            "native topology changed during preparation"
        );
        // The reservation excludes topology mutations. Pane title events can still
        // publish revisions while launches are staged; they do not invalidate next.
        workspace.prepared = Some((next, std::mem::take(added)));
        Ok(())
    }
    fn commit(&self, scope: &str, client: usize) -> Result<TopologySnapshot> {
        let mut workspaces = self.lock()?;
        let workspace = workspaces
            .get_mut(scope)
            .context("native workspace unavailable")?;
        ensure!(
            workspace.reservation == Some(client),
            "native topology transaction is not owned by this client"
        );
        let (next, added) = workspace
            .prepared
            .take()
            .context("native command not prepared")?;
        let snapshot = next.snapshot();
        let live: Vec<_> = snapshot
            .sessions
            .iter()
            .flat_map(|s| &s.windows)
            .flat_map(|w| &w.panes)
            .filter_map(|p| p.pane_id.as_ref())
            .collect();
        workspace.panes.retain(|id, pane| {
            if live.contains(&id) {
                true
            } else {
                pane.stop();
                false
            }
        });
        workspace.panes.extend(added);
        workspace.topology = next;
        workspace.reservation = None;
        self.topology_ready.notify_waiters();
        workspace.publish()
    }
    fn abort(&self, scope: &str, client: usize) -> Result<()> {
        let mut workspaces = self.lock()?;
        if let Some(workspace) = workspaces.get_mut(scope)
            && workspace.reservation == Some(client)
        {
            if let Some((_, added)) = workspace.prepared.take() {
                for pane in added.into_values() {
                    pane.stop();
                }
            }
            workspace.reservation = None;
            self.topology_ready.notify_waiters();
        }
        Ok(())
    }
    fn change_notifier(
        &self,
        workspace: String,
        pending: Arc<AtomicBool>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        let events = self.exits.clone();
        Arc::new(move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = events.send(ServerEvent::TopologyChanged {
                    workspace: workspace.clone(),
                    pending: Arc::clone(&pending),
                });
            }
        })
    }
    /// Apply an observed pane exit or topology wakeup in an embedded server.
    /// # Errors
    /// Returns topology publication failures.
    pub async fn handle_event(self: &Arc<Self>, event: ServerEvent) -> Result<()> {
        match event {
            ServerEvent::PaneExited {
                workspace,
                pane,
                generation,
            } => self.pane_exited(workspace, pane, generation).await,
            ServerEvent::TopologyChanged { workspace, pending } => {
                pending.store(false, Ordering::Release);
                if let Some(workspace) = self.lock()?.get_mut(&workspace) {
                    workspace.publish()?;
                }
                Ok(())
            }
        }
    }
    async fn respawn(
        &self,
        scope: &str,
        id: &str,
        mut launch: SessionLaunchConfig,
        history: &str,
    ) -> Result<()> {
        let pane = loop {
            let ready = self.topology_ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            {
                let mut workspaces = self.lock()?;
                let workspace = workspaces
                    .get_mut(scope)
                    .context("native workspace unavailable")?;
                if workspace.reservation.is_none() {
                    let snapshot = workspace.topology.snapshot();
                    let anchor = snapshot
                        .sessions
                        .iter()
                        .flat_map(|s| &s.windows)
                        .flat_map(|w| &w.panes)
                        .find(|p| p.pane_id.as_deref() == Some(id))
                        .context("native pane unavailable")?;
                    if launch.working_directory.is_none() {
                        launch.working_directory = anchor.cwd.as_ref().map(Into::into);
                    }
                    launch.pane_id = Some(id.to_owned());
                    break workspace
                        .panes
                        .get(id)
                        .cloned()
                        .context("native pane unavailable")?;
                }
            }
            ready.await;
        };
        launch.capture_environment();
        let cwd = launch.working_directory.clone();
        let history = bootty_terminal::terminal_history::styled_history_bytes(history)?;
        pane.call(|reply| PaneCommand::Respawn {
            launch: Box::new(launch),
            history,
            reply,
        })
        .await?;
        let mut workspaces = self.lock()?;
        let workspace = workspaces
            .get_mut(scope)
            .context("native workspace closed during respawn")?;
        ensure!(
            workspace
                .panes
                .get(id)
                .is_some_and(|current| current.generation == pane.generation),
            "native pane closed during respawn"
        );
        if let Some(cwd) = cwd {
            workspace.topology.set_pane_cwd(id, cwd)?;
        }
        workspace.publish()?;
        drop(workspaces);
        Ok(())
    }
    fn snapshot(&self, scope: &str) -> Result<TopologySnapshot> {
        Ok(self
            .lock()?
            .entry(scope.to_owned())
            .or_insert_with(|| Workspace::new(self.generation))
            .snapshot())
    }
    fn subscribe(
        &self,
        scope: &str,
        subscriber: Subscriber,
        launch: Option<SessionLaunchConfig>,
        vt: Option<super::protocol::VtPolicy>,
    ) -> Result<()> {
        self.configure(scope, launch, vt)?;
        let mut workspaces = self.lock()?;
        let workspace = workspaces
            .get_mut(scope)
            .context("native workspace missing")?;
        workspace
            .subscribers
            .retain(|subscriber| !subscriber.dead.load(Ordering::Acquire));
        workspace.subscribers.push(subscriber);
        Ok(())
    }
    fn configure(
        &self,
        scope: &str,
        launch: Option<SessionLaunchConfig>,
        vt: Option<super::protocol::VtPolicy>,
    ) -> Result<()> {
        let mut workspaces = self.lock()?;
        let workspace = workspaces
            .entry(scope.to_owned())
            .or_insert_with(|| Workspace::new(self.generation));
        if let Some(mut launch) = launch {
            launch.capture_environment();
            workspace.launch = launch;
        }
        if let Some(vt) = vt {
            // Commit the desired policy before waking any worker. Queue pressure
            // cannot reject it: busy workers reconcile before their next command.
            workspace.vt.send_replace(vt);
            for pane in workspace.panes.values().chain(
                workspace
                    .prepared
                    .iter()
                    .flat_map(|(_, panes)| panes.values()),
            ) {
                let _ = pane.tx.try_send(PaneCommand::ReconcilePolicy);
            }
        }
        Ok(())
    }
    /// Serve this protocol on any duplex stream, including remote-link streams.
    /// # Errors
    /// Returns a handshake, wire, request or stream failure.
    pub async fn serve(self: Arc<Self>, stream: impl AsyncRead + AsyncWrite + Unpin) -> Result<()> {
        let (mut reader, mut writer) = tokio::io::split(stream);
        self.clients.fetch_add(1, Ordering::AcqRel);
        let id = self.next_client.fetch_add(1, Ordering::Relaxed);
        let (subscriber, output) = Subscriber::new();
        let mut scope = String::new();
        let result = async {
            let hello = handshake(&mut reader, &mut writer).await?;
            scope = hello.workspace;
            let owner = Arc::clone(&self);
            let subscribe_scope = scope.clone();
            let subscribe = subscriber.clone();
            tokio::task::spawn_blocking(move || {
                owner.subscribe(&subscribe_scope, subscribe, hello.launch, hello.vt)
            })
            .await??;
            welcome(&mut writer, self.generation, &self.server_id).await?;
            let mut prepared: Option<()> = None;
            let (outgoing, commands) = mpsc::channel(256);
            let read_requests = async {
                loop {
                    let request = next_request(&mut reader, prepared.is_some()).await?;
                    if matches!(request, Request::Detach) {
                        outgoing.send(ConnectionWrite::Finish).await?;
                        return Ok(());
                    }
                    let response = match request {
                        Request::Resize { pane, size } => {
                            self.pane(&scope, &pane)?
                                .tx
                                .send(PaneCommand::LatestClient(subscriber.clone()))
                                .await?;
                            self.request(&scope, id, Request::Resize { pane, size }, &mut prepared)
                                .await
                        }
                        Request::AttachFrames { pane } => {
                            outgoing
                                .send(ConnectionWrite::AttachFrames(
                                    self.pane(&scope, &pane)?.frames,
                                ))
                                .await?;
                            Ok(Response::Ok)
                        }
                        Request::PaneCommand {
                            pane,
                            request_id,
                            command,
                        } => Ok(self
                            .terminal_command(&scope, &pane, request_id, command, &subscriber)
                            .await),
                        request => self.request(&scope, id, request, &mut prepared).await,
                    }
                    .unwrap_or_else(|error| {
                        error
                            .downcast_ref::<super::protocol::LaunchFailure>()
                            .map_or_else(
                                || Response::Error(format!("{error:#}")),
                                |error| Response::LaunchFailed(error.0.clone()),
                            )
                    });
                    outgoing.send(ConnectionWrite::response(&response)?).await?;
                }
            };
            let write_output = connection_writer(&mut writer, output, commands, &subscriber);
            tokio::try_join!(read_requests, write_output)?;
            Ok(())
        }
        .await;
        subscriber.close();
        let owner = Arc::clone(&self);
        let _ = tokio::task::spawn_blocking(move || owner.abort(&scope, id)).await;
        self.clients.fetch_sub(1, Ordering::AcqRel);
        self.idle.notify_one();
        result
    }
    async fn terminal_command(
        &self,
        scope: &str,
        pane: &str,
        request_id: u64,
        command: super::protocol::PaneCommand,
        subscriber: &Subscriber,
    ) -> Response {
        use super::protocol::PaneError;
        let result = match self.pane(scope, pane) {
            Ok(pane) => pane
                .call(|reply| PaneCommand::Command(command, subscriber.clone(), reply))
                .await
                .map_err(|e| PaneError::CommandFailed(format!("{e:#}")))
                .and_then(std::convert::identity),
            Err(e) => Err(PaneError::Unavailable(format!("{e:#}"))),
        };
        Response::PaneReply { request_id, result }
    }
    async fn prepare_when_ready(
        self: &Arc<Self>,
        scope: &str,
        command: MuxCommand,
        id: usize,
    ) -> Result<()> {
        loop {
            let ready = self.topology_ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            let reserved = self
                .lock()?
                .get(scope)
                .is_some_and(|w| w.reservation.is_some());
            if reserved {
                ready.await;
                continue;
            }
            let server = Arc::clone(self);
            let scope = scope.to_owned();
            let command = command.clone();
            if tokio::task::spawn_blocking(move || server.prepare(&scope, &command, id)).await?? {
                return Ok(());
            }
        }
    }
    async fn request(
        self: &Arc<Self>,
        scope: &str,
        id: usize,
        request: Request,
        prepared: &mut Option<()>,
    ) -> Result<Response> {
        match request {
            Request::Configure { launch, vt } => {
                let server = Arc::clone(self);
                let scope = scope.to_owned();
                tokio::task::spawn_blocking(move || server.configure(&scope, launch, vt)).await??;
                Ok(Response::Ok)
            }
            Request::Snapshot => Ok(Response::Snapshot(self.snapshot(scope)?)),
            Request::Prepare(command) => {
                self.prepare_when_ready(scope, command, id).await?;
                *prepared = Some(());
                Ok(Response::Ok)
            }
            Request::Abort => {
                let server = Arc::clone(self);
                let scope = scope.to_owned();
                tokio::task::spawn_blocking(move || server.abort(&scope, id)).await??;
                *prepared = None;
                Ok(Response::Ok)
            }
            Request::Commit => {
                prepared.take().context("no prepared native command")?;
                let server = Arc::clone(self);
                let scope = scope.to_owned();
                let snapshot =
                    tokio::task::spawn_blocking(move || server.commit(&scope, id)).await??;
                Ok(Response::Snapshot(snapshot))
            }
            Request::Execute(command) => {
                self.prepare_when_ready(scope, command, id).await?;
                let server = Arc::clone(self);
                let scope = scope.to_owned();
                let snapshot =
                    tokio::task::spawn_blocking(move || server.commit(&scope, id)).await??;
                Ok(Response::Snapshot(snapshot))
            }
            Request::AttachFrames { .. } | Request::PaneCommand { .. } | Request::Detach => {
                anyhow::bail!("attachment requires its stream")
            }
            Request::SetLayout {
                session,
                window,
                layout,
            } => loop {
                let ready = self.topology_ready.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                {
                    let mut workspaces = self.lock()?;
                    let workspace = workspaces.get_mut(scope).context("workspace missing")?;
                    if workspace.reservation.is_none() {
                        workspace.topology.set_layout(&session, &window, &layout)?;
                        workspace.publish()?;
                        return Ok(Response::Ok);
                    }
                }
                ready.await;
            },
            request => self.pane_request(scope, request).await,
        }
    }
    async fn pane_request(&self, scope: &str, request: Request) -> Result<Response> {
        match request {
            Request::Input { pane, input } => {
                self.pane(scope, &pane)?
                    .call(|reply| PaneCommand::Input(input, reply))
                    .await?;
                Ok(Response::Ok)
            }
            Request::Resize { pane, size } => {
                ensure!(size.cols > 0 && size.rows > 0, "empty pane geometry");
                self.pane(scope, &pane)?
                    .call(|reply| PaneCommand::Resize { size, reply })
                    .await?;
                Ok(Response::Ok)
            }
            Request::CaptureCheckpoint { pane, options } => {
                let runtime = self.pane(scope, &pane)?;
                Ok(Response::CheckpointCaptured(
                    runtime
                        .call(|reply| PaneCommand::CaptureCheckpoint {
                            pane,
                            options,
                            reply,
                        })
                        .await?,
                ))
            }
            Request::Capture { pane, options } => Ok(Response::Captured(
                self.pane(scope, &pane)?
                    .call(|reply| PaneCommand::Capture(options, reply))
                    .await?,
            )),
            Request::Respawn {
                pane,
                launch,
                history,
            } => {
                self.respawn(scope, &pane, launch, &history).await?;
                Ok(Response::Ok)
            }
            _ => anyhow::bail!("expected native pane request"),
        }
    }
    pub(super) async fn pane_exited(
        self: &Arc<Self>,
        scope: String,
        pane: String,
        generation: u64,
    ) -> Result<()> {
        loop {
            let ready = self.topology_ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            {
                let mut workspaces = self.lock()?;
                let Some(workspace) = workspaces.get_mut(&scope) else {
                    return Ok(());
                };
                if workspace.reservation.is_none() {
                    if workspace
                        .panes
                        .get(&pane)
                        .is_none_or(|p| p.generation != generation)
                    {
                        return Ok(());
                    }
                    let snapshot = workspace.topology.snapshot();
                    if let Some(session) = snapshot.sessions.iter().find(|s| {
                        s.windows
                            .iter()
                            .any(|w| w.panes.iter().any(|p| p.pane_id.as_deref() == Some(&pane)))
                    }) {
                        workspace.topology.execute(&MuxCommand::ClosePane {
                            session_id: session.id.clone(),
                            pane_id: Some(pane.clone()),
                        })?;
                        workspace.panes.remove(&pane);
                        workspace.publish()?;
                    }
                    return Ok(());
                }
            }
            ready.await;
        }
    }
    fn is_idle(&self) -> bool {
        self.clients.load(Ordering::Acquire) == 0
            && self.lock().is_ok_and(|w| {
                w.values()
                    .all(|w| w.topology.snapshot().sessions.is_empty())
            })
    }
}
fn command_argv(command: &MuxCommand) -> Vec<String> {
    match command {
        MuxCommand::CreateProjectSession { argv, .. } | MuxCommand::NewWindow { argv, .. } => {
            argv.clone().unwrap_or_default()
        }
        MuxCommand::CreatePane { argv, .. } => argv.clone(),
        _ => Vec::new(),
    }
}

/// # Errors
/// Returns endpoint, listener, or server failures.
pub fn run(identity: bootty_config::ApplicationIdentity) -> Result<()> {
    identity.initialize_process()?;
    let endpoint = super::local::endpoint(identity)?;
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async {
        let listener = super::local::bind(&endpoint)?;
        let Some(listener) = listener else { return Ok(()); };
        let (server, mut exits) = MuxServer::with_server_id(local_launcher(), super::identity::load_server_id(&identity.default_config_path())?)?;
        // Give the launcher a bounded interval to establish its first connection.
        let initial_idle = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(initial_idle);
        let mut awaiting_first_client = true;
        let mut lease_refresh = tokio::time::interval(super::local::LEASE_REFRESH);
        lease_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = lease_refresh.tick() => { let _ = listener.refresh_lease(); }
                accepted = listener.accept() => {
                    let (stream, peer) = accepted?;
                    if peer.is_current_user() { awaiting_first_client = false; let server = Arc::clone(&server); tokio::spawn(async move { let _ = server.serve(stream).await; }); }
                }
                Some(event) = exits.recv() => {
                    server.handle_event(event).await?;
                    if server.is_idle() { break; }
                }
                () = server.idle.notified() => { if server.is_idle() { break; } }
                () = &mut initial_idle, if awaiting_first_client => { if server.is_idle() { break; } awaiting_first_client = false; }
            }
        }
        #[cfg(unix)]
        std::fs::remove_file(endpoint.as_path())?;
        Ok(())
    })
}

impl Drop for MuxServer {
    fn drop(&mut self) {
        let workspaces = self
            .workspaces
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for workspace in workspaces.values() {
            for pane in workspace.panes.values() {
                pane.stop();
            }
            if let Some((_, added)) = &workspace.prepared {
                for pane in added.values() {
                    pane.stop();
                }
            }
        }
    }
}

async fn write_subscriber(
    writer: &mut (impl AsyncWrite + Unpin),
    frame: &Frame,
    subscriber: &Subscriber,
) -> Result<()> {
    ensure!(
        !subscriber.dead.load(Ordering::Acquire),
        "native mux subscriber overflow"
    );
    tokio::select! {
        biased;
        () = subscriber.wake.notified() => anyhow::bail!("native mux subscriber overflow"),
        result = protocol::write(writer, frame) => result,
    }
}

async fn handshake(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<Hello> {
    let hello: Hello = protocol::read(reader).await?.decode()?;
    if hello.version != WIRE_VERSION {
        protocol::write(
            writer,
            &Frame::json(&Response::IncompatibleVersion(
                protocol::IncompatibleVersion {
                    client: hello.version,
                    server: WIRE_VERSION,
                },
            ))?,
        )
        .await?;
        anyhow::bail!("native mux wire version mismatch");
    }
    Ok(hello)
}

async fn welcome(
    writer: &mut (impl AsyncWrite + Unpin),
    generation: u64,
    server_id: &str,
) -> Result<()> {
    protocol::write(
        writer,
        &Frame::json(&Welcome {
            server_id: server_id.to_owned(),
            generation,
            version: WIRE_VERSION,
            capabilities: protocol::CAPABILITIES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        })?,
    )
    .await?;
    Ok(())
}

enum ConnectionWrite {
    Frame(Frame),
    PaneReply(Frame),
    AttachFrames(
        tokio::sync::watch::Receiver<Option<Arc<bootty_terminal::terminal_frame::RenderFrame>>>,
    ),
    Finish,
}

impl ConnectionWrite {
    fn response(response: &Response) -> Result<Self> {
        let frame = Frame::json(response)?;
        Ok(if matches!(response, Response::PaneReply { .. }) {
            Self::PaneReply(frame)
        } else {
            Self::Frame(frame)
        })
    }
}

async fn connection_writer(
    writer: &mut (impl AsyncWrite + Unpin),
    mut output: mpsc::Receiver<super::subscriber::QueuedFrame>,
    mut commands: mpsc::Receiver<ConnectionWrite>,
    subscriber: &Subscriber,
) -> Result<()> {
    let (_keepalive, mut frames) = tokio::sync::watch::channel::<
        Option<Arc<bootty_terminal::terminal_frame::RenderFrame>>,
    >(None);
    let mut encoder = bootty_terminal::frame_wire::FrameEncoder::default();
    loop {
        tokio::select! {
            biased;
            () = subscriber.wake.notified() => anyhow::bail!("native mux subscriber overflow"),
            command = commands.recv() => match command.context("native connection reader stopped")? {
                ConnectionWrite::Frame(frame) => write_subscriber(writer, &frame, subscriber).await?,
                ConnectionWrite::PaneReply(reply) => {
                    let frame = frames.borrow_and_update().clone();
                    if let Some(frame) = frame { write_frame_update(writer, &mut encoder, &frame, subscriber).await?; }
                    write_subscriber(writer, &reply, subscriber).await?;
                }
                ConnectionWrite::AttachFrames(receiver) => {
                    frames = receiver;
                    frames.mark_changed();
                    encoder = bootty_terminal::frame_wire::FrameEncoder::default();
                }
                ConnectionWrite::Finish => {
                    write_subscriber(writer, &Frame::json(&Response::Ok)?, subscriber).await?;
                    return Ok(());
                }
            },
            queued = output.recv() => {
                let queued = queued.context("native mux subscription stopped")?;
                write_subscriber(writer, &queued.frame, subscriber).await?;
            }
            changed = frames.changed() => {
                if changed.is_err() {
                    write_subscriber(writer, &Frame::json(&Response::Exited)?, subscriber).await?;
                    return Ok(());
                }
                let frame = frames.borrow_and_update().clone();
                if let Some(frame) = frame {
                    write_frame_update(writer, &mut encoder, &frame, subscriber).await?;
                }
            }
        }
    }
}

async fn write_frame_update(
    writer: &mut (impl AsyncWrite + Unpin),
    encoder: &mut bootty_terminal::frame_wire::FrameEncoder,
    frame: &bootty_terminal::terminal_frame::RenderFrame,
    subscriber: &Subscriber,
) -> Result<()> {
    let Some(update) = encoder.encode_changed(frame) else {
        return Ok(());
    };
    // Large images share this stream; separate them when transfer latency requires it.
    for image in encoder.take_image_contents() {
        write_subscriber(
            writer,
            &Frame {
                kind: protocol::IMAGE_CONTENT,
                bytes: bootty_terminal::frame_wire::encode_image_content(&image)?,
            },
            subscriber,
        )
        .await?;
    }
    write_subscriber(
        writer,
        &Frame {
            kind: protocol::FRAME_UPDATE,
            bytes: bootty_terminal::frame_wire::encode_update(&update)?,
        },
        subscriber,
    )
    .await?;
    Ok(())
}

async fn next_request(reader: &mut (impl AsyncRead + Unpin), prepared: bool) -> Result<Request> {
    let frame = if prepared {
        tokio::time::timeout(std::time::Duration::from_secs(5), protocol::read(reader))
            .await
            .context("native topology transaction deadline expired")??
    } else {
        protocol::read(reader).await?
    };
    frame.decode()
}
