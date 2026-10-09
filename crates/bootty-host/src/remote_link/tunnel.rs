use super::{client::RemoteLink, tls::Identity};
use crate::ssh::SshRemote;
use anyhow::{Context as _, Result, bail};
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use rustls::pki_types::CertificateDer;
use std::{
    process::{Command, Stdio},
    time::Duration,
};

pub(super) async fn connect(
    remote: &SshRemote,
    identity: &Identity,
    certificate: CertificateDer<'static>,
    port: u16,
) -> Result<RemoteLink> {
    if port == 0 {
        bail!("remote daemon has no TCP transport endpoint");
    }
    let (program, mut args) = remote.connection_options(&[
        "-o",
        "BatchMode=yes",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ServerAliveInterval=1",
        "-o",
        "ServerAliveCountMax=3",
    ]);
    // Stdio forwarding needs no local port. Broker death closes stdin, so SSH exits
    // even when no Rust destructor could run; no detached forwarding process survives.
    args.extend([
        "-T".into(),
        "-W".into(),
        format!("127.0.0.1:{port}"),
        "--".into(),
        remote.destination(),
    ]);
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut tunnel =
        ProcessTreeChild::spawn_with_console_window(&mut command, ConsoleWindowBehavior::Suppress)?;
    let input = tokio::process::ChildStdin::from_std(
        tunnel
            .child_mut()
            .stdin
            .take()
            .context("SSH tunnel stdin")?,
    )?;
    let output = tokio::process::ChildStdout::from_std(
        tunnel
            .child_mut()
            .stdout
            .take()
            .context("SSH tunnel stdout")?,
    )?;
    let socket = tokio::io::join(output, input);
    let link = tokio::time::timeout(
        Duration::from_secs(5),
        RemoteLink::connect_stream(identity, certificate, socket),
    )
    .await??;
    Ok(link.with_tunnel(tunnel))
}
