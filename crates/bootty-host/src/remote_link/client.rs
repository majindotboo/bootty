use super::{
    protocol::{
        self, RemoteOutput, RemoteProcessRequest, RemoteReader, RemoteTerminalSize, RemoteWriter,
        SERVER_NAME,
    },
    tls::Identity,
};
use anyhow::{Result, bail};
use bytes::Bytes;
use quinn::{Connection, Endpoint};
use rustls::pki_types::CertificateDer;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

/// One pinned, mutually authenticated connection shared by every remote process.
pub struct RemoteLink {
    transport: Transport,
    tunnel: Option<rmux_os::process_tree::ProcessTreeChild>,
}

enum Transport {
    Quic {
        endpoint: Endpoint,
        connection: Connection,
    },
    Tcp {
        client: h2::client::SendRequest<Bytes>,
        driver: tokio::task::JoinHandle<()>,
        closed: tokio::sync::watch::Receiver<bool>,
    },
}

impl RemoteLink {
    /// # Errors
    /// Returns socket, TLS authentication, or connection timeout errors.
    pub async fn connect(
        identity: &Identity,
        server: CertificateDer<'static>,
        address: SocketAddr,
    ) -> Result<Self> {
        let bind = if address.is_ipv6() {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        };
        let mut endpoint = Endpoint::client(SocketAddr::new(bind, 0))?;
        endpoint.set_default_client_config(identity.client(server)?);
        let connection = tokio::time::timeout(
            Duration::from_secs(3),
            endpoint.connect(address, SERVER_NAME)?,
        )
        .await??;
        let link = Self {
            transport: Transport::Quic {
                endpoint,
                connection,
            },
            tunnel: None,
        };
        tokio::time::timeout(Duration::from_secs(3), link.probe()).await??;
        Ok(link)
    }

    /// # Errors
    /// Returns socket, pinned mutual-TLS, or HTTP/2 connection errors.
    pub async fn connect_tcp(
        identity: &Identity,
        server: CertificateDer<'static>,
        address: SocketAddr,
    ) -> Result<Self> {
        let socket = tokio::net::TcpStream::connect(address).await?;
        socket.set_nodelay(true)?;
        Self::connect_stream(identity, server, socket).await
    }

    pub(super) async fn connect_stream<
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    >(
        identity: &Identity,
        server: CertificateDer<'static>,
        socket: S,
    ) -> Result<Self> {
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(
            identity.client_tls(server, b"h2")?,
        ));
        let socket = connector
            .connect(
                rustls::pki_types::ServerName::try_from(SERVER_NAME)?,
                socket,
            )
            .await?;
        let (client, connection) = h2::client::Builder::new()
            .initial_window_size(2 * 1024 * 1024)
            .initial_connection_window_size(16 * 1024 * 1024)
            .max_send_buffer_size(protocol::CHUNK)
            .handshake(socket)
            .await?;
        let (closed_tx, closed) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(async move {
            _ = connection.await;
            _ = closed_tx.send(true);
        });
        let link = Self {
            transport: Transport::Tcp {
                client,
                driver,
                closed,
            },
            tunnel: None,
        };
        link.probe().await?;
        Ok(link)
    }

    async fn probe(&self) -> Result<()> {
        use tokio::io::AsyncWriteExt as _;
        let (mut input, mut output) = self.open_stream().await?;
        protocol::write_frame(&mut input, protocol::PROBE, &[]).await?;
        input.shutdown().await?;
        let (kind, bytes) = protocol::read_frame(&mut output).await?;
        if kind != protocol::PROBE || !bytes.is_empty() {
            bail!("remote transport readiness probe failed");
        }
        Ok(())
    }

    pub(super) fn with_tunnel(mut self, tunnel: rmux_os::process_tree::ProcessTreeChild) -> Self {
        self.tunnel = Some(tunnel);
        self
    }

    #[must_use]
    pub const fn transport_name(&self) -> &'static str {
        match &self.transport {
            Transport::Quic { .. } => "quic",
            Transport::Tcp { .. } if self.tunnel.is_some() => "tls-over-ssh",
            Transport::Tcp { .. } => "tls",
        }
    }

    /// # Errors
    /// Returns invalid request, unavailable connection, or stream write errors.
    pub async fn start(&self, request: &RemoteProcessRequest) -> Result<RemoteProcess> {
        request.validate()?;
        let (mut input, output) = self.open_stream().await?;
        protocol::write_json(&mut input, request).await?;
        Ok(RemoteProcess { input, output })
    }

    pub(super) async fn open_stream(&self) -> Result<(RemoteWriter, RemoteReader)> {
        match &self.transport {
            Transport::Quic { connection, .. } => {
                let (send, recv) = connection.open_bi().await?;
                Ok((Box::new(send), Box::new(recv)))
            }
            Transport::Tcp { client, .. } => {
                let mut client = client.clone().ready().await?;
                let request = http::Request::builder()
                    .method(http::Method::POST)
                    .uri("/process")
                    .body(())?;
                let (response, send) = client.send_request(request, false)?;
                let response = response.await?;
                if response.status() != http::StatusCode::OK {
                    bail!("remote process stream was rejected");
                }
                Ok((
                    Box::new(super::http2::Writer::new(send)),
                    Box::new(super::http2::Reader::new(response.into_body())),
                ))
            }
        }
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        match &self.transport {
            Transport::Quic { connection, .. } => connection.close_reason().is_some(),
            Transport::Tcp { driver, .. } => driver.is_finished(),
        }
    }

    pub(super) async fn closed(&self) {
        match &self.transport {
            Transport::Quic { connection, .. } => {
                _ = connection.closed().await;
            }
            Transport::Tcp { closed, .. } => {
                let mut closed = closed.clone();
                while !*closed.borrow() {
                    if closed.changed().await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

impl Drop for RemoteLink {
    fn drop(&mut self) {
        match &self.transport {
            Transport::Quic { endpoint, .. } => endpoint.close(0_u32.into(), b"client closed"),
            Transport::Tcp { driver, .. } => driver.abort(),
        }
    }
}

pub struct RemoteProcess {
    input: RemoteWriter,
    output: RemoteReader,
}

impl RemoteProcess {
    pub(super) fn into_streams(self) -> (RemoteWriter, RemoteReader) {
        (self.input, self.output)
    }
    /// # Errors
    /// Returns a closed stream or input delivery error.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        for bytes in bytes.chunks(protocol::CHUNK) {
            protocol::write_frame(&mut self.input, protocol::INPUT, bytes).await?;
        }
        Ok(())
    }

    /// # Errors
    /// Returns serialization or resize delivery errors.
    pub async fn resize(&mut self, size: RemoteTerminalSize) -> Result<()> {
        protocol::write_frame(
            &mut self.input,
            protocol::RESIZE,
            &serde_json::to_vec(&size)?,
        )
        .await
    }

    /// # Errors
    /// Returns a closed stream or EOF delivery error.
    pub async fn finish_input(&mut self) -> Result<()> {
        protocol::write_frame(&mut self.input, protocol::EOF, &[]).await
    }

    /// Terminate the captured process tree; drain output to `Exit` to await cleanup.
    /// # Errors
    /// Returns a closed stream or cancellation delivery error.
    pub async fn cancel(&mut self) -> Result<()> {
        protocol::write_frame(&mut self.input, protocol::CANCEL, &[]).await
    }

    /// # Errors
    /// Returns stream loss, invalid framing, or malformed exit status.
    pub async fn next(&mut self) -> Result<RemoteOutput> {
        let (kind, bytes) = protocol::read_frame(&mut self.output).await?;
        match kind {
            protocol::STDOUT => Ok(RemoteOutput::Stdout(bytes)),
            protocol::STDERR => Ok(RemoteOutput::Stderr(bytes)),
            protocol::EXIT if bytes.len() == 4 => Ok(RemoteOutput::Exit(i32::from_be_bytes(
                bytes.as_slice().try_into()?,
            ))),
            _ => bail!("invalid remote process output"),
        }
    }
}
