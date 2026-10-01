use crate::MuxBackendKind;
use anyhow::{Context, Result, bail};

use crate::{
    backend::{MuxBackend, PaneCapture, PaneInput, PaneText},
    command::MuxCommand,
    snapshot::MuxSnapshot,
};

use super::space_protocol::{PaneRequest, encode_command};
use bootty_host::remote::{REMOTE_DAEMON_PROGRAM, RemoteHost, remote_daemon_failure};
use bootty_host::{CommandRunner, SystemCommandRunner};

const REMOTE_SPACE_SUBCOMMAND: &str = "remote-space";

pub struct RemoteSpaceBackend {
    remote: RemoteHost,
    space_id: String,
    backend: MuxBackendKind,
}

impl RemoteSpaceBackend {
    pub fn new(remote: RemoteHost, space_id: impl Into<String>, backend: MuxBackendKind) -> Self {
        Self {
            remote,
            space_id: space_id.into(),
            backend,
        }
    }

    fn run(&self, args: &[String]) -> Result<String> {
        self.remote.ensure_daemon()?;
        let (program, args) = self.remote.proxy_command(REMOTE_DAEMON_PROGRAM, args)?;
        let output = SystemCommandRunner.run(&program, &args)?;
        if output.success {
            return Ok(output.stdout);
        }
        bail!(
            "{}",
            remote_daemon_failure(self.remote.host(), &output.stderr)
        )
    }
}

impl MuxBackend for RemoteSpaceBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let output = self.run(&[
            REMOTE_SPACE_SUBCOMMAND.to_owned(),
            "snapshot".to_owned(),
            "--id".to_owned(),
            self.space_id.clone(),
            "--backend".to_owned(),
            backend_name(self.backend).to_owned(),
        ])?;
        serde_json::from_str(&output).context("decode remote Space snapshot")
    }

    // An explicit create's `argv` needs daemon protocol 14. The daemon path is versioned by that
    // protocol, so this client never reaches an older daemon that would drop the field.
    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.run(&[
            REMOTE_SPACE_SUBCOMMAND.to_owned(),
            "execute".to_owned(),
            "--id".to_owned(),
            self.space_id.clone(),
            "--backend".to_owned(),
            backend_name(self.backend).to_owned(),
            "--payload".to_owned(),
            encode_command(&command)?,
        ])?;
        Ok(())
    }

    fn send_pane_input(&self, pane_id: &str, input: &PaneInput) -> Result<()> {
        self.pane(&PaneRequest::Input {
            pane: pane_id.to_owned(),
            input: input.clone(),
        })
        .map(|_| ())
    }

    fn capture_pane(&self, pane_id: &str, capture: PaneCapture) -> Result<PaneText> {
        self.pane(&PaneRequest::Capture {
            pane: pane_id.to_owned(),
            capture,
        })?
        .context("the remote daemon returned no capture")
    }
}

impl RemoteSpaceBackend {
    /// The daemon checks that the pane belongs to a session this Space holds, then runs the
    /// request with the same backend implementation a local binding uses.
    fn pane(&self, request: &PaneRequest) -> Result<Option<PaneText>> {
        request.send(
            &self.remote,
            &[
                REMOTE_SPACE_SUBCOMMAND.to_owned(),
                "pane".to_owned(),
                "--id".to_owned(),
                self.space_id.clone(),
                "--backend".to_owned(),
                backend_name(self.backend).to_owned(),
            ],
        )
    }
}

const fn backend_name(backend: MuxBackendKind) -> &'static str {
    match backend {
        MuxBackendKind::Herdr => "herdr",
        MuxBackendKind::Native => "native",
        MuxBackendKind::Rmux => "rmux",
        MuxBackendKind::Tmux => "tmux",
    }
}
