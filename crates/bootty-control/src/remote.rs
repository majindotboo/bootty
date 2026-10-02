use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    thread,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsAcceptor;

use crate::{
    InstanceDescriptor, invoke_instance_timeout,
    protocol::{COMMAND_TIMEOUT, IO_TIMEOUT, REQUEST_LIMIT, RPC_ID_LIMIT, RpcRequest, RpcResponse},
};

const CONNECTION_LIMIT: usize = 8;

/// An explicitly enabled, ephemeral remote attachment to one local control owner.
///
/// Dropping it revokes its pairing code and closes remote connections. Commands
/// already accepted by the desktop keep the local command path's bounded lifetime.
pub struct RemoteControlServer {
    address: SocketAddr,
    pairing_code: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Serialize)]
struct Pairing {
    version: u32,
    host: IpAddr,
    port: u16,
    certificate: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedRequest {
    token: String,
    request: RpcRequest,
}

impl RemoteControlServer {
    /// Creates a fresh certificate and credential; neither is written to disk.
    /// Call off the UI thread. The host shown in the code must be reachable by the phone.
    ///
    /// # Errors
    /// Returns an error for an invalid owner/address, certificate, bind, runtime or thread.
    pub fn spawn(
        descriptor: &InstanceDescriptor,
        listen: SocketAddr,
        advertised_host: IpAddr,
    ) -> Result<Self> {
        ensure!(
            descriptor.pid == std::process::id(),
            "remote control requires this process's local owner"
        );
        ensure!(
            !advertised_host.is_unspecified() && !advertised_host.is_multicast(),
            "pairing requires a reachable unicast address"
        );
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["bootty.local".to_owned()])?;
        let certificate = cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let tls =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .with_no_client_auth()
                .with_single_cert(vec![certificate.clone()], key.into())?;
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).context("generate remote credential")?;
        let token = URL_SAFE_NO_PAD.encode(secret);
        let listener = std::net::TcpListener::bind(listen).context("bind remote control")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let pairing_code = format!(
            "bootty://pair/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Pairing {
                version: 1,
                host: advertised_host,
                port: address.port(),
                certificate: URL_SAFE_NO_PAD.encode(certificate.as_ref()),
                token: token.clone(),
            })?)
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(CONNECTION_LIMIT)
            .enable_all()
            .build()?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener)?
        };
        let (shutdown, receive_shutdown) = tokio::sync::oneshot::channel();
        let descriptor = descriptor.clone();
        let thread = thread::Builder::new()
            .name("bootty-remote-control".to_owned())
            .spawn(move || {
                runtime.block_on(serve(
                    listener,
                    TlsAcceptor::from(Arc::new(tls)),
                    descriptor,
                    token,
                    receive_shutdown,
                ));
                // A revoked remote attachment cannot keep the desktop waiting on local I/O.
                runtime.shutdown_background();
            })?;
        Ok(Self {
            address,
            pairing_code,
            shutdown: Some(shutdown),
            thread: Some(thread),
        })
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// A secret granting full local command authority. Show/copy only on explicit request.
    #[must_use]
    pub fn pairing_code(&self) -> &str {
        &self.pairing_code
    }
}

impl Drop for RemoteControlServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve(
    listener: tokio::net::TcpListener,
    acceptor: TlsAcceptor,
    descriptor: InstanceDescriptor,
    token: String,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let slots = Arc::new(tokio::sync::Semaphore::new(CONNECTION_LIMIT));
    let mut connections = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue; };
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue; };
                let acceptor = acceptor.clone();
                let descriptor = descriptor.clone();
                let token = token.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let _ = serve_connection(stream, acceptor, descriptor, token).await;
                });
            }
        }
    }
    connections.abort_all();
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    acceptor: TlsAcceptor,
    descriptor: InstanceDescriptor,
    token: String,
) -> Result<()> {
    let mut stream = tokio::time::timeout(IO_TIMEOUT, acceptor.accept(stream)).await??;
    let mut line = String::new();
    {
        let mut reader = tokio::io::BufReader::new(&mut stream).take(REQUEST_LIMIT + 1);
        tokio::time::timeout(IO_TIMEOUT, reader.read_line(&mut line)).await??;
    }
    let response = if u64::try_from(line.len())? > REQUEST_LIMIT || !line.ends_with('\n') {
        RpcResponse::error(
            Value::Null,
            -32600,
            "invalid or oversized remote request",
            None,
        )
    } else if let Ok(authenticated) = serde_json::from_str::<AuthenticatedRequest>(&line) {
        if !bool::from(authenticated.token.as_bytes().ct_eq(token.as_bytes())) {
            RpcResponse::error(
                Value::Null,
                -32001,
                "remote control credential rejected",
                None,
            )
        } else if authenticated.request.jsonrpc != "2.0" {
            RpcResponse::error(
                authenticated.request.id,
                -32600,
                "invalid JSON-RPC version",
                None,
            )
        } else if serde_json::to_vec(&authenticated.request.id)?.len() > RPC_ID_LIMIT {
            RpcResponse::error(
                Value::Null,
                -32600,
                "request ID exceeds payload limit",
                None,
            )
        } else {
            let request = authenticated.request;
            let id = request.id.clone();
            let mut response = tokio::task::spawn_blocking(move || {
                invoke_instance_timeout(
                    &descriptor,
                    &request.method,
                    request.params,
                    COMMAND_TIMEOUT,
                )
            })
            .await?
            .unwrap_or_else(|_| {
                RpcResponse::error(
                    Value::Null,
                    -32002,
                    "desktop control owner is unavailable",
                    None,
                )
            });
            response.id = id;
            response
        }
    } else {
        RpcResponse::error(Value::Null, -32600, "invalid remote request", None)
    };
    let mut encoded = serde_json::to_vec(&response)?;
    if u64::try_from(encoded.len())? > REQUEST_LIMIT {
        encoded = serde_json::to_vec(&RpcResponse::error(
            Value::Null,
            -32603,
            "remote response exceeds payload limit",
            None,
        ))?;
    }
    encoded.push(b'\n');
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(&encoded)).await??;
    tokio::time::timeout(IO_TIMEOUT, stream.shutdown()).await??;
    Ok(())
}
