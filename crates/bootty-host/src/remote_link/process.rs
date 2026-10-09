use super::protocol::{CHUNK, RemoteOutput, RemoteProcessRequest, RemoteTerminalSize};
use anyhow::{Context as _, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rmux_os::process_tree::ProcessTreeChild;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};
use tokio::sync::mpsc;

pub(super) enum Input {
    Bytes(Vec<u8>),
    Resize(RemoteTerminalSize),
    Eof,
}

pub(super) struct Process {
    pub input: mpsc::Sender<Input>,
    pub output: mpsc::Receiver<RemoteOutput>,
    pub cancel: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for Process {
    fn drop(&mut self) {
        (self.cancel)();
    }
}

pub(super) fn spawn(request: &RemoteProcessRequest) -> Result<Process> {
    request.validate()?;
    let program = if request.program == crate::REMOTE_DAEMON_PROGRAM {
        std::env::current_exe()?.into_os_string()
    } else {
        request.program.clone().into()
    };
    if let Some(size) = request.terminal {
        return spawn_terminal(request, &program, size);
    }
    let mut command = Command::new(program);
    command
        .args(
            crate::exec::daemon_program_args(
                &request.program,
                &request.args,
                bootty_config::ApplicationIdentity::for_process(),
            )
            .as_ref(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    let mut child = ProcessTreeChild::spawn(&mut command).context("start remote process")?;
    let controller = child.controller();
    let cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        _ = controller.terminate();
    });
    let stdin = child.child_mut().stdin.take().context("remote stdin")?;
    let stdout = child.child_mut().stdout.take().context("remote stdout")?;
    let stderr = child.child_mut().stderr.take().context("remote stderr")?;
    let (input, mut input_rx) = mpsc::channel(8);
    let (output_tx, output) = mpsc::channel(8);
    let stdout = read_output(stdout, false, output_tx.clone(), cancel.clone());
    let stderr = read_output(stderr, true, output_tx.clone(), cancel.clone());
    thread::spawn(move || {
        let mut stdin = Some(stdin);
        while let Some(input) = input_rx.blocking_recv() {
            match input {
                Input::Bytes(bytes) => {
                    if let Some(stdin) = &mut stdin
                        && stdin
                            .write_all(&bytes)
                            .and_then(|()| stdin.flush())
                            .is_err()
                    {
                        break;
                    }
                }
                Input::Eof => stdin = None,
                Input::Resize(_) => {}
            }
        }
    });
    thread::spawn(move || {
        // Retain the process-tree anchor while descendants still own captured pipes.
        let complete = matches!((stdout.join(), stderr.join()), (Ok(true), Ok(true)));
        let status = child.wait().map_or(1, |status| status.code().unwrap_or(1));
        _ = output_tx.blocking_send(RemoteOutput::Exit(if complete { status } else { 1 }));
    });
    Ok(Process {
        input,
        output,
        cancel,
    })
}

const fn pty_size(size: RemoteTerminalSize) -> PtySize {
    PtySize {
        cols: size.cols,
        rows: size.rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn spawn_terminal(
    request: &RemoteProcessRequest,
    program: &std::ffi::OsStr,
    size: RemoteTerminalSize,
) -> Result<Process> {
    let pair = native_pty_system().openpty(pty_size(size))?;
    let mut command = CommandBuilder::new(program);
    command.args(
        crate::exec::daemon_program_args(
            &request.program,
            &request.args,
            bootty_config::ApplicationIdentity::for_process(),
        )
        .as_ref(),
    );
    command.env("TERM", "xterm-256color");
    if let Some(cwd) = &request.cwd {
        command.cwd(cwd);
    }
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let control = Arc::new(TerminalControl::new(&*child)?);
    let cancellation = control.clone();
    let cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        cancellation.terminate();
    });
    let reader = pair.master.try_clone_reader()?;
    // Dropping portable-pty's Unix writer injects newline + Ctrl+D. A disconnected
    // tmux attachment must never send those bytes to its backend shell.
    #[cfg(unix)]
    let mut writer: Box<dyn Write + Send> = Box::new(filedescriptor::FileDescriptor::dup(
        &pair.master.as_raw_fd().context("remote PTY descriptor")?,
    )?);
    #[cfg(not(unix))]
    let mut writer = pair.master.take_writer()?;
    let (input, mut input_rx) = mpsc::channel(8);
    let (output_tx, output) = mpsc::channel(8);
    let reader = read_output(reader, false, output_tx.clone(), cancel.clone());
    thread::spawn(move || {
        while let Some(input) = input_rx.blocking_recv() {
            let result = match input {
                Input::Bytes(bytes) => writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .map_err(anyhow::Error::from),
                Input::Resize(size) => pair.master.resize(pty_size(size)),
                // A PTY has no independent stdin half-close; it remains available for output.
                Input::Eof => Ok(()),
            };
            if result.is_err() {
                break;
            }
        }
    });
    thread::spawn(move || {
        let complete = reader.join().unwrap_or(false);
        let code = control
            .wait(&mut *child)
            .ok()
            .and_then(|status| i32::try_from(status.exit_code()).ok())
            .unwrap_or(1);
        _ = output_tx.blocking_send(RemoteOutput::Exit(if complete { code } else { 1 }));
    });
    Ok(Process {
        input,
        output,
        cancel,
    })
}

fn read_output(
    mut reader: impl Read + Send + 'static,
    stderr: bool,
    output: mpsc::Sender<RemoteOutput>,
    cancel: Arc<dyn Fn() + Send + Sync>,
) -> thread::JoinHandle<bool> {
    thread::spawn(move || {
        let mut bytes = vec![0; CHUNK];
        loop {
            let count = match reader.read(&mut bytes) {
                Ok(0) => return true,
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    cancel();
                    _ = output.blocking_send(RemoteOutput::Stderr(
                        format!("Remote output read failed: {error}\n").into_bytes(),
                    ));
                    return false;
                }
            };
            let Some(chunk) = bytes.get(..count) else {
                cancel();
                return false;
            };
            let chunk = chunk.to_vec();
            let frame = if stderr {
                RemoteOutput::Stderr(chunk)
            } else {
                RemoteOutput::Stdout(chunk)
            };
            if output.blocking_send(frame).is_err() {
                cancel();
                return false;
            }
        }
    })
}

// Retain the Unix PID until group cancellation is disarmed. portable-pty's
// direct-child killer does not cover the terminal's process group.
struct TerminalControl {
    #[cfg(unix)]
    pid: Mutex<Option<rustix::process::Pid>>,
    #[cfg(not(unix))]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

impl TerminalControl {
    fn new(child: &dyn portable_pty::Child) -> Result<Self> {
        #[cfg(unix)]
        {
            let pid = child
                .process_id()
                .context("remote terminal process identity")?;
            let pid = rustix::process::Pid::from_raw(i32::try_from(pid)?)
                .context("remote terminal process identity")?;
            Ok(Self {
                pid: Mutex::new(Some(pid)),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                killer: Mutex::new(child.clone_killer()),
            })
        }
    }

    fn terminate(&self) {
        #[cfg(unix)]
        if let Ok(pid) = self.pid.lock()
            && let Some(pid) = *pid
        {
            _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        #[cfg(not(unix))]
        if let Ok(mut killer) = self.killer.lock() {
            _ = killer.kill();
        }
    }

    fn wait(
        &self,
        child: &mut dyn portable_pty::Child,
    ) -> std::io::Result<portable_pty::ExitStatus> {
        #[cfg(unix)]
        loop {
            use rustix::process::{WaitId, WaitIdOptions, waitid};
            let mut guard = self
                .pid
                .lock()
                .map_err(|_| std::io::Error::other("terminal process state poisoned"))?;
            if let Some(pid) = *guard {
                match waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                ) {
                    Ok(Some(_)) => {
                        // Stop remaining owned descendants before releasing the zombie anchor.
                        _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                        guard.take();
                        return child.wait();
                    }
                    Ok(None) => {}
                    Err(error) if error == rustix::io::Errno::INTR => {}
                    Err(error) => {
                        guard.take();
                        return Err(error.into());
                    }
                }
            } else {
                return child.wait();
            }
            drop(guard);
            // Usually already exited when its output closes; poll only a child that closed
            // all terminal descriptors while continuing to work. Cancellation stays available.
            std::thread::park_timeout(std::time::Duration::from_millis(10));
        }
        #[cfg(not(unix))]
        {
            child.wait()
        }
    }
}
