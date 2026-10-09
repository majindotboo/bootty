use super::{
    FileCancellation, FileDescriptor, MAX_FILE_CHUNK, check_revision, open_file, read_range,
    validate_descriptor,
};
use crate::{CancellableCommandRunner, CommandCancellation, remote::RemoteHost};
use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const HEADER_LIMIT: u64 = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Range {
    offset: u64,
    length: u32,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Ready { len: u64, revision: String },
    Data { length: u32 },
    Error { message: String },
}

fn line<T: serde::de::DeserializeOwned>(input: &mut impl BufRead) -> Result<Option<T>> {
    let mut line = String::new();
    let size = input.take(HEADER_LIMIT).read_line(&mut line)?;
    if size == 0 {
        return Ok(None);
    }
    ensure!(
        size < usize::try_from(HEADER_LIMIT)? && line.ends_with('\n'),
        "Invalid file frame header"
    );
    Ok(Some(serde_json::from_str(&line)?))
}
fn write_line(value: &impl Serialize, output: &mut impl Write) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

/// Serve one open file source until request EOF. Bodies are raw, bounded binary ranges.
/// # Errors
/// Returns malformed requests, changed files, or stream errors.
pub fn serve(payload: &str, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    ensure!(payload.len() <= 32 * 1024, "File request exceeds its bound");
    let descriptor: FileDescriptor = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    serve_descriptor(&descriptor, &mut input, &mut output)
}

/// Serve bounded ranges from an already decoded source descriptor.
/// # Errors
/// Returns malformed requests, changed files, or stream errors.
pub fn serve_descriptor(
    descriptor: &FileDescriptor,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<()> {
    let opened = (|| {
        validate_descriptor(descriptor)?;
        let file = open_file(&descriptor.path)?;
        check_revision(&file, descriptor)?;
        Ok(file)
    })();
    let mut file = report_error(opened, &mut output)?;
    write_line(
        &Response::Ready {
            len: descriptor.len,
            revision: descriptor.revision.clone(),
        },
        &mut output,
    )?;
    let mut bytes = Vec::with_capacity(MAX_FILE_CHUNK);
    loop {
        let requested = (|| {
            let Some(request) = line::<Range>(&mut input)? else {
                return Ok(None);
            };
            let length = usize::try_from(request.length)?;
            ensure!(length <= MAX_FILE_CHUNK, "File range exceeds its bound");
            bytes.resize(length, 0);
            read_range(&mut file, descriptor, request.offset, &mut bytes)?;
            Ok(Some(request.length))
        })();
        let Some(length) = report_error(requested, &mut output)? else {
            return Ok(());
        };
        // Once a body starts, IO failure ends the stream; an error frame would corrupt that body.
        write_line(&Response::Data { length }, &mut output)?;
        output.write_all(&bytes)?;
        output.flush()?;
    }
}
fn report_error<T>(result: Result<T>, output: &mut impl Write) -> Result<T> {
    if let Err(error) = &result {
        let message = error.to_string().chars().take(1024).collect();
        let _ = write_line(&Response::Error { message }, output);
    }
    result
}

pub(super) struct RemoteReader {
    child: ProcessTreeChild,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    cancellation: FileCancellation,
    deadline: mpsc::SyncSender<Option<Instant>>,
    watchdog: Option<JoinHandle<()>>,
    errors: Option<JoinHandle<()>>,
}
impl RemoteReader {
    pub(super) fn open(descriptor: &FileDescriptor, remote: &RemoteHost) -> Result<Self> {
        remote.ensure_daemon_with(&CancellableCommandRunner::with_deadline(
            CommandCancellation::default(),
            Instant::now()
                .checked_add(IO_TIMEOUT)
                .context("File deadline overflow")?,
        ))?;
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(descriptor)?);
        let (program, args) = remote.proxy_command(
            crate::REMOTE_DAEMON_PROGRAM,
            &["file-reader".to_owned(), payload],
        )?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = ProcessTreeChild::spawn_with_console_window(
            &mut command,
            ConsoleWindowBehavior::Suppress,
        )?;
        let input = child.child_mut().stdin.take().context("file input")?;
        let output = BufReader::new(child.child_mut().stdout.take().context("file output")?);
        let mut stderr = child
            .child_mut()
            .stderr
            .take()
            .context("file diagnostics")?;
        // Drain diagnostics without retaining an unbounded remote output buffer.
        let errors = thread::Builder::new()
            .name("file-stderr".to_owned())
            .spawn(move || {
                let _ = io::copy(&mut stderr, &mut io::sink());
            })?;
        let cancellation = FileCancellation {
            cancelled: Arc::default(),
            process: Some(child.controller()),
        };
        let watchdog_cancel = cancellation.clone();
        let (deadline, receive) = mpsc::sync_channel::<Option<Instant>>(1);
        let watchdog = thread::Builder::new()
            .name("file-deadline".to_owned())
            .spawn(move || {
                let mut until: Option<Instant> = None;
                loop {
                    let result = until.map_or_else(
                        || {
                            receive
                                .recv()
                                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                        },
                        |until| {
                            receive.recv_timeout(until.saturating_duration_since(Instant::now()))
                        },
                    );
                    match result {
                        Ok(next) => until = next,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            watchdog_cancel.cancel();
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        let mut reader = Self {
            child,
            input,
            output,
            cancellation,
            deadline,
            watchdog: Some(watchdog),
            errors: Some(errors),
        };
        reader.deadline.send(Some(
            Instant::now()
                .checked_add(IO_TIMEOUT)
                .context("File deadline overflow")?,
        ))?;
        let response = line::<Response>(&mut reader.output)?.context("Missing file handshake")?;
        match response {
            Response::Ready { len, revision } => ensure!(
                len == descriptor.len && revision == descriptor.revision,
                "File handshake revision mismatch"
            ),
            Response::Error { message } => anyhow::bail!("{message}"),
            Response::Data { .. } => anyhow::bail!("Unexpected file handshake"),
        }
        reader.deadline.send(None)?;
        Ok(reader)
    }
    pub(super) const fn cancellation(&self) -> &FileCancellation {
        &self.cancellation
    }
    pub(super) fn read_range(&mut self, offset: u64, bytes: &mut [u8]) -> Result<()> {
        self.cancellation.check()?;
        self.deadline.send(Some(
            Instant::now()
                .checked_add(IO_TIMEOUT)
                .context("File deadline overflow")?,
        ))?;
        let result = (|| {
            write_line(
                &Range {
                    offset,
                    length: u32::try_from(bytes.len())?,
                },
                &mut self.input,
            )?;
            match line::<Response>(&mut self.output)?.context("File transport closed")? {
                Response::Data { length } => ensure!(
                    usize::try_from(length)? == bytes.len(),
                    "File response length mismatch"
                ),
                Response::Error { message } => anyhow::bail!("{message}"),
                Response::Ready { .. } => anyhow::bail!("Unexpected file response"),
            }
            self.output.read_exact(bytes).context("Truncated file body")
        })();
        if result.is_err() {
            self.cancellation.cancel();
        }
        let _ = self.deadline.send(None);
        result
    }
}
impl Drop for RemoteReader {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let _ = self.child.wait();
        // Disconnect the watchdog even when it is idle and waiting without a deadline.
        let (replacement, _) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.deadline, replacement));
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
        if let Some(errors) = self.errors.take() {
            let _ = errors.join();
        }
    }
}
