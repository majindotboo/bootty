use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, StreamOwned,
    pki_types::{CertificateDer, ServerName},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const RESPONSE_LIMIT: usize = 1024 * 1024;
const PAIRING_LIMIT: usize = 8192;
const TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pairing {
    version: u32,
    host: IpAddr,
    port: u16,
    certificate: String,
    token: String,
}

/// Credentials are transient and deliberately have no Debug implementation.
#[derive(Clone)]
pub struct Connection {
    address: SocketAddr,
    token: String,
    tls: Arc<ClientConfig>,
}

/// Desktop-issued targets are opaque. The phone never constructs or edits handles/generations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub kind: String,
    pub handle: String,
    pub generation: String,
}

#[derive(Clone, Serialize)]
pub struct Invocation {
    pub command: String,
    pub arguments: Vec<String>,
    pub caller: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<Value>,
}

impl Invocation {
    #[must_use]
    pub fn new(command: impl Into<String>, arguments: Vec<String>, target: Option<Target>) -> Self {
        Self {
            command: command.into(),
            arguments,
            caller: "socket",
            target,
            confirmation: None,
        }
    }

    pub fn confirm(&mut self) {
        self.confirmation = Some(
            json!({"command": self.command, "arguments": self.arguments, "target": self.target}),
        );
    }
}

pub enum CommandResult {
    Value(Value),
    Confirmation(Value),
}

impl Connection {
    /// Uses only the certificate copied from the desktop. System/public roots are never trusted.
    ///
    /// Numeric addresses avoid a second, unbounded DNS resolver on the background worker.
    ///
    /// # Errors
    /// Rejects incompatible, malformed, oversized or unusable pairing credentials.
    pub fn from_code(code: &str) -> Result<Self, String> {
        if code.len() > PAIRING_LIMIT {
            return Err("Pairing code is too large".into());
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(
                code.trim()
                    .strip_prefix("bootty://pair/")
                    .ok_or("Copy a Bootty pairing code from your computer")?,
            )
            .map_err(|_| "Invalid pairing code")?;
        let pairing: Pairing =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid pairing code")?;
        if pairing.version != 1
            || pairing.port == 0
            || pairing.host.is_unspecified()
            || pairing.host.is_multicast()
        {
            return Err("This pairing code is incompatible".into());
        }
        if URL_SAFE_NO_PAD
            .decode(&pairing.token)
            .map_err(|_| "Invalid pairing credential")?
            .len()
            != 32
        {
            return Err("Invalid pairing credential".into());
        }
        let certificate = URL_SAFE_NO_PAD
            .decode(pairing.certificate)
            .map_err(|_| "Invalid desktop certificate")?;
        if certificate.len() > 4096 {
            return Err("Desktop certificate is too large".into());
        }
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(certificate))
            .map_err(|_| "Invalid desktop certificate")?;
        let tls =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(|error| error.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
        Ok(Self {
            address: SocketAddr::new(pairing.host, pairing.port),
            token: pairing.token,
            tls: Arc::new(tls),
        })
    }

    #[must_use]
    pub fn address(&self) -> String {
        self.address.to_string()
    }

    /// Blocking transport; callers must use the GPUI background executor.
    ///
    /// A mutation is never retried: reconnect only refreshes live state.
    ///
    /// # Errors
    /// Returns connection, certificate, protocol, payload or owner errors.
    pub fn invoke(&self, invocation: &Invocation) -> Result<CommandResult, String> {
        let response = self.rpc("command.invoke", &json!({"invocation": invocation}))?;
        match response.get("status").and_then(Value::as_str) {
            Some("success") => Ok(CommandResult::Value(
                response.get("value").cloned().unwrap_or(Value::Null),
            )),
            Some("confirmation_required") => Ok(CommandResult::Confirmation(
                response
                    .get("confirmation")
                    .cloned()
                    .ok_or("Desktop did not supply its confirmation")?,
            )),
            _ => Err(response
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Desktop command failed")
                .to_owned()),
        }
    }

    /// # Errors
    /// Returns a bounded and authenticated desktop reply or a visible error.
    pub fn rpc(&self, method: &str, params: &Value) -> Result<Value, String> {
        let request = json!({"token": self.token, "request": {"jsonrpc":"2.0", "id":1, "method":method, "params":params}});
        let mut encoded = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        if encoded.len() >= RESPONSE_LIMIT {
            return Err("Command exceeds the payload limit".into());
        }
        encoded.push(b'\n');
        let socket = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))
            .map_err(|_| "Computer is offline or remote control is disabled")?;
        socket
            .set_read_timeout(Some(TIMEOUT))
            .map_err(|error| error.to_string())?;
        socket
            .set_write_timeout(Some(TIMEOUT))
            .map_err(|error| error.to_string())?;
        let name = ServerName::try_from("bootty.local").map_err(|error| error.to_string())?;
        let tls = ClientConnection::new(Arc::clone(&self.tls), name)
            .map_err(|error| error.to_string())?;
        let mut stream = StreamOwned::new(tls, socket);
        while stream.conn.is_handshaking() {
            stream.conn.complete_io(&mut stream.sock).map_err(
                |_| "Couldn’t verify the paired computer. Check the connection or pair again",
            )?;
        }
        stream
            .write_all(&encoded)
            .map_err(|_| "Couldn’t authenticate the computer or send the command")?;
        let mut response = String::new();
        BufReader::new(stream).take((RESPONSE_LIMIT + 1) as u64).read_line(&mut response)
            .map_err(|_| "Connection lost; the command may already have run. Check the terminal before retrying")?;
        if response.len() > RESPONSE_LIMIT || !response.ends_with('\n') {
            return Err("Computer returned an invalid or oversized reply".into());
        }
        let response: Value =
            serde_json::from_str(&response).map_err(|_| "Computer returned an invalid reply")?;
        if response["jsonrpc"] != "2.0" {
            return Err("Computer returned an incompatible reply".into());
        }
        if let Some(error) = response.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Remote command failed")
                .into());
        }
        if response["id"] != 1 {
            return Err("Computer returned an incompatible reply".into());
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| "Computer omitted its reply".into())
    }
}
