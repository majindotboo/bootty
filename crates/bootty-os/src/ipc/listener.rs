use std::io;

use super::{LocalEndpoint, LocalStream, PeerIdentity};

/// A listening local endpoint: a Unix socket, or a Windows named pipe that only this account can
/// open.
#[derive(Debug)]
pub struct LocalListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    pipe_name: std::ffi::OsString,
    /// The next unconnected pipe instance. Replaced as soon as a client takes it.
    #[cfg(windows)]
    pending: tokio::sync::Mutex<tokio::net::windows::named_pipe::NamedPipeServer>,
}

impl LocalListener {
    /// Binds `endpoint`. Must run inside a Tokio runtime, which registers the listener.
    ///
    /// # Errors
    /// Returns the bind failure, including an endpoint another listener already owns.
    pub fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: tokio::net::UnixListener::bind(endpoint.as_path())?,
            })
        }
        #[cfg(windows)]
        {
            let pipe_name = endpoint.as_path().as_os_str().to_owned();
            let pending = crate::windows::create_pipe_server(&pipe_name, true)?;
            Ok(Self {
                pipe_name,
                pending: tokio::sync::Mutex::new(pending),
            })
        }
    }

    /// Accepts one client with its process and account. Cancel-safe.
    ///
    /// A client that disconnects before its identity can be read is dropped and the next one is
    /// awaited: one vanished client is not a listener failure.
    ///
    /// # Errors
    /// Returns the listener's accept failure.
    pub async fn accept(&self) -> io::Result<(LocalStream, PeerIdentity)> {
        loop {
            #[cfg(unix)]
            {
                let (stream, _) = self.inner.accept().await?;
                let Ok(credentials) = stream.peer_cred() else {
                    continue;
                };
                let Some(pid) = credentials.pid().and_then(|pid| u32::try_from(pid).ok()) else {
                    continue;
                };
                let user = crate::identity::UserIdentity::from_uid(credentials.uid());
                return Ok((stream, PeerIdentity { pid, user }));
            }
            #[cfg(windows)]
            {
                let mut pending = self.pending.lock().await;
                let connected = pending.connect().await;
                let next = crate::windows::create_pipe_server(&self.pipe_name, false)?;
                let server = std::mem::replace(&mut *pending, next);
                drop(pending);
                if connected.is_err() {
                    continue;
                }
                let Ok(peer) = crate::windows::pipe_client_identity(&server) else {
                    continue;
                };
                return Ok((server, peer));
            }
        }
    }
}
