//! Stable server identity is separate from a process generation or a network route.
use anyhow::{Context as _, Result, ensure};
use bootty_write::{NewFileMode, WriteTarget};
use std::path::Path;

/// Atomically publish one complete identity; an invalid existing identity is never replaced.
/// # Errors
/// Returns storage failures or a corrupt existing identity.
pub fn load_server_id(config_path: &Path) -> Result<String> {
    let path = config_path
        .parent()
        .context("mux state directory missing")?
        .join("mux-server-id");
    std::fs::create_dir_all(path.parent().context("mux identity directory missing")?)?;
    let lock = WriteTarget::resolve(&path)
        .map_err(bootty_write::ResolveTargetError::into_io)?
        .lock()?;
    let result = match std::fs::read_to_string(&path) {
        Ok(id) => {
            ensure!(
                id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid persisted mux server identity"
            );
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = format!(
                "{:032x}",
                u128::from(getrandom::u64()?).wrapping_shl(64) | u128::from(getrandom::u64()?)
            );
            lock.create(id.as_bytes(), NewFileMode::Private)
                .map_err(bootty_write::CommitError::into_io)?;
            Ok(id)
        }
        Err(error) => Err(error.into()),
    };
    drop(lock);
    result
}
