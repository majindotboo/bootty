use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Result, bail};

static INTERRUPTED: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();

/// # Errors
/// Returns an error if the operating system rejects interrupt handler registration.
pub fn install() -> Result<()> {
    let flag = INTERRUPTED
        .get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone();
    match INSTALLED.get_or_init(|| {
        signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone())
            .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        signal_hook::flag::register(signal_hook::consts::SIGTERM, flag)
            .map_err(|error| error.to_string())?;
        Ok(())
    }) {
        Ok(()) => Ok(()),
        Err(error) => bail!("failed to install interrupt handler: {error}"),
    }
}

pub fn interrupted() -> bool {
    INTERRUPTED
        .get()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
}

#[derive(Debug)]
pub struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("interrupted")
    }
}

impl std::error::Error for Interrupted {}

#[must_use]
pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| signal_exit_code(status))
}

#[cfg(unix)]
fn signal_exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .signal()
        .map_or(1, |signal| 128_i32.saturating_add(signal))
}

#[cfg(not(unix))]
fn signal_exit_code(_status: ExitStatus) -> i32 {
    1
}
