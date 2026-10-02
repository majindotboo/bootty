#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, Ipv4Addr, TcpStream},
    os::unix::net::UnixListener,
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use assert_fs::TempDir;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_control::{InstanceDescriptor, RemoteControlServer};
use pretty_assertions::assert_eq;
use rstest::rstest;
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, StreamOwned,
    pki_types::{CertificateDer, ServerName},
};
use serde::Deserialize;
use serde_json::{Value, json};

struct Host {
    _directory: TempDir,
    local: UnixListener,
    remote: RemoteControlServer,
    pairing: Pairing,
}

#[derive(Deserialize)]
struct Pairing {
    certificate: String,
    token: String,
}

impl Host {
    fn new() -> Result<Self> {
        let directory = TempDir::new()?;
        let endpoint = directory.path().join("control.sock");
        let local = UnixListener::bind(&endpoint)?;
        let descriptor = InstanceDescriptor {
            instance_id: "fixture".into(),
            generation: 1,
            pid: std::process::id(),
            window_state_key: "fixture".into(),
            endpoint,
            started_at_ms: 0,
            protocol_version: 1,
        };
        let host = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let remote = RemoteControlServer::spawn(&descriptor, (host, 0).into(), host)?;
        let bytes = URL_SAFE_NO_PAD.decode(
            remote
                .pairing_code()
                .strip_prefix("bootty://pair/")
                .context("pairing prefix")?,
        )?;
        let pairing = serde_json::from_slice(&bytes)?;
        Ok(Self {
            _directory: directory,
            local,
            remote,
            pairing,
        })
    }

    fn stream(&self, certificate: &[u8]) -> Result<StreamOwned<ClientConnection, TcpStream>> {
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(certificate.to_vec()))?;
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .with_root_certificates(roots)
                .with_no_client_auth();
        let connection =
            ClientConnection::new(Arc::new(config), ServerName::try_from("bootty.local")?)?;
        let socket = TcpStream::connect(self.remote.address())?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(StreamOwned::new(connection, socket))
    }

    fn call(&self, request: &Value) -> Result<Value> {
        let certificate = URL_SAFE_NO_PAD.decode(&self.pairing.certificate)?;
        let mut stream = self.stream(&certificate)?;
        serde_json::to_writer(&mut stream, request)?;
        stream.write_all(b"\n")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    }

    fn request(&self) -> Value {
        json!({"token": self.pairing.token, "request": {
            "jsonrpc":"2.0", "id":"phone", "method":"command.invoke", "params": {
                "invocation": {"command":"terminal.write", "arguments":["🥟 echo"], "caller":"socket",
                    "target":{"kind":"terminal", "handle":"opaque", "generation":"9007199254740993"}}
            }
        }})
    }
}

#[rstest]
fn trusted_remote_preserves_command_target_arguments_and_response() -> Result<()> {
    let host = Host::new()?;
    let local = host.local.try_clone()?;
    let worker = thread::spawn(move || -> Result<Value> {
        let (mut stream, _) = local.accept()?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let request: Value = serde_json::from_str(&line)?;
        stream.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"success\",\"value\":\"actual owner reply\"}}\n")?;
        Ok(request)
    });
    let request = host.request();
    let response = host.call(&request)?;
    assert_eq!(response["result"]["value"], "actual owner reply");
    assert_eq!(response["id"], "phone");
    let forwarded = worker
        .join()
        .map_err(|_| anyhow::anyhow!("owner worker panicked"))??;
    assert_eq!(forwarded["method"], request["request"]["method"]);
    assert_eq!(forwarded["params"], request["request"]["params"]);
    Ok(())
}

#[rstest]
#[case("credential", -32001)]
#[case("version", -32600)]
#[case("id", -32600)]
fn rejected_remote_requests_never_reach_the_owner(
    #[case] invalid: &str,
    #[case] code: i32,
) -> Result<()> {
    let host = Host::new()?;
    let mut request = host.request();
    match invalid {
        "credential" => request["token"] = json!("incorrect"),
        "version" => request["request"]["jsonrpc"] = json!("1.0"),
        "id" => request["request"]["id"] = json!("x".repeat(4097)),
        _ => bail!("unknown case"),
    }
    assert_eq!(host.call(&request)?["error"]["code"], code);
    host.local.set_nonblocking(true)?;
    assert_eq!(
        host.local.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    Ok(())
}

#[rstest]
fn a_different_certificate_is_rejected_and_drop_revokes_the_listener() -> Result<()> {
    let host = Host::new()?;
    let other = rcgen::generate_simple_self_signed(vec!["bootty.local".into()])?;
    let mut stream = host.stream(other.cert.der())?;
    let result = stream
        .write_all(b"{}\n")
        .and_then(|()| stream.read(&mut [0]));
    ensure!(result.is_err(), "unpaired certificate was accepted");
    let address = host.remote.address();
    drop(host);
    ensure!(
        TcpStream::connect(address).is_err(),
        "revoked listener accepted a connection"
    );
    Ok(())
}
