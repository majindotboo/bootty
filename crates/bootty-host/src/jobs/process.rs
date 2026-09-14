use super::{Job, JobSpec, JobStatus, JobStream};
use anyhow::{Context as _, Result, ensure};
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::{atomic::Ordering, mpsc},
    time::Duration,
};

pub(super) fn run(job: &Job, spec: &JobSpec) -> Result<()> {
    ensure!(
        std::path::Path::new(&spec.cwd).is_absolute(),
        "job working directory must be absolute on its host"
    );
    ensure!(
        !job.cancel.load(Ordering::Acquire),
        "job cancelled before launch"
    );
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        ProcessTreeChild::spawn_with_console_window(&mut command, ConsoleWindowBehavior::Suppress)
            .context("spawn job")?;
    job.control(child.controller())?;
    job.status(JobStatus::Running {
        pid: child.child_mut().id(),
    });
    let (sender, receiver) = mpsc::sync_channel(64);
    let readers = [
        reader(
            child.child_mut().stdout.take().context("job stdout")?,
            JobStream::Stdout,
            sender.clone(),
        )?,
        reader(
            child.child_mut().stderr.take().context("job stderr")?,
            JobStream::Stderr,
            sender,
        )?,
    ];
    let status = loop {
        if job.cancel.load(Ordering::Acquire) {
            child.terminate()?;
        }
        if child.has_exited()? {
            // A batch job owns its descendants too. Close inherited pipes before reaping the leader.
            child.terminate()?;
            break child.wait()?;
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok((stream, bytes)) => job.output(stream, &bytes),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    for (stream, bytes) in receiver {
        job.output(stream, &bytes);
    }
    for reader in readers {
        reader
            .join()
            .map_err(|_| anyhow::anyhow!("job output worker stopped"))??;
    }
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    job.status(JobStatus::Exited {
        code: status.code(),
        signal,
    });
    Ok(())
}
fn reader(
    mut pipe: impl Read + Send + 'static,
    stream: JobStream,
    sender: mpsc::SyncSender<(JobStream, Vec<u8>)>,
) -> std::io::Result<std::thread::JoinHandle<std::io::Result<()>>> {
    std::thread::Builder::new()
        .name("job-output".to_owned())
        .spawn(move || {
            let mut bytes = [0; 16 * 1024];
            loop {
                let size = pipe.read(&mut bytes)?;
                if size == 0
                    || sender
                        .send((
                            stream,
                            bytes
                                .get(..size)
                                .ok_or_else(|| {
                                    std::io::Error::other("job output read exceeded buffer")
                                })?
                                .to_vec(),
                        ))
                        .is_err()
                {
                    return Ok(());
                }
            }
        })
}
