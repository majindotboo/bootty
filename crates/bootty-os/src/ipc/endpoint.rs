use std::io;
use std::path::{Path, PathBuf};

/// The address of a local endpoint: a Unix socket path or a Windows named-pipe name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocalEndpoint {
    path: PathBuf,
}

impl LocalEndpoint {
    /// An endpoint at an explicit socket path or pipe name, such as one read from a descriptor.
    #[must_use]
    pub const fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

/// The endpoint this account uses for `label`.
///
/// Unix sockets live in `/tmp/bootty-<uid>/<label>`, which stays inside every supported
/// platform's `sun_path` limit; the caller creates that directory owner-only before binding.
/// Windows pipes are `\\.\pipe\bootty-<sid>-<label>`. Labels should be unguessable when a
/// squatter matters; both ends still verify the peer's account.
///
/// # Errors
/// Returns the account lookup failure.
pub fn endpoint_for_label(label: &str) -> io::Result<LocalEndpoint> {
    let user = crate::identity::UserIdentity::current()?;
    #[cfg(unix)]
    {
        Ok(LocalEndpoint::from_path(
            Path::new("/tmp")
                .join(format!("bootty-{}", user.uid()))
                .join(label),
        ))
    }
    #[cfg(windows)]
    {
        Ok(LocalEndpoint::from_path(PathBuf::from(format!(
            r"\\.\pipe\bootty-{}-{label}",
            user.sid()
        ))))
    }
}
