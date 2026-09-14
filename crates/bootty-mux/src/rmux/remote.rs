//! Runs Bootty's embedded rmux backend through the small remote Bootty daemon.
//!
//! The remote host never resolves or executes an `rmux` binary. Bootty serializes backend requests,
//! sends them through SSH, and handles them with the same embedded rmux SDK path used locally.

#[cfg(feature = "terminal-runtime")]
use std::io::BufReader;
use std::io::{BufRead, BufWriter, Write};
#[cfg(feature = "terminal-runtime")]
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
#[cfg(feature = "terminal-runtime")]
use bootty_host::{CommandOutput, CommandRunner, SystemCommandRunner};
use rmux_sdk::TerminalSizeSpec;
use serde::{Deserialize, Serialize};
#[cfg(feature = "terminal-runtime")]
use tokio::sync::mpsc as tokio_mpsc;

use super::backend::RmuxBackend;
use super::bridge::rmux_execute;
#[cfg(feature = "terminal-runtime")]
use super::pane_io::{RMUX_OUTPUT_CHANNEL_CAPACITY, RmuxPaneIo};
use super::pane_io::{RmuxPaneEvent, RmuxPaneTarget, open_rmux_pane_io, resize_rmux_pane};
use crate::command::MuxCommand;
#[cfg(feature = "terminal-runtime")]
use crate::{backend::MuxBackend, snapshot::MuxSnapshot};
#[cfg(feature = "terminal-runtime")]
use bootty_host::remote::{RemoteHost, remote_daemon_failure};

#[cfg(feature = "terminal-runtime")]
const REMOTE_RMUX_SUBCOMMAND: &str = "remote-rmux";
const MAX_REMOTE_RMUX_PAYLOAD: usize = 1024 * 1024;

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum RemoteRmuxRequest {
    Snapshot,
    Execute {
        command: MuxCommand,
    },
    PaneStream {
        session: String,
        pane: String,
    },
    PaneInput {
        session: String,
        pane: String,
    },
    Resize {
        session: String,
        pane: String,
        cols: u16,
        rows: u16,
    },
    ResizeWindow {
        window: String,
        cols: u16,
        rows: u16,
    },
}

#[derive(Debug, Deserialize, Serialize)]
enum RemotePaneFrame {
    Rebase(String),
    Bytes(String),
    ProcessExited,
    End(Option<String>),
    Error(String),
}

#[cfg(feature = "terminal-runtime")]
pub struct RemoteRmuxBackend {
    remote: RemoteHost,
}

#[cfg(feature = "terminal-runtime")]
impl RemoteRmuxBackend {
    pub const fn new(remote: RemoteHost) -> Self {
        Self { remote }
    }

    fn run(&self, request: &RemoteRmuxRequest) -> Result<CommandOutput> {
        self.remote.ensure_daemon()?;
        let (program, args) = remote_rmux_argv(&self.remote, request)?;
        let output = SystemCommandRunner.run(&program, &args)?;
        if output.success {
            return Ok(output);
        }
        bail!(
            "{}",
            remote_daemon_failure(self.remote.host(), &output.stderr)
        )
    }
}

#[cfg(feature = "terminal-runtime")]
pub fn resize_remote_rmux_window(
    remote: &RemoteHost,
    window: &str,
    cols: u16,
    rows: u16,
) -> Result<()> {
    RemoteRmuxBackend::new(remote.clone()).run(&RemoteRmuxRequest::ResizeWindow {
        window: window.to_owned(),
        cols,
        rows,
    })?;
    Ok(())
}

#[cfg(feature = "terminal-runtime")]
impl MuxBackend for RemoteRmuxBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let output = self.run(&RemoteRmuxRequest::Snapshot)?;
        serde_json::from_str(&output.stdout).context("decode remote Space snapshot")
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.run(&RemoteRmuxRequest::Execute { command })?;
        Ok(())
    }
}

#[cfg(feature = "terminal-runtime")]
pub fn open_remote_rmux_pane_io(
    remote: &RemoteHost,
    target: &RmuxPaneTarget,
) -> Result<RmuxPaneIo> {
    let pane = target.pane_selector().map(str::to_owned).with_context(|| {
        format!(
            "remote terminal session {} has no pane to attach",
            target.session_selector()
        )
    })?;
    remote.ensure_daemon()?;
    let session = target.session_selector().to_owned();
    let (output_tx, output_rx) = tokio_mpsc::channel(RMUX_OUTPUT_CHANNEL_CAPACITY);
    let (input_tx, input_rx) = tokio_mpsc::unbounded_channel();
    let (resize_tx, resize_rx) = tokio_mpsc::unbounded_channel();
    let (result_tx, result_rx) = tokio_mpsc::unbounded_channel();

    spawn_output(
        remote,
        session.clone(),
        pane.clone(),
        output_tx,
        result_tx.clone(),
    )?;
    spawn_input(
        remote,
        session.clone(),
        pane.clone(),
        input_rx,
        result_tx.clone(),
    )?;
    spawn_resize(remote, session, pane, resize_rx, result_tx);

    Ok(RmuxPaneIo {
        output_rx,
        input_tx,
        resize_tx,
        result_rx,
    })
}

#[cfg(feature = "terminal-runtime")]
fn remote_rmux_argv(
    remote: &RemoteHost,
    request: &RemoteRmuxRequest,
) -> Result<(String, Vec<String>)> {
    let payload = request.encode()?;
    remote.proxy_command(
        bootty_host::REMOTE_DAEMON_PROGRAM,
        &[REMOTE_RMUX_SUBCOMMAND.to_owned(), payload],
    )
}

impl RemoteRmuxRequest {
    /// # Errors
    /// Returns an error for invalid base64 or a malformed remote request.
    pub fn decode(payload: &str) -> Result<Self> {
        if payload.len() > MAX_REMOTE_RMUX_PAYLOAD * 2 {
            bail!("remote terminal request is too large")
        }
        let json = URL_SAFE_NO_PAD
            .decode(payload)
            .context("decode remote terminal request")?;
        serde_json::from_slice(&json).context("parse remote terminal request")
    }

    #[cfg(feature = "terminal-runtime")]
    /// # Errors
    /// Returns an error if request serialization fails.
    pub fn encode(&self) -> Result<String> {
        let json = serde_json::to_vec(self).context("encode remote terminal request")?;
        if json.len() > MAX_REMOTE_RMUX_PAYLOAD {
            bail!("remote terminal request is too large")
        }
        Ok(URL_SAFE_NO_PAD.encode(json))
    }
}
/// # Errors
/// Returns malformed request, connection, or backend command errors.
pub fn run_remote_rmux_command(payload: &str) -> Result<i32> {
    match RemoteRmuxRequest::decode(payload)? {
        RemoteRmuxRequest::Snapshot => {
            println!(
                "{}",
                serde_json::to_string(&RmuxBackend::new().snapshot()?)?
            );
        }
        RemoteRmuxRequest::Execute { command } => rmux_execute(command)?,
        RemoteRmuxRequest::PaneStream { session, pane } => stream_pane(session, pane)?,
        RemoteRmuxRequest::PaneInput { session, pane } => input_pane(session, pane)?,
        RemoteRmuxRequest::Resize {
            session,
            pane,
            cols,
            rows,
        } => resize_rmux_pane(
            RmuxPaneTarget::new(session, Some(pane)),
            TerminalSizeSpec::new(cols, rows),
        )?,
        RemoteRmuxRequest::ResizeWindow { window, cols, rows } => {
            super::backend::resize_bootty_rmux_window(&window, cols, rows)?;
        }
    }
    Ok(0)
}

fn stream_pane(session: String, pane: String) -> Result<()> {
    let mut io = open_rmux_pane_io(RmuxPaneTarget::new(session, Some(pane)))?;
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    while let Some(event) = io.output_rx.blocking_recv() {
        let frame = pane_frame(event);
        serde_json::to_writer(&mut stdout, &frame)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        if matches!(frame, RemotePaneFrame::End(_) | RemotePaneFrame::Error(_)) {
            break;
        }
    }
    Ok(())
}

fn pane_frame(event: RmuxPaneEvent) -> RemotePaneFrame {
    match event {
        RmuxPaneEvent::Rebase(bytes) => RemotePaneFrame::Rebase(URL_SAFE_NO_PAD.encode(bytes)),
        RmuxPaneEvent::Bytes(bytes) => RemotePaneFrame::Bytes(URL_SAFE_NO_PAD.encode(bytes)),
        RmuxPaneEvent::ProcessExited => RemotePaneFrame::ProcessExited,
        RmuxPaneEvent::End(reason) => RemotePaneFrame::End(reason),
        RmuxPaneEvent::Error(error) => RemotePaneFrame::Error(error),
    }
}

fn input_pane(session: String, pane: String) -> Result<()> {
    let mut io = open_rmux_pane_io(RmuxPaneTarget::new(session, Some(pane)))?;
    thread::spawn(move || while io.output_rx.blocking_recv().is_some() {});
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let bytes = decode_input_line(&line?)?;
        io.input_tx
            .send(bytes)
            .map_err(|_| anyhow::anyhow!("remote terminal input stopped"))?;
        io.result_rx
            .blocking_recv()
            .context("remote terminal input worker stopped")?
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

fn decode_input_line(line: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(line)
        .context("decode remote terminal input")
}

#[cfg(feature = "terminal-runtime")]
fn spawn_output(
    remote: &RemoteHost,
    session: String,
    pane: String,
    output_tx: tokio_mpsc::Sender<RmuxPaneEvent>,
    result_tx: tokio_mpsc::UnboundedSender<std::result::Result<(), String>>,
) -> Result<()> {
    let request = RemoteRmuxRequest::PaneStream { session, pane };
    let (program, args) = remote_rmux_argv(remote, &request)?;
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("stream remote terminal pane")?;
    let stdout = child
        .stdout
        .take()
        .context("remote terminal output stream has no stdout")?;

    thread::spawn(move || {
        let _guard = ChildGuard(child);
        for line in BufReader::new(stdout).lines() {
            let result = line
                .map_err(anyhow::Error::from)
                .and_then(|line| serde_json::from_str::<RemotePaneFrame>(&line).map_err(Into::into))
                .and_then(decode_frame);
            match result {
                Ok(event) => {
                    let ended = matches!(&event, RmuxPaneEvent::End(_));
                    if output_tx.blocking_send(event).is_err() {
                        return;
                    }
                    if ended {
                        return;
                    }
                }
                Err(error) => {
                    let _ = result_tx.send(Err(format!("remote terminal output stopped: {error}")));
                    return;
                }
            }
        }
        let _ = result_tx.send(Err("remote terminal output ended".to_owned()));
    });
    Ok(())
}

#[cfg(feature = "terminal-runtime")]
fn decode_frame(frame: RemotePaneFrame) -> Result<RmuxPaneEvent> {
    Ok(match frame {
        RemotePaneFrame::Rebase(keyframe) => RmuxPaneEvent::Rebase(
            URL_SAFE_NO_PAD
                .decode(keyframe)
                .context("decode remote terminal rebase")?,
        ),
        RemotePaneFrame::Bytes(bytes) => RmuxPaneEvent::Bytes(
            URL_SAFE_NO_PAD
                .decode(bytes)
                .context("decode remote terminal output")?,
        ),
        RemotePaneFrame::ProcessExited => RmuxPaneEvent::ProcessExited,
        RemotePaneFrame::End(reason) => RmuxPaneEvent::End(reason),
        RemotePaneFrame::Error(error) => RmuxPaneEvent::Error(error),
    })
}

#[cfg(feature = "terminal-runtime")]
fn spawn_input(
    remote: &RemoteHost,
    session: String,
    pane: String,
    mut input_rx: tokio_mpsc::UnboundedReceiver<Vec<u8>>,
    result_tx: tokio_mpsc::UnboundedSender<std::result::Result<(), String>>,
) -> Result<()> {
    let request = RemoteRmuxRequest::PaneInput { session, pane };
    let (program, args) = remote_rmux_argv(remote, &request)?;
    let mut child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("open remote terminal input")?;
    let stdin = child
        .stdin
        .take()
        .context("remote terminal input has no stdin")?;

    thread::spawn(move || {
        let _guard = ChildGuard(child);
        let mut writer = BufWriter::new(stdin);
        while let Some(bytes) = input_rx.blocking_recv() {
            if let Err(error) = write_input_line(&mut writer, &bytes) {
                let _ = result_tx.send(Err(format!("remote terminal input stopped: {error}")));
                return;
            }
        }
    });
    Ok(())
}

#[cfg(feature = "terminal-runtime")]
fn write_input_line(writer: &mut BufWriter<ChildStdin>, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(URL_SAFE_NO_PAD.encode(bytes).as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(feature = "terminal-runtime")]
fn spawn_resize(
    remote: &RemoteHost,
    session: String,
    pane: String,
    mut resize_rx: tokio_mpsc::UnboundedReceiver<TerminalSizeSpec>,
    result_tx: tokio_mpsc::UnboundedSender<std::result::Result<(), String>>,
) {
    let remote = remote.clone();
    thread::spawn(move || {
        while let Some(mut size) = resize_rx.blocking_recv() {
            while let Ok(newest) = resize_rx.try_recv() {
                size = newest;
            }
            let request = RemoteRmuxRequest::Resize {
                session: session.clone(),
                pane: pane.clone(),
                cols: size.cols,
                rows: size.rows,
            };
            let result = remote_rmux_argv(&remote, &request).and_then(|(program, args)| {
                let output = SystemCommandRunner.run(&program, &args)?;
                if output.success {
                    Ok(())
                } else {
                    bail!("{}", remote_daemon_failure(remote.host(), &output.stderr))
                }
            });
            let _ = result_tx.send(result.map_err(|error| error.to_string()));
        }
    });
}

#[cfg(feature = "terminal-runtime")]
struct ChildGuard(Child);

#[cfg(feature = "terminal-runtime")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
