//! Restores the authenticated transport before opening the unchanged mux protocol.
use super::{
    RemoteLink,
    broker::announce,
    routes::{Route, Routes},
    server::Bootstrap,
    tls::Identity,
};
use crate::ssh::SshRemote;
use anyhow::{Context as _, Result};
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

pub type MuxConnector = Arc<
    dyn Fn(String) -> Pin<Box<dyn std::future::Future<Output = Result<MuxStream>> + Send>>
        + Send
        + Sync,
>;

pub type MuxPreflight = Arc<
    dyn Fn(MuxStream) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

pub struct MuxStream {
    stream: Box<dyn Stream>,
}
impl MuxStream {
    pub fn new(stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static) -> Self {
        Self {
            stream: Box::new(stream),
        }
    }
}
impl AsyncRead for MuxStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for MuxStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

struct State {
    announcement: Option<Bootstrap>,
    link: Option<Arc<RemoteLink>>,
    routes: Routes,
}
pub struct RemoteMux {
    remote: SshRemote,
    namespace: String,
    identity: Identity,
    preflight: MuxPreflight,
    state: tokio::sync::Mutex<State>,
    changed: tokio::sync::watch::Sender<u64>,
}
impl RemoteMux {
    /// Call inside the owning runtime. Construction starts no SSH or network work.
    /// # Errors
    /// Returns certificate generation failures.
    pub fn new(remote: SshRemote, namespace: String, preflight: MuxPreflight) -> Result<Arc<Self>> {
        let (changed, _) = tokio::sync::watch::channel(0);
        let owner = Arc::new(Self {
            remote,
            namespace,
            identity: Identity::generate()?,
            preflight,
            state: tokio::sync::Mutex::new(State {
                announcement: None,
                link: None,
                routes: Routes::new(Instant::now(), Duration::from_secs(30)),
            }),
            changed,
        });
        let weak = Arc::downgrade(&owner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let Some(owner) = weak.upgrade() else {
                    break;
                };
                owner.preflight_routes().await;
            }
        });
        Ok(owner)
    }

    async fn dial(
        &self,
        announcement: Option<&Bootstrap>,
        route: Route,
    ) -> Result<Arc<RemoteLink>> {
        let response = announcement.context("remote route announcement missing")?;
        let certificate = response.certificate.clone().into();
        let link = match route {
            Route::Quic => {
                RemoteLink::connect(&self.identity, certificate, response.quic_address()).await?
            }
            Route::Tls => {
                RemoteLink::connect_tcp(&self.identity, certificate, response.tcp_address()).await?
            }
            // One SSH channel carries every stream. A process per stream outgrew sshd's session
            // limit, and each refused stream reconnected until sshd refused the host.
            Route::Ssh => {
                super::tunnel::connect(&self.remote, &self.identity, certificate, response.tcp_port)
                    .await?
            }
        };
        // The SSH-authenticated pinned certificate proves this route before any
        // launch environment or application credential is sent. No TLS early data.
        (self.preflight)(link.open_mux(&self.namespace).await?).await?;
        Ok(Arc::new(link))
    }
    async fn promote(&self, state: &mut State) -> Result<()> {
        let candidates: Vec<_> = state.routes.candidates(Instant::now()).collect();
        for route in candidates {
            // The SSH tunnel spends its own handshake before TLS; direct routes fail fast.
            let budget = Duration::from_secs(if route == Route::Ssh { 10 } else { 4 });
            let result =
                tokio::time::timeout(budget, self.dial(state.announcement.as_ref(), route)).await;
            if let Ok(Ok(link)) = result {
                self.install(state, route, link);
                return Ok(());
            }
        }
        state.routes.defer(Instant::now());
        anyhow::bail!("preferred remote routes unavailable")
    }
    fn install(&self, state: &mut State, route: Route, link: Arc<RemoteLink>) {
        if let Some(old) = state.link.replace(link) {
            old.close();
        }
        state.routes.connected(route, Instant::now());
        self.changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    async fn preflight_routes(&self) {
        // Network preflight must not hold the lock used to open active streams:
        // blocked UDP must never stall input on a healthy TLS or SSH fallback.
        let (mut announcement, candidates, active, generation) = {
            let mut state = self.state.lock().await;
            let active = state.routes.active();
            if active.is_none_or(|route| route == Route::Quic) {
                return;
            }
            let candidates: Vec<_> = state.routes.candidates(Instant::now()).collect();
            if candidates.is_empty() {
                return;
            }
            state.routes.defer(Instant::now());
            (
                state.announcement.clone(),
                candidates,
                active,
                *self.changed.borrow(),
            )
        };
        if active == Some(Route::Ssh)
            && let Ok(response) = announce(&self.remote, &self.identity).await
        {
            announcement = Some(response);
        }
        for route in candidates {
            let result = tokio::time::timeout(
                Duration::from_secs(4),
                self.dial(announcement.as_ref(), route),
            )
            .await;
            if let Ok(Ok(link)) = result {
                let mut state = self.state.lock().await;
                if *self.changed.borrow() != generation {
                    return;
                }
                state.announcement = announcement;
                self.install(&mut state, route, link);
                drop(state);
                return;
            }
        }
    }
    /// # Errors
    /// Returns installation, authenticated route, or stream establishment failures.
    pub async fn open(&self) -> Result<MuxStream> {
        let mut state = self.state.lock().await;
        let closed = state.link.as_ref().is_some_and(|link| link.is_closed());
        if closed {
            state.link = None;
            state.routes.disconnected(Instant::now());
        }
        if state.routes.active().is_none() {
            if state.announcement.is_none() {
                let remote = self.remote.clone();
                tokio::task::spawn_blocking(move || remote.ensure_daemon()).await??;
                state.announcement = announce(&self.remote, &self.identity).await.ok();
            }
            self.promote(&mut state).await?;
        }
        let link = state
            .link
            .as_ref()
            .context("no remote route is connected")?;
        let stream =
            match tokio::time::timeout(Duration::from_secs(10), link.open_mux(&self.namespace))
                .await
                .unwrap_or_else(|error| Err(error.into()))
            {
                Ok(stream) => stream,
                Err(error) => {
                    link.close();
                    state.link = None;
                    state.announcement = None;
                    state.routes.disconnected(Instant::now());
                    return Err(error);
                }
            };
        let mut changed = self.changed.subscribe();
        let link = state.link.clone();
        drop(state);
        let (client, mut relay) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let _link = link;
            let mut stream = stream;
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut relay, &mut stream) => {},
                _ = changed.changed() => {},
            }
        });
        Ok(MuxStream::new(client))
    }
}
