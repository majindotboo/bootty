use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use std::{
    io::{self, Read},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub fn spawn_query(mut command: Command) -> Result<ProcessTreeChild, String> {
    configure_group(&mut command);
    ProcessTreeChild::spawn_with_console_window(&mut command, ConsoleWindowBehavior::Suppress)
        .map_err(|error| error.to_string())
}

pub fn terminate_query(child: &mut ProcessTreeChild) {
    let _ = child.terminate();
    let _ = child.wait();
}

/// Provider launchers may spawn a native child. Keep that tree in our own group.
#[cfg(unix)]
pub fn configure_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
pub const fn configure_group(_command: &mut Command) {}

/// Private stdin is an owner lifetime pipe; the provider keeps its existing null stdin.
/// EOF after owner death terminates only this supervisor's isolated process group.
#[cfg(unix)]
pub fn parent_bound_command(program: &str) -> Command {
    const SUPERVISE: &str = r#"
exec 3<&0
(
    while IFS= read -r owner; do :; done
    /bin/kill -KILL -- "-$$"
) <&3 &
watcher=$!
"$@" </dev/null 3<&- &
provider=$!
exec 3<&-
wait "$provider"
result=$?
kill "$watcher" 2>/dev/null
wait "$watcher" 2>/dev/null
exit "$result"
"#;
    let mut command = Command::new("/bin/sh");
    command.args(["-c", SUPERVISE, "bootty-provider-owner", program]);
    command.stdin(Stdio::piped());
    configure_group(&mut command);
    command
}

const OUTPUT_LIMIT: usize = 1024 * 1024;

fn drain(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = pipe.read(&mut buffer)?;
        if count == 0 {
            return Ok(retained);
        }
        let available = OUTPUT_LIMIT
            .saturating_add(1)
            .saturating_sub(retained.len());
        if let Some(chunk) = buffer.get(..count.min(available)) {
            retained.extend_from_slice(chunk);
        }
    }
}

/// Provider queries run on workers; both pipes drain even if retained output is full.
pub struct QueryOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub successful: bool,
}

pub fn query_output_using(
    program: &str,
    arguments: &[&str],
    environment: Option<(&str, &str)>,
    transport: &crate::TerminalProviderTransport,
    cancelled: impl Fn() -> bool,
) -> Result<QueryOutput, String> {
    let _provider_use = crate::ProviderUse::acquire(program)?;
    let mut command = bootty_host::local_command(program);
    if let Some((name, value)) = environment {
        command.env(name, value);
    }
    command.args(arguments);
    (transport.output)(command, &cancelled)
}

pub fn query_output_command(
    mut command: Command,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> Result<QueryOutput, String> {
    if cancelled() {
        return Err("Provider query cancelled".to_owned());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_query(command)?;
    let mut terminated = false;
    let mut drainers = Vec::new();
    let mut completed = Vec::new();
    let (sender, receiver) = mpsc::sync_channel(2);
    let result = (|| {
        let stdout = child
            .child_mut()
            .stdout
            .take()
            .ok_or("Provider stdout is unavailable")?;
        let stderr = child
            .child_mut()
            .stderr
            .take()
            .ok_or("Provider stderr is unavailable")?;
        let output_sender = sender.clone();
        let error_sender = sender.clone();
        drainers.push((
            false,
            thread::spawn(move || {
                let _ = output_sender.send((false, drain(stdout)));
            }),
        ));
        drainers.push((
            true,
            thread::spawn(move || {
                let _ = error_sender.send((true, drain(stderr)));
            }),
        ));
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("Provider query deadline is invalid")?;
        let status = loop {
            match child.child_mut().try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if cancelled() => return Err("Provider query cancelled".to_owned()),
                Ok(None) if Instant::now() >= deadline => {
                    return Err("Provider query timed out".to_owned());
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(error) => return Err(error.to_string()),
            }
        };
        // A completed query cannot leave native launcher descendants holding its pipes.
        terminate_query(&mut child);
        terminated = true;
        let mut bytes = Vec::new();
        let mut error_bytes = Vec::new();
        for _ in 0..2 {
            let (stderr, result) = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "Provider output did not close before the query deadline")?;
            completed.push(stderr);
            if stderr {
                error_bytes = result.map_err(|error| error.to_string())?;
            } else {
                bytes = result.map_err(|error| error.to_string())?;
            }
        }
        if bytes.len() > OUTPUT_LIMIT || error_bytes.len() > OUTPUT_LIMIT {
            return Err("Provider query exceeds 1 MiB".to_owned());
        }
        Ok(QueryOutput {
            stdout: bytes,
            stderr: error_bytes,
            exit_code: status.code(),
            successful: status.success(),
        })
    })();
    if !terminated {
        terminate_query(&mut child);
    }
    join_query_drainers(drainers, &receiver, completed)?;
    result
}

// Join only readers that reported EOF. A writer that escaped the owned process group cannot
// make worker teardown wait forever; it instead reports an unsupported pipe lifetime.
fn join_query_drainers(
    drainers: Vec<(bool, thread::JoinHandle<()>)>,
    receiver: &mpsc::Receiver<(bool, io::Result<Vec<u8>>)>,
    mut completed: Vec<bool>,
) -> Result<(), String> {
    let cleanup_deadline = Instant::now().checked_add(Duration::from_secs(5));
    while completed.len() < drainers.len() {
        let Some(remaining) =
            cleanup_deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
        else {
            break;
        };
        let Ok((stderr, _)) = receiver.recv_timeout(remaining) else {
            break;
        };
        completed.push(stderr);
    }
    let mut all_drained = true;
    for (stderr, worker) in drainers {
        if completed.contains(&stderr) {
            if worker.join().is_err() {
                all_drained = false;
            }
        } else {
            all_drained = false;
        }
    }
    if all_drained {
        Ok(())
    } else {
        Err("Provider output did not close after owned query teardown".to_owned())
    }
}
