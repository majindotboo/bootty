//! Helpers that outlive the process that started them.

#[cfg(unix)]
use crate::spawn::CommandSpawn as _;
use std::io;
use std::process::{Child, Command};

/// Spawns `command` detached from the launcher.
///
/// Unix starts a new session, so the helper has no controlling terminal. Windows starts it
/// without a console, outside the launcher's job, and without the launcher's standard handles,
/// which would otherwise keep a caller's pipes open until the helper exits.
///
/// # Errors
/// Returns the spawn failure.
pub fn spawn_hidden(command: &mut Command) -> io::Result<Child> {
    #[cfg(unix)]
    {
        unix::detach(command);
        command.bootty_spawn()
    }
    #[cfg(windows)]
    {
        crate::windows::spawn_hidden(command)
    }
}

#[cfg(unix)]
mod unix {
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;

    #[expect(
        unsafe_code,
        reason = "std has no stable setsid option; pre_exec is the only safe-API gap here"
    )]
    pub(super) fn detach(command: &mut Command) {
        // SAFETY: The hook runs in the forked child before exec and only calls setsid, an
        // async-signal-safe syscall that touches no parent-owned Rust state.
        unsafe {
            command.pre_exec(|| rustix::process::setsid().map(drop).map_err(Into::into));
        }
    }
}
