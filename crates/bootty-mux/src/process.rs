//! Compatibility reexports for the host execution seam.
//!
//! New code should import these values from `bootty_host`; this module remains while downstream
//! provider crates migrate off the old mux path.
pub use bootty_host::{
    CancellableCommandRunner, CommandCancellation, CommandOutput, CommandRunner,
    SystemCommandRunner, require_success,
};
#[cfg(target_os = "macos")]
pub use bootty_host::{
    macos_shell_environment_prelude, macos_shell_environment_prelude_from, resolve_program,
    wait_for_launchd_exit,
};
