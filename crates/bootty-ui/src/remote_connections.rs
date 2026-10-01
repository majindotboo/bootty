//! One per-application remote-control lease, injected with the local control owner's descriptor.
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, bail};
use bootty_control::{InstanceDescriptor, RemoteControlServer};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default)]
pub struct RemoteConnections {
    state: Arc<Mutex<ConnectionState>>,
}

#[derive(Default)]
struct ConnectionState {
    descriptor: Option<InstanceDescriptor>,
    server: Option<RemoteControlServer>,
    revision: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ConnectionStatus {
    pub enabled: bool,
    pub address: Option<SocketAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
    pub suggested_host: Option<IpAddr>,
}

impl RemoteConnections {
    pub fn set_owner(
        &self,
        descriptor: Option<InstanceDescriptor>,
    ) -> Result<Option<RemoteControlServer>> {
        let old = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("Connection state unavailable"))?;
            state.descriptor = descriptor;
            state.revision = state.revision.wrapping_add(1);
            state.server.take()
        };
        Ok(old)
    }

    pub fn status(&self, include_pairing_code: bool) -> Result<ConnectionStatus> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Connection state unavailable"))?;
        Ok(ConnectionStatus {
            enabled: state.server.is_some(),
            address: state.server.as_ref().map(RemoteControlServer::address),
            pairing_code: state
                .server
                .as_ref()
                .filter(|_| include_pairing_code)
                .map(|server| server.pairing_code().to_owned()),
            suggested_host: state.server.as_ref().map(|server| server.address().ip()),
        })
    }

    pub fn enable(&self, host: IpAddr) -> Result<ConnectionStatus> {
        if host.is_unspecified() || host.is_multicast() {
            bail!("Enter this computer's local IP address");
        }
        let (descriptor, revision) = {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("Connection state unavailable"))?;
            if state.server.is_some() {
                drop(state);
                return self.status(true);
            }
            (
                state
                    .descriptor
                    .clone()
                    .context("Local control owner is not available")?,
                state.revision,
            )
        };
        // Bind only the chosen interface. Starting a listener never holds a UI-visible lock.
        let server = RemoteControlServer::spawn(&descriptor, SocketAddr::new(host, 0), host)?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("Connection state unavailable"))?;
            if state.descriptor.as_ref() != Some(&descriptor) || state.revision != revision {
                bail!("Connection was revoked or its owner changed; enable it again");
            }
            if state.server.is_none() {
                state.server = Some(server);
            }
        }
        self.status(true)
    }

    pub fn revoke(&self) -> Result<ConnectionStatus> {
        let old = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("Connection state unavailable"))?;
            state.revision = state.revision.wrapping_add(1);
            state.server.take()
        };
        drop(old);
        self.status(false)
    }

    pub fn copy_pairing_code(&self) -> Result<()> {
        let code = self
            .status(true)?
            .pairing_code
            .context("Enable the connection first")?;
        crate::platform::write_clipboard_text(&code)
    }
}

/// Route selection does not send a datagram or scan neighboring devices.
pub fn suggested_host() -> Result<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9))?;
    let host = socket.local_addr()?.ip();
    if host.is_unspecified() {
        bail!("Enter this computer's local IP address");
    }
    Ok(host)
}
