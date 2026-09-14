pub mod clipboard_image;
mod exec;
pub mod file_watch;
pub mod files;
mod install;
pub mod jobs;
mod process;
mod shell;
pub mod ssh;
pub mod ssh_forward;
pub mod text_file;

pub use exec::{REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION, run_remote_command};
pub use process::{
    CancellableCommandRunner, CommandBytes, CommandCancellation, CommandOutput, CommandRunner,
    SystemCommandRunner, require_success,
};
#[cfg(target_os = "macos")]
pub use process::{
    macos_shell_environment_prelude, macos_shell_environment_prelude_from, resolve_program,
    wait_for_launchd_exit,
};
pub use shell::shell_quote;

pub mod remote;
pub mod wsl;

pub mod shell_history;
