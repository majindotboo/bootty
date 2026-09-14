use anyhow::{Result, bail};

/// Bundled daemons and platform resources must be updated together with the application.
/// Enable in-place updates when a complete installation can be replaced and rolled back safely.
///
/// # Errors
/// Always returns the complete-package installation instructions.
pub fn update() -> Result<()> {
    bail!(
        "in-place updates are unavailable; install the complete Bootty release package from https://github.com/majindotboo/bootty/releases/latest so the application, resources, and bundled daemons stay at the same version"
    )
}
