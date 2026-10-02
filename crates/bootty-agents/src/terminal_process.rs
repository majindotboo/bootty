use std::{
    io::{self, Read},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

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
    pub successful: bool,
}

pub fn query_output(
    program: &str,
    arguments: &[&str],
    cancelled: impl Fn() -> bool,
) -> Result<QueryOutput, String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Provider stdout is unavailable")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Provider stderr is unavailable")?;
    let (sender, receiver) = mpsc::sync_channel(2);
    let error_sender = sender.clone();
    thread::spawn(move || {
        let _ = sender.send((false, drain(stdout)));
    });
    thread::spawn(move || {
        let _ = error_sender.send((true, drain(stderr)));
    });
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .ok_or("Provider query deadline is invalid")?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline && !cancelled() => {
                thread::sleep(Duration::from_millis(5));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(result.err().map_or_else(
                    || "Provider query timed out".to_owned(),
                    |error| error.to_string(),
                ));
            }
        }
    };
    let mut bytes = Vec::new();
    let mut error_bytes = Vec::new();
    for _ in 0..2 {
        let (stderr, result) = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "Provider output did not close before the query deadline")?;
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
        successful: status?.success(),
    })
}

pub fn query(
    program: &str,
    arguments: &[&str],
    cancelled: impl Fn() -> bool,
) -> Result<Vec<u8>, String> {
    let output = query_output(program, arguments, cancelled)?;
    if !output.successful {
        return Err("Provider query failed".to_owned());
    }
    Ok(output.stdout)
}
