//! The account a local process runs as.

use std::io;

/// A local account: the real user ID on Unix, the user SID string on Windows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UserIdentity {
    #[cfg(unix)]
    uid: u32,
    #[cfg(windows)]
    sid: String,
}

impl UserIdentity {
    /// The account that owns this process.
    ///
    /// # Errors
    /// Returns the Windows token query failure. Unix lookup cannot fail.
    pub fn current() -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::from_uid(rustix::process::getuid().as_raw()))
        }
        #[cfg(windows)]
        {
            crate::windows::current_user_sid().map(Self::from_sid)
        }
    }

    #[cfg(unix)]
    #[must_use]
    pub(crate) const fn from_uid(uid: u32) -> Self {
        Self { uid }
    }

    #[cfg(unix)]
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    #[cfg(windows)]
    #[must_use]
    pub(crate) const fn from_sid(sid: String) -> Self {
        Self { sid }
    }

    #[cfg(windows)]
    #[must_use]
    pub fn sid(&self) -> &str {
        &self.sid
    }
}
