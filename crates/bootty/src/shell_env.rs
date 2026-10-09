//! Login-shell environment hydration.
//!
//! A macOS `.app` launched from Finder/Dock/Spotlight inherits launchd's minimal
//! environment: PATH is `/usr/bin:/bin:/usr/sbin:/sbin` and the user's shell
//! exports (Homebrew, rustup, custom PATH entries) are absent. That breaks the
//! tmux backend and any tool the user spawns, since both rely on PATH. Launching
//! the same binary from a terminal works only because the terminal hands us its
//! environment. We reproduce that here by running the login shell once and
//! importing what it exports.

use std::{collections::BTreeMap, ffi::OsString, process::Command};

use anyhow::{Context, Result};
use bootty_terminal::terminal_session::{BOOTTY_SHELL_ENV, configured_user_shell};
#[cfg(unix)]
const INITIALIZED: &str = "BOOTTY_STARTUP_ENVIRONMENT_READY";

/// Adopt the login environment before constructing any services. Re-exec uses the standard
/// library's child environment API instead of mutating process-global environment in Rust 2024.
///
/// Returns the child exit status after a restart, or `None` when this process is ready.
///
/// # Errors
/// Returns an error if the executable cannot be located or restarted.
pub fn initialize_shell_environment() -> Result<Option<i32>> {
    #[cfg(unix)]
    if std::env::var(INITIALIZED).ok().as_deref() == Some(std::process::id().to_string().as_str()) {
        return Ok(None);
    }
    let mut updates = BTreeMap::<OsString, OsString>::new();
    if let Some(shell) = advertised_shell(
        std::env::var(BOOTTY_SHELL_ENV).ok(),
        configured_user_shell(),
    ) {
        updates.insert("SHELL".into(), shell.into());
    }
    #[cfg(target_os = "macos")]
    if let Some(vars) = capture_login_env(&login_shell()) {
        for (key, value) in vars {
            let present = std::env::var_os(&key).is_some()
                || updates.contains_key(std::ffi::OsStr::new(&key));
            if should_apply_login_environment(&key, present) {
                updates.insert(key.into(), value.into());
            }
        }
    }
    updates.retain(|key, value| std::env::var_os(key).as_ref() != Some(value));
    if updates.is_empty() {
        return Ok(None);
    }
    let mut command = Command::new(std::env::current_exe().context("locate Bootty executable")?);
    command.args(std::env::args_os().skip(1)).envs(updates);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec preserves this PID; descendants must hydrate their own login environment.
        command.env(INITIALIZED, std::process::id().to_string());
        Err(command.exec()).context("restart Bootty with the login environment")
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .context("restart Bootty with the login environment")?;
        Ok(Some(status.code().unwrap_or(1)))
    }
}

/// The value `$SHELL` should advertise: an explicit override, then the login
/// shell, taking the first that is an absolute path. `None` leaves `$SHELL` as
/// inherited (e.g. non-macOS, where no account shell is resolved).
fn advertised_shell(override_shell: Option<String>, login_shell: Option<String>) -> Option<String> {
    [override_shell, login_shell]
        .into_iter()
        .flatten()
        .find(|shell| std::path::Path::new(shell).is_absolute())
}

#[cfg(target_os = "macos")]
fn login_shell() -> String {
    selected_login_shell(
        advertised_shell(
            std::env::var(BOOTTY_SHELL_ENV).ok(),
            configured_user_shell(),
        ),
        std::env::var("SHELL").ok(),
    )
}

#[cfg(target_os = "macos")]
fn selected_login_shell(configured: Option<String>, inherited: Option<String>) -> String {
    [configured, inherited]
        .into_iter()
        .flatten()
        .find(|shell| std::path::Path::new(shell).is_absolute())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// Load the same login and interactive startup files as a terminal. Tool PATH
/// entries often live inside interactive-only guards. Frame the environment so
/// shell greetings cannot become variable names; NUL preserves multiline values.
#[cfg(target_os = "macos")]
fn capture_login_env(shell: &str) -> Option<Vec<(String, String)>> {
    let output = Command::new(shell)
        .args([
            "-l",
            "-i",
            "-c",
            r"printf '\036BOOTTY_ENV\037'; /usr/bin/env -0",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let output = String::from_utf8_lossy(&output.stdout);
    let (_, environment) = output.split_once("\u{001e}BOOTTY_ENV\u{001f}")?;
    Some(parse_login_environment(environment))
}

#[cfg(target_os = "macos")]
fn parse_login_environment(raw: &str) -> Vec<(String, String)> {
    raw.split('\0')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn should_apply_login_environment(key: &str, current_present: bool) -> bool {
    key == "PATH" || !current_present
}
