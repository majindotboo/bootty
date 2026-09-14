//! Window-owned SSH forwards for remote loopback links. Establishment happens off the UI thread.
use crate::{CommandRunner, SystemCommandRunner, require_success, ssh::SshRemote};
use anyhow::{Context as _, Result, bail};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener},
    sync::Arc,
};
use url::{Host, Url};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ForwardInfo {
    pub id: String,
    pub host: String,
    pub remote_host: String,
    pub remote_port: u16,
    pub local_port: u16,
    pub source_url: String,
    pub url: String,
}
pub struct ForwardLease {
    id: String,
    source_url: Url,
    local_url: String,
    remote: SshRemote,
    control: Arc<tempfile::TempDir>,
    remote_host: String,
    remote_port: u16,
    local_port: u16,
}

/// None means the URL does not refer to the remote machine's loopback interface.
#[must_use]
pub fn loopback_destination(url: &Url) -> Option<(IpAddr, String, u16)> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let (bind, host) = match url.host()? {
        Host::Domain("localhost") => (IpAddr::V4(Ipv4Addr::LOCALHOST), "localhost".to_owned()),
        Host::Ipv4(ip) if ip.is_loopback() => (IpAddr::V4(ip), ip.to_string()),
        Host::Ipv6(ip) if ip.is_loopback() => (IpAddr::V6(ip), format!("[{ip}]")),
        Host::Ipv4(ip) if ip.is_unspecified() => {
            (IpAddr::V4(Ipv4Addr::LOCALHOST), "127.0.0.1".to_owned())
        }
        Host::Ipv6(ip) if ip.is_unspecified() => {
            (IpAddr::V6(Ipv6Addr::LOCALHOST), "[::1]".to_owned())
        }
        _ => return None,
    };
    Some((bind, host, url.port_or_known_default()?))
}

impl ForwardLease {
    /// # Errors
    /// Returns port reservation, SSH setup, or URL validation errors.
    pub fn restart(&self, runner: &impl CommandRunner) -> Result<Arc<Self>> {
        Self::start(self.remote.clone(), &self.source_url, runner)
    }
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    #[must_use]
    pub fn info(&self) -> ForwardInfo {
        ForwardInfo {
            id: self.id.clone(),
            host: self.remote.destination(),
            remote_host: self.remote_host.clone(),
            remote_port: self.remote_port,
            local_port: self.local_port,
            source_url: self.source_url.to_string(),
            url: self.local_url.clone(),
        }
    }
    #[must_use]
    pub fn matches(&self, remote: &SshRemote, url: &Url) -> bool {
        self.remote == *remote
            && loopback_destination(url)
                .is_some_and(|(_, host, port)| host == self.remote_host && port == self.remote_port)
    }
    /// # Errors
    /// Returns an error for a non-loopback URL or one that cannot carry a TCP port.
    pub fn url(&self, url: Url) -> Result<String> {
        forwarded_url(url, self.local_port)
    }
    /// # Errors
    /// Returns an error if the SSH control master is unavailable.
    pub fn check(&self, runner: &impl CommandRunner) -> Result<()> {
        let (program, args) = self.command(&["-O".to_owned(), "check".to_owned()]);
        require_success(&program, &args, runner.run(&program, &args)?).map(|_| ())
    }
    /// Call on a worker before application shutdown so cleanup is awaited by the host.
    /// # Errors
    /// Returns an error if the SSH control master cannot be stopped.
    pub fn close(&self, runner: &impl CommandRunner) -> Result<()> {
        let (program, args) = self.command(&["-O".to_owned(), "exit".to_owned()]);
        require_success(&program, &args, runner.run(&program, &args)?).map(|_| ())
    }
    /// # Errors
    /// Returns unsupported platform, invalid URL, port reservation, identity, or SSH setup errors.
    pub fn start(remote: SshRemote, url: &Url, runner: &impl CommandRunner) -> Result<Arc<Self>> {
        // Windows OpenSSH lacks control masters; do not open an unrelated local server there.
        if !cfg!(unix) {
            bail!("Remote loopback links require OpenSSH control-master support on this platform");
        }
        let (bind, remote_host, remote_port) =
            loopback_destination(url).context("URL is not loopback")?;
        let reservation = TcpListener::bind((bind, 0)).context("reserve a loopback port")?;
        let local_port = reservation.local_addr()?.port();
        let mut random = [0; 16];
        getrandom::fill(&mut random)
            .map_err(|error| anyhow::anyhow!("Forward identity: {error}"))?;
        let lease = Arc::new(Self {
            id: format!("forward-{:032x}", u128::from_le_bytes(random)),
            source_url: url.clone(),
            local_url: forwarded_url(url.clone(), local_port)?,
            remote,
            control: Arc::new(
                tempfile::Builder::new()
                    .prefix("bootty-forward-")
                    .tempdir()?,
            ),
            remote_host,
            remote_port,
            local_port,
        });
        let local = match bind {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        let (program, args) = lease.command(&[
            "-f".to_owned(),
            "-N".to_owned(),
            "-T".to_owned(),
            "-L".to_owned(),
            format!("{local}:{local_port}:{}:{remote_port}", lease.remote_host),
        ]);
        drop(reservation);
        // -f returns after authentication and listener creation; bind conflicts fail explicitly.
        require_success(&program, &args, runner.run(&program, &args)?)
            .context("establish remote URL forward")?;
        Ok(lease)
    }
    fn command(&self, operation: &[String]) -> (String, Vec<String>) {
        // OpenSSH keeps the first ControlPath, including when supplied with -S. It must
        // precede both user arguments and the ordinary shared-command socket defaults.
        let control = format!(
            "ControlPath={}",
            self.control.path().join("control").display()
        );
        let (program, mut args) = self.remote.connection_options(&[
            "-o",
            "BatchMode=yes",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=no",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            &control,
        ]);
        args.extend_from_slice(operation);
        args.extend(["--".to_owned(), self.remote.destination()]);
        (program, args)
    }
}
impl Drop for ForwardLease {
    fn drop(&mut self) {
        let (program, args) = self.command(&["-O".to_owned(), "exit".to_owned()]);
        // Preserve the socket pathname until cleanup completes; never run SSH on the UI thread.
        let directory = Arc::clone(&self.control);
        std::thread::spawn(move || {
            if directory.path().join("control").exists() {
                let _ = SystemCommandRunner.run(&program, &args);
            }
            drop(directory);
        });
    }
}

fn forwarded_url(mut url: Url, local_port: u16) -> Result<String> {
    let (bind, _, _) = loopback_destination(&url).context("URL is not loopback")?;
    // Keep localhost for HTTPS name validation. Wildcard listen addresses are not destinations.
    if url.host().is_some_and(|host| {
        matches!(host,Host::Ipv4(ip) if ip.is_unspecified())
            || matches!(host,Host::Ipv6(ip) if ip.is_unspecified())
    }) {
        url.set_ip_host(bind)
            .map_err(|()| anyhow::anyhow!("invalid loopback address"))?;
    }
    url.set_port(Some(local_port))
        .map_err(|()| anyhow::anyhow!("URL has no TCP port"))?;
    Ok(url.into())
}
