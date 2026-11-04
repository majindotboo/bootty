//! Serializes descriptor creation with child creation.
use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

/// Hold from before creating pipes or PTYs through spawn completion, never while waiting.
///
/// Unix openpty (including Linux) and macOS pipes set CLOEXEC after creation.
/// Remove this lock when all descriptor creation uses atomic CLOEXEC APIs.
#[must_use]
pub fn lock() -> SpawnGuard {
    #[cfg(unix)]
    {
        static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SpawnGuard {
            _guard: SPAWN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }
    #[cfg(not(unix))]
    {
        SpawnGuard {}
    }
}

/// Releases the process-wide spawn lock when dropped.
pub struct SpawnGuard {
    #[cfg(unix)]
    _guard: std::sync::MutexGuard<'static, ()>,
}

/// Child creation that participates in the process-wide descriptor lock.
pub trait CommandSpawn {
    /// # Errors
    /// Returns the child creation failure.
    fn bootty_spawn(&mut self) -> io::Result<Child>;
    /// # Errors
    /// Returns the child creation or wait failure.
    fn bootty_status(&mut self) -> io::Result<ExitStatus>;
    /// Always nulls stdin and captures stdout and stderr, replacing any stdio the
    /// caller configured; unlike `Command::output`, configured redirects are not kept.
    /// # Errors
    /// Returns the child creation or output collection failure.
    fn bootty_output(&mut self) -> io::Result<Output>;
}

impl CommandSpawn for Command {
    #[expect(clippy::disallowed_methods, reason = "The spawn lock is held here")]
    fn bootty_spawn(&mut self) -> io::Result<Child> {
        let _spawn = lock();
        self.spawn()
    }
    fn bootty_status(&mut self) -> io::Result<ExitStatus> {
        self.bootty_spawn()?.wait()
    }
    fn bootty_output(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.bootty_spawn()?.wait_with_output()
    }
}
