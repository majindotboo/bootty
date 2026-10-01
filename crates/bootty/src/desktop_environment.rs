use std::{os::unix::process::CommandExt as _, process::Command};

use anyhow::{Context as _, Result};

/// Keep GPUI and GTK on the same X server, including `XWayland` in a Wayland session.
/// Re-exec changes only the new process environment, before either toolkit is initialized.
///
/// # Errors
/// Returns an error if the executable cannot be located or restarted.
pub fn initialize_browser_environment() -> Result<()> {
    if std::env::var_os("DISPLAY").is_none_or(|display| display.is_empty()) {
        return Ok(());
    }
    let has_wayland =
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty());
    if !has_wayland && std::env::var("GDK_BACKEND").ok().as_deref() == Some("x11") {
        return Ok(());
    }
    let error = Command::new(std::env::current_exe().context("locate Bootty executable")?)
        .args(std::env::args_os().skip(1))
        .env_remove("WAYLAND_DISPLAY")
        .env("GDK_BACKEND", "x11")
        .exec();
    Err(error).context("restart Bootty on the shared X11 browser backend")
}
