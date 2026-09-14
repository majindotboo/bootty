use super::{Job, JobRead, JobRegistry, JobSpec, JobStatus};
use crate::{CancellableCommandRunner, CommandCancellation, remote::RemoteHost};
use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
    sync::{Arc, atomic::Ordering},
};

pub(super) fn run(job: &Arc<Job>, spec: &JobSpec, remote: &RemoteHost) -> Result<()> {
    let cancelled = job.clone();
    let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
        CommandCancellation::default(),
        job.deadline,
        move || cancelled.cancel.load(Ordering::Acquire),
    );
    remote.ensure_daemon_with(&runner)?;
    ensure!(
        !job.cancel.load(Ordering::Acquire),
        "job cancelled before remote launch"
    );
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(spec)?);
    let (program, args) =
        remote.proxy_command(crate::REMOTE_DAEMON_PROGRAM, &["job".to_owned(), payload])?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        ProcessTreeChild::spawn_with_console_window(&mut command, ConsoleWindowBehavior::Suppress)?;
    job.control(child.controller())?;
    let _stdin = child.child_mut().stdin.take().context("remote job input")?;
    let stderr = child
        .child_mut()
        .stderr
        .take()
        .context("remote job diagnostics")?;
    let errors = std::thread::Builder::new()
        .name("remote-job-errors".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut pipe = stderr;
            let mut chunk = [0; 4096];
            while let Ok(size) = pipe.read(&mut chunk) {
                if size == 0 {
                    break;
                }
                let keep = size.min((64 * 1024_usize).saturating_sub(bytes.len()));
                bytes.extend(chunk.iter().take(keep));
            }
            bytes
        })?;
    let mut output = BufReader::new(
        child
            .child_mut()
            .stdout
            .take()
            .context("remote job output")?,
    );
    let result = (|| {
        loop {
            if job.cancel.load(Ordering::Acquire) {
                child.terminate()?;
            }
            let mut line = String::new();
            let size = output.by_ref().take(128 * 1024).read_line(&mut line)?;
            ensure!(
                size > 0 && size < 128 * 1024 && line.ends_with('\n'),
                "remote job ended without a complete result"
            );
            let batch: JobRead = serde_json::from_str(&line).context("decode remote job stream")?;
            ensure!(
                !batch.gap,
                "remote job output exceeded retention before delivery"
            );
            for chunk in batch.chunks {
                job.output(chunk.stream, &super::STANDARD.decode(chunk.data)?);
            }
            {
                let mut state = job
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.summary.cancel_requested |= batch.job.cancel_requested;
                state.summary.timed_out |= batch.job.timed_out;
            }
            if batch.job.status.finished() && batch.cursor == batch.job.next_cursor {
                return Ok(batch.job.status);
            }
            if matches!(batch.job.status, JobStatus::Running { .. }) {
                job.status(batch.job.status);
            }
        }
    })();
    // Terminate/reap the transport on every path, including malformed or truncated streams.
    let _ = child.terminate();
    let _ = child.wait();
    let errors = errors.join().unwrap_or_default();
    match result {
        Ok(status) => {
            job.status(status);
            Ok(())
        }
        Err(error) => Err(error)
            .with_context(|| format!("remote job transport: {}", String::from_utf8_lossy(&errors))),
    }
}

/// The remote daemon owns the job tree. Losing the request stream cancels that tree.
/// # Errors
/// Returns invalid request, job startup, output streaming, or job protocol errors.
pub fn serve(payload: &str) -> Result<()> {
    ensure!(payload.len() <= 96 * 1024, "job request exceeds the limit");
    let spec: JobSpec = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    let registry = Arc::new(JobRegistry::default());
    let job = registry.start(spec, None, Arc::new(|| {}))?;
    let cancellation = registry.clone();
    let id = job.id.clone();
    std::thread::spawn(move || {
        let mut input = [0; 1];
        let _ = std::io::stdin().read(&mut input);
        let _ = cancellation.cancel(&id);
    });
    let result = (|| {
        let mut cursor = 0;
        let mut output = std::io::stdout().lock();
        loop {
            let batch = registry.read(&job.id, cursor, 4000)?;
            cursor = batch.cursor;
            serde_json::to_writer(&mut output, &batch)?;
            output.write_all(b"\n")?;
            output.flush()?;
            if batch.job.status.finished() && cursor == batch.job.next_cursor {
                return Ok(());
            }
        }
    })();
    let _ = registry.cancel(&job.id);
    result
}
