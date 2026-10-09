use super::{
    process::{self, Input},
    protocol::{self, RemoteOutput, RemoteProcessRequest, RemoteTerminalSize},
    tls::Identity,
};
use anyhow::{Context as _, Result, bail};
use quinn::Endpoint;
use rustls::pki_types::CertificateDer;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{
    io::{BufRead as _, Read as _, Write as _},
    net::{IpAddr, SocketAddr},
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::AsyncWriteExt as _;

#[derive(Deserialize, Serialize)]
pub(super) struct Bootstrap {
    pub address: SocketAddr,
    pub certificate: Vec<u8>,
    #[serde(default)]
    pub tcp_port: u16,
}

/// One SSH-authorized client certificate; unknown clients cannot open process streams.
pub struct RemoteLinkServer {
    endpoint: Endpoint,
    identity: Identity,
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl RemoteLinkServer {
    /// # Errors
    /// Returns certificate, client trust, or socket bind errors.
    pub fn bind(address: SocketAddr, client: CertificateDer<'static>) -> Result<Self> {
        let identity = Identity::generate()?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(
            identity.server_tls(Some(client.clone()), b"h2")?,
        ));
        let endpoint = Endpoint::server(identity.server(client)?, address)?;
        let tcp = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        tcp.set_nonblocking(true)?;
        let tcp = tokio::net::TcpListener::from_std(tcp)?;
        Ok(Self {
            endpoint,
            identity,
            tcp,
            acceptor,
        })
    }

    /// # Errors
    /// Returns an unavailable listener address.
    pub fn address(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }
    /// # Errors
    /// Returns an unavailable loopback listener address.
    pub fn tcp_address(&self) -> Result<SocketAddr> {
        Ok(self.tcp.local_addr()?)
    }

    #[must_use]
    pub fn certificate(&self) -> CertificateDer<'static> {
        self.identity.cert.clone()
    }

    /// # Errors
    /// Returns authentication timeout or listener errors.
    pub async fn serve(self) -> Result<()> {
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(20))
            .context("authentication deadline overflow")?;
        let mut handshakes = tokio::task::JoinSet::new();
        let connection = loop {
            tokio::select! {
                incoming = self.endpoint.accept(), if handshakes.len() < 16 => {
                    let incoming = incoming.context("remote listener closed")?;
                    handshakes.spawn(async move { Ok::<_, anyhow::Error>(Accepted::Quic(incoming.await?)) });
                }
                socket = self.tcp.accept(), if handshakes.len() < 16 => {
                    let (socket, _) = socket?;
                    socket.set_nodelay(true)?;
                    let acceptor = self.acceptor.clone();
                    handshakes.spawn(async move {
                        let socket = acceptor.accept(socket).await?;
                        let connection = h2::server::Builder::new()
                            .initial_window_size(2 * 1024 * 1024)
                            .initial_connection_window_size(16 * 1024 * 1024)
                            .max_concurrent_streams(64)
                            .max_send_buffer_size(protocol::CHUNK)
                            .handshake(socket).await?;
                        Ok::<_, anyhow::Error>(Accepted::Tcp(Box::new(connection)))
                    });
                }
                accepted = handshakes.join_next(), if !handshakes.is_empty() => {
                    if let Some(Ok(Ok(connection))) = accepted { break connection; }
                }
                () = tokio::time::sleep_until(deadline) => bail!("remote authentication deadline elapsed"),
            }
        };
        handshakes.abort_all();
        let mut processes = tokio::task::JoinSet::new();
        match connection {
            Accepted::Quic(connection) => loop {
                tokio::select! {
                    stream = connection.accept_bi() => {
                        let Ok((send, recv)) = stream else { break; };
                        processes.spawn(serve_process(Box::new(send), Box::new(recv)));
                    }
                    _ = processes.join_next(), if !processes.is_empty() => {},
                }
            },
            Accepted::Tcp(mut connection) => loop {
                tokio::select! {
                    stream = connection.accept() => {
                        let Some(Ok((request, mut reply))) = stream else { break; };
                        if request.method() != http::Method::POST || request.uri().path() != "/process" {
                            reply.send_response(http::Response::builder().status(404).body(())?, true)?;
                            continue;
                        }
                        let send = reply.send_response(http::Response::new(()), false)?;
                        processes.spawn(serve_process(Box::new(super::http2::Writer::new(send)), Box::new(super::http2::Reader::new(request.into_body()))));
                    }
                    _ = processes.join_next(), if !processes.is_empty() => {},
                }
            },
        }
        // A disconnected client never leaves an execution process or attach client behind.
        processes.abort_all();
        while processes.join_next().await.is_some() {}
        self.endpoint.close(0_u32.into(), b"client disconnected");
        Ok(())
    }
}

enum Accepted {
    Quic(quinn::Connection),
    Tcp(
        Box<
            h2::server::Connection<
                tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
                bytes::Bytes,
            >,
        >,
    ),
}

async fn serve_process(
    mut send: protocol::RemoteWriter,
    mut recv: protocol::RemoteReader,
) -> Result<()> {
    let (kind, bytes) = protocol::read_frame(&mut recv).await?;
    if kind == protocol::PROBE && bytes.is_empty() {
        protocol::write_frame(&mut send, protocol::PROBE, &[]).await?;
        send.shutdown().await?;
        return Ok(());
    }
    if kind != 0 {
        bail!("invalid remote process request");
    }
    let request: RemoteProcessRequest = serde_json::from_slice(&bytes)?;
    request.validate()?;
    let terminal = request.terminal.is_some();
    let mut process = match tokio::task::spawn_blocking(move || process::spawn(&request)).await? {
        Ok(process) => process,
        Err(error) => {
            protocol::write_frame(
                &mut send,
                protocol::STDERR,
                format!("{error:#}\n").as_bytes(),
            )
            .await?;
            protocol::write_frame(&mut send, protocol::EXIT, &1_i32.to_be_bytes()).await?;
            send.shutdown().await?;
            return Ok(());
        }
    };
    let input = process.input.clone();
    let cancel = process.cancel.clone();
    let mut reader = tokio::spawn(async move {
        let mut ended = false;
        loop {
            let (kind, bytes) = protocol::read_frame(&mut recv).await?;
            let message = match kind {
                protocol::INPUT if !ended && bytes.len() <= protocol::CHUNK => Input::Bytes(bytes),
                protocol::RESIZE if terminal && bytes.len() <= 128 => {
                    let size: RemoteTerminalSize = serde_json::from_slice(&bytes)?;
                    if size.cols == 0 || size.rows == 0 {
                        bail!("invalid terminal dimensions");
                    }
                    Input::Resize(size)
                }
                protocol::EOF if bytes.is_empty() && !ended => {
                    ended = true;
                    Input::Eof
                }
                protocol::CANCEL if bytes.is_empty() => {
                    ended = true;
                    cancel();
                    Input::Eof
                }
                _ => bail!("invalid remote process input"),
            };
            // Closing stdin does not end stdout/stderr or replace the child's exit status.
            _ = input.send(message).await;
        }
    });
    let result = async {
        loop {
            tokio::select! {
                stopped = &mut reader => { return stopped?; }
                output = process.output.recv() => {
                    match output.context("remote process output ended without status")? {
                        RemoteOutput::Stdout(bytes) => protocol::write_frame(&mut send, protocol::STDOUT, &bytes).await?,
                        RemoteOutput::Stderr(bytes) => protocol::write_frame(&mut send, protocol::STDERR, &bytes).await?,
                        RemoteOutput::Exit(code) => {
                            protocol::write_frame(&mut send, protocol::EXIT, &code.to_be_bytes()).await?;
                            send.shutdown().await?;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }.await;
    reader.abort();
    result
}

/// SSH carries only public certificates and the authenticated endpoint announcement.
/// # Errors
/// Returns invalid certificate, child launch, or startup announcement errors.
pub fn run_bootstrap() -> Result<()> {
    let mut client = Vec::new();
    std::io::stdin()
        .lock()
        .take(16 * 1024 + 1)
        .read_to_end(&mut client)?;
    if client.is_empty() || client.len() > 16 * 1024 {
        bail!("invalid bootstrap certificate size");
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--application-identity",
            match bootty_config::ApplicationIdentity::for_process() {
                bootty_config::ApplicationIdentity::Production => "bootty",
                bootty_config::ApplicationIdentity::Development => "bootty-dev",
            },
            "remote-link-server",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, true);
    let mut child = rmux_os::daemon::spawn_hidden_daemon_command(&mut command)?;
    child
        .stdin
        .take()
        .context("bootstrap stdin")?
        .write_all(&client)?;
    let mut response = String::new();
    std::io::BufReader::new(child.stdout.take().context("bootstrap stdout")?)
        .take(32 * 1024)
        .read_line(&mut response)?;
    let parsed: Bootstrap = serde_json::from_str(&response).context("remote transport startup")?;
    if parsed.certificate.len() > 16 * 1024 {
        bail!("invalid server certificate size");
    }
    std::io::stdout().lock().write_all(response.as_bytes())?;
    Ok(())
}

/// # Errors
/// Returns missing SSH metadata, invalid certificate, listener, or authentication errors.
pub fn run_server() -> Result<()> {
    let mut certificate = Vec::new();
    std::io::stdin()
        .lock()
        .take(16 * 1024 + 1)
        .read_to_end(&mut certificate)?;
    if certificate.is_empty() || certificate.len() > 16 * 1024 {
        bail!("invalid client certificate size");
    }
    let connection =
        std::env::var("SSH_CONNECTION").context("SSH connection metadata is missing")?;
    let address: IpAddr = connection
        .split_whitespace()
        .nth(2)
        .context("SSH server address is missing")?
        .parse()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let server = RemoteLinkServer::bind(SocketAddr::new(address, 0), certificate.into())?;
        let response = Bootstrap {
            address: server.address()?,
            certificate: server.certificate().to_vec(),
            tcp_port: server.tcp_address()?.port(),
        };
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
        stdout.flush()?;
        drop(stdout);
        server.serve().await
    })
}
