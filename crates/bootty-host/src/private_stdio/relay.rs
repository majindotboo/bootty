//! A remote private endpoint relayed over the owning host's encrypted process stream.
use super::{MAX_TOOL_IMAGE_RESPONSE_BYTES, MAX_TOOL_MESSAGE_BYTES, read_message};
use crate::remote::RemoteHost;
use anyhow::{Context as _, Result};
use rmux_os::process_tree::ProcessTreeChild;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DirectoryRequest {
    name: String,
    files: Vec<PrivateFile>,
}

/// Its process lifetime bounds the remote socket. Tool authority remains at the local listener.
pub struct RemoteStdioRelay {
    child: Option<ProcessTreeChild>,
    remote: RemoteHost,
    daemon: String,
    name: String,
}

impl RemoteStdioRelay {
    /// Prepare on a worker; private files travel on stdin, never in arguments or logs.
    /// # Errors
    /// Returns invalid private material, transport, or remote readiness errors.
    pub fn start(
        remote: &RemoteHost,
        daemon: &str,
        name: &str,
        files: Vec<PrivateFile>,
        local_socket: PathBuf,
    ) -> Result<Self> {
        anyhow::ensure!(cfg!(unix), "Private tool relay requires Unix sockets");
        let request = DirectoryRequest {
            name: name.into(),
            files,
        };
        validate(&request)?;
        let bytes = serde_json::to_vec(&request)?;
        anyhow::ensure!(
            bytes.len() < MAX_TOOL_MESSAGE_BYTES,
            "Private tool files exceed their bound"
        );
        let identity = match bootty_config::ApplicationIdentity::for_process() {
            bootty_config::ApplicationIdentity::Production => "bootty",
            bootty_config::ApplicationIdentity::Development => "bootty-dev",
        };
        let (program, args) = remote.proxy_command_in(
            "/",
            daemon,
            &[
                "--application-identity".into(),
                identity.into(),
                "private-stdio-relay".into(),
            ],
        )?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = ProcessTreeChild::spawn(&mut command)?;
        let mut relay = Self {
            child: Some(child),
            remote: remote.clone(),
            daemon: daemon.into(),
            name: name.into(),
        };
        let child = relay.child.as_mut().context("Remote tool process")?;
        let mut input = child
            .child_mut()
            .stdin
            .take()
            .context("Remote tool input")?;
        input.write_all(&bytes)?;
        input.write_all(b"\n")?;
        input.flush()?;
        let output = child
            .child_mut()
            .stdout
            .take()
            .context("Remote tool output")?;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            let ready = read_message(&mut output, 128).and_then(|line| {
                if line.as_deref() == Some(b"READY\n") {
                    Ok(())
                } else {
                    Err(std::io::Error::other(
                        "Remote tool endpoint did not become ready",
                    ))
                }
            });
            let active = ready.is_ok();
            let _ = ready_tx.send(ready);
            if active {
                let _ = pump(&local_socket, &mut output, &mut input);
            }
        });
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .context("Remote tool readiness")??;
        Ok(relay)
    }
}

impl Drop for RemoteStdioRelay {
    fn drop(&mut self) {
        drop(self.child.take());
        let remote = self.remote.clone();
        let daemon = self.daemon.clone();
        let name = self.name.clone();
        // Closing a conversation never waits for the network. An offline host can retain
        // private files; closing the owned relay severs their route to the local lease.
        std::thread::spawn(move || {
            use crate::CommandRunner as _;
            let identity = match bootty_config::ApplicationIdentity::for_process() {
                bootty_config::ApplicationIdentity::Production => "bootty",
                bootty_config::ApplicationIdentity::Development => "bootty-dev",
            };
            let runner =
                crate::remote::RemoteCommandRunner::new(remote, crate::SystemCommandRunner);
            let _ = runner.run_in(
                "/",
                &daemon,
                &[
                    "--application-identity".into(),
                    identity.into(),
                    "private-stdio-cleanup".into(),
                    name,
                ],
            );
        });
    }
}

#[cfg(unix)]
fn pump(
    socket: &Path,
    output: &mut impl std::io::BufRead,
    input: &mut impl Write,
) -> std::io::Result<()> {
    use std::os::unix::net::UnixStream;
    while let Some(request) = read_message(output, MAX_TOOL_MESSAGE_BYTES.saturating_add(1024))? {
        let mut stream = UnixStream::connect(socket)?;
        let timeout = Some(Duration::from_secs(6));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        stream.write_all(&request)?;
        let response = read_message(&mut BufReader::new(stream), MAX_TOOL_IMAGE_RESPONSE_BYTES)?;
        // MCP notifications have no response; the relay still needs a framing acknowledgement.
        input.write_all(response.as_deref().unwrap_or(b"\n"))?;
        input.flush()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn pump(_: &Path, _: &mut impl std::io::BufRead, _: &mut impl Write) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "Private tool relay requires Unix sockets",
    ))
}

fn validate(request: &DirectoryRequest) -> Result<()> {
    let nonce = request.name.strip_prefix("bt-tool-").unwrap_or_default();
    anyhow::ensure!(
        nonce.len() == 64
            && nonce
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "Invalid private tool directory"
    );
    anyhow::ensure!(
        !request.files.is_empty() && request.files.len() <= 4,
        "Invalid private tool file count"
    );
    let mut names = std::collections::HashSet::new();
    for file in &request.files {
        anyhow::ensure!(
            matches!(
                file.name.as_str(),
                "connection.json" | "mcp.json" | "tools.ts" | "permissions.ts"
            ) && names.insert(&file.name),
            "Invalid private tool file"
        );
        anyhow::ensure!(
            file.bytes.len() <= MAX_TOOL_MESSAGE_BYTES,
            "Private tool file exceeds its bound"
        );
    }
    Ok(())
}

/// Serve a private Unix endpoint for one owned remote process, without choosing local authority.
/// # Errors
/// Returns bounded framing, private-file, socket or retired transport errors.
#[cfg(unix)]
pub fn serve(input: &mut impl std::io::BufRead, output: &mut impl Write) -> Result<()> {
    use std::os::unix::{
        fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _},
        net::UnixListener,
    };
    let bytes =
        read_message(input, MAX_TOOL_MESSAGE_BYTES)?.context("Missing private tool files")?;
    let request: DirectoryRequest = serde_json::from_slice(&bytes)?;
    validate(&request)?;
    let directory = Path::new("/tmp").join(&request.name);
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let directory = DirectoryCleanup(directory);
    for file in request.files {
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.0.join(file.name))?;
        target.write_all(&file.bytes)?;
        target.flush()?;
    }
    let socket = directory.0.join("tools.sock");
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    output.write_all(b"READY\n")?;
    output.flush()?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        let timeout = Some(Duration::from_secs(6));
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        let request = read_message(
            &mut BufReader::new(&mut stream),
            MAX_TOOL_MESSAGE_BYTES.saturating_add(1024),
        )?
        .context("Missing tool request")?;
        output.write_all(&request)?;
        output.flush()?;
        let Some(response) = read_message(input, MAX_TOOL_IMAGE_RESPONSE_BYTES)? else {
            return Ok(());
        };
        if response != b"\n" {
            stream.write_all(&response)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn serve(_: &mut impl std::io::BufRead, _: &mut impl Write) -> Result<()> {
    anyhow::bail!("Private tool relay requires Unix sockets")
}

#[cfg(unix)]
struct DirectoryCleanup(PathBuf);
#[cfg(unix)]
impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Remove only a previously prepared private relay directory.
/// # Errors
/// Retains directories with unexpected entries or permissions rather than remove other data.
#[cfg(unix)]
pub fn cleanup(name: &str) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    validate(&DirectoryRequest {
        name: name.into(),
        files: vec![PrivateFile {
            name: "connection.json".into(),
            bytes: Vec::new(),
        }],
    })?;
    let directory = Path::new("/tmp").join(name);
    let metadata = match std::fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_dir() && metadata.permissions().mode().trailing_zeros() >= 6,
        "Relay cleanup requires its private directory"
    );
    let entries = std::fs::read_dir(&directory)?.collect::<std::io::Result<Vec<_>>>()?;
    for entry in &entries {
        let metadata = std::fs::symlink_metadata(entry.path())?;
        let name = entry.file_name();
        let valid = if name == "tools.sock" {
            metadata.file_type().is_socket()
        } else {
            matches!(
                name.to_str(),
                Some("connection.json" | "mcp.json" | "tools.ts" | "permissions.ts")
            ) && metadata.is_file()
        };
        anyhow::ensure!(
            valid && metadata.permissions().mode().trailing_zeros() >= 6,
            "Relay cleanup retains unexpected entries"
        );
    }
    for entry in entries {
        std::fs::remove_file(entry.path())?;
    }
    std::fs::remove_dir(directory)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn cleanup(_: &str) -> Result<()> {
    anyhow::bail!("Private tool relay requires Unix sockets")
}
