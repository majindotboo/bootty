//! Coordinate installation maintenance with local provider process lifetimes.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use crate::AgentKind;

#[derive(Default)]
struct Activity {
    users: usize,
    updating: bool,
}

fn activity() -> &'static Mutex<HashMap<PathBuf, Activity>> {
    static ACTIVITY: OnceLock<Mutex<HashMap<PathBuf, Activity>>> = OnceLock::new();
    ACTIVITY.get_or_init(Mutex::default)
}

/// A local process reserves its selected executable until it has been reaped.
pub struct ProviderUse(PathBuf);

impl ProviderUse {
    /// # Errors
    /// Returns an in-progress installation update; callers must not launch in that case.
    pub fn acquire(program: &str) -> Result<Option<Self>, String> {
        let Some(path) = bootty_host::find_executable(program) else {
            return Ok(None);
        };
        let mut activity = activity()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = activity.entry(path.clone()).or_default();
        if entry.updating {
            return Err("The provider is updating. Try again when the update finishes.".into());
        }
        entry.users = entry
            .users
            .checked_add(1)
            .ok_or("Too many provider processes")?;
        drop(activity);
        Ok(Some(Self(path)))
    }
}

impl Drop for ProviderUse {
    fn drop(&mut self) {
        let mut activity = activity()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = activity.get_mut(&self.0) {
            entry.users = entry.users.saturating_sub(1);
            if entry.users == 0 && !entry.updating {
                activity.remove(&self.0);
            }
        }
    }
}

/// An exclusive reservation for one recognized installation, acquired before running its updater.
pub struct ProviderUpdate {
    path: PathBuf,
    argv: Vec<String>,
}

impl ProviderUpdate {
    /// Unknown installations remain manual; busy installations can be retried when idle.
    /// # Errors
    /// Returns `provider_busy` while a local provider process or update owns this executable.
    pub fn prepare(provider: AgentKind, program: &str) -> Result<Option<Self>, String> {
        let Some(path) = bootty_host::find_executable(program) else {
            return Ok(None);
        };
        let Some(argv) = crate::terminal_provider_installer(provider, &path) else {
            return Ok(None);
        };
        let mut activity = activity()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = activity.entry(path.clone()).or_default();
        if entry.users != 0 || entry.updating {
            return Err("provider_busy".into());
        }
        entry.updating = true;
        drop(activity);
        Ok(Some(Self { path, argv }))
    }

    /// # Errors
    /// Returns bounded host execution errors or an unsuccessful installer result.
    pub fn run(
        &self,
        deadline: std::time::Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        let (program, args) = self
            .argv
            .split_first()
            .ok_or("The installer has no executable")?;
        let mut command = bootty_host::local_command(program);
        command.args(args);
        let result = crate::terminal_process::query_output_command(
            command,
            deadline.saturating_duration_since(std::time::Instant::now()),
            cancelled,
        )?;
        if result.successful {
            return Ok(());
        }
        // WinGet reports an already-current package as a nonzero result.
        // Check its stable HRESULT instead of depending on localized output.
        if std::path::Path::new(program)
            .file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case("winget"))
            && result.exit_code == Some(-1_978_335_189)
        {
            return Ok(());
        }
        Err(format!(
            "Provider update failed (exit {:?}): {}",
            result.exit_code,
            String::from_utf8_lossy(&result.stderr)
                .trim()
                .chars()
                .take(2048)
                .collect::<String>()
        ))
    }
}

impl Drop for ProviderUpdate {
    fn drop(&mut self) {
        activity()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
    }
}
