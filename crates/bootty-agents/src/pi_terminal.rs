use std::io;
use std::path::Path;

use crate::AgentLaunch;
use crate::terminal_observation::ObservationSink;

/// A native extension and its private, per-terminal observation channel.
pub struct PiTerminalObserver {
    arguments: Vec<String>,
    #[cfg(unix)]
    runtime: unix::Runtime,
}

impl PiTerminalObserver {
    /// # Errors
    /// Returns an error if private runtime files or the observation transport cannot be created.
    /// Windows requires a private local transport implementation before observation is supported.
    pub fn prepare(
        launch: &AgentLaunch,
        runtime_directory: &Path,
        sink: ObservationSink,
    ) -> io::Result<Self> {
        launch
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        #[cfg(unix)]
        {
            let runtime = unix::Runtime::prepare(runtime_directory, sink)?;
            let extension = runtime.directory.join("extension.ts");
            let extension = extension.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Pi extension path must be UTF-8",
                )
            })?;
            let mut arguments = vec!["--extension".to_owned(), extension.to_owned()];
            arguments.extend(launch.arguments.iter().cloned());
            Ok(Self { arguments, runtime })
        }
        #[cfg(not(unix))]
        {
            let _ = (runtime_directory, sink);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Pi terminal observation requires a private Unix socket",
            ))
        }
    }

    #[must_use]
    pub fn arguments(&self) -> Vec<String> {
        self.arguments.clone()
    }

    pub fn stop(&self) {
        #[cfg(unix)]
        self.runtime.stop();
    }
}

#[cfg(unix)]
mod unix {
    use std::fs::{self, DirBuilder, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use base64::Engine as _;
    use serde::{Deserialize, Serialize};

    use super::{Path, io};
    use crate::terminal_observation::{AgentObservation, ObservationSink, TerminalAgentStatus};

    const MAX_EVENT_BYTES: usize = 16 * 1024;
    const READ_DEADLINE: Duration = Duration::from_millis(250);

    pub(super) struct Runtime {
        pub(super) directory: PathBuf,
        stopped: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Connection {
        socket_path: PathBuf,
        token: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct Snapshot {
        token: String,
        session_id: String,
        session_file: Option<String>,
        status: Activity,
        detail: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Activity {
        Idle,
        Working,
        Waiting,
        Finished,
        Stopped,
        Error,
    }

    impl Runtime {
        pub(super) fn prepare(directory: &Path, sink: ObservationSink) -> io::Result<Self> {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
            let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
            // Unix socket paths are short; exclusive creation still prevents directory reuse.
            let directory =
                directory.join(format!("p-{}", token.chars().take(8).collect::<String>()));
            DirBuilder::new().mode(0o700).create(&directory)?;
            let setup = || -> io::Result<UnixListener> {
                let socket_path = directory.join("events.sock");
                let listener = UnixListener::bind(&socket_path)?;
                fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
                listener.set_nonblocking(true)?;
                let connection = Connection {
                    socket_path,
                    token: token.clone(),
                };
                write_private(
                    &directory.join("connection.json"),
                    &serde_json::to_vec(&connection)?,
                )?;
                write_private(
                    &directory.join("extension.ts"),
                    include_bytes!("assets/pi-terminal.ts"),
                )?;
                Ok(listener)
            };
            let listener = match setup() {
                Ok(listener) => listener,
                Err(error) => {
                    let _ = fs::remove_dir_all(&directory);
                    return Err(error);
                }
            };
            let stopped = Arc::new(AtomicBool::new(false));
            let worker_stopped = stopped.clone();
            let worker = thread::Builder::new()
                .name("bootty-pi-observer".to_owned())
                .spawn(move || {
                    while !worker_stopped.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if let Some(observation) =
                                    read_snapshot(stream, &token, &worker_stopped)
                                    && !worker_stopped.load(Ordering::Acquire)
                                {
                                    sink(observation);
                                }
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                thread::park_timeout(Duration::from_millis(20));
                            }
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => break,
                        }
                    }
                });
            match worker {
                Ok(worker) => Ok(Self {
                    directory,
                    stopped,
                    worker: Some(worker),
                }),
                Err(error) => {
                    let _ = fs::remove_dir_all(&directory);
                    Err(error)
                }
            }
        }

        pub(super) fn stop(&self) {
            self.stopped.store(true, Ordering::Release);
            if let Some(worker) = &self.worker {
                worker.thread().unpark();
            }
        }
    }

    impl Drop for Runtime {
        fn drop(&mut self) {
            self.stop();
            // The worker observes cancellation; disposal must not wait on socket I/O.
            drop(self.worker.take());
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?
            .write_all(bytes)
    }

    fn read_snapshot(
        mut stream: UnixStream,
        token: &str,
        stopped: &AtomicBool,
    ) -> Option<AgentObservation> {
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .ok()?;
        let deadline = Instant::now().checked_add(READ_DEADLINE)?;
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            match stream.read(&mut buffer) {
                Ok(0) => return None,
                Ok(count) => {
                    let chunk = buffer.get(..count)?;
                    if bytes.len().saturating_add(count) > MAX_EVENT_BYTES {
                        return None;
                    }
                    bytes.extend_from_slice(chunk);
                    if bytes.last() == Some(&b'\n') {
                        let snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
                        if snapshot.token != token
                            || !valid_value(&snapshot.session_id, 512)
                            || snapshot
                                .session_file
                                .as_ref()
                                .is_some_and(|file| !valid_value(file, 8192))
                            || snapshot
                                .detail
                                .as_ref()
                                .is_some_and(|detail| !valid_value(detail, 256))
                        {
                            return None;
                        }
                        return Some(AgentObservation {
                            session_id: Some(snapshot.session_id),
                            session_file: snapshot.session_file,
                            status: match snapshot.status {
                                Activity::Idle => TerminalAgentStatus::Idle,
                                Activity::Working => TerminalAgentStatus::Working,
                                Activity::Waiting => TerminalAgentStatus::Waiting,
                                Activity::Finished => TerminalAgentStatus::Finished,
                                Activity::Stopped => TerminalAgentStatus::Stopped,
                                Activity::Error => TerminalAgentStatus::Error,
                            },
                            detail: snapshot.detail,
                        });
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return None,
            }
        }
        None
    }

    fn valid_value(value: &str, maximum: usize) -> bool {
        !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
    }
}
