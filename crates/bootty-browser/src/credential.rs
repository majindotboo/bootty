use std::fmt;

use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WebOrigin(String);

impl WebOrigin {
    /// Parse the exact HTTP(S) origin, excluding path, query and fragment.
    ///
    /// # Errors
    /// Rejects non-web URLs, embedded credentials and oversized addresses.
    pub fn parse(address: &str) -> Result<Self, CredentialError> {
        if address.len() > 8192 {
            return Err(CredentialError::InvalidOrigin);
        }
        let url = Url::parse(address).map_err(|_| CredentialError::InvalidOrigin)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(CredentialError::InvalidOrigin);
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CredentialAccount(String);

impl CredentialAccount {
    /// Preserve the exact account chosen by the user.
    ///
    /// # Errors
    /// Rejects empty, oversized and control-character account names.
    pub fn new(account: String) -> Result<Self, CredentialError> {
        if account.is_empty() || account.len() > 512 || account.chars().any(char::is_control) {
            return Err(CredentialError::InvalidAccount);
        }
        Ok(Self(account))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Deliberately has no Clone, Display or serialization traits.
pub struct SecretPassword(String);

impl SecretPassword {
    /// Accept an explicit password without retaining a second copy.
    ///
    /// # Errors
    /// Rejects empty or oversized passwords without returning their contents.
    pub fn new(password: String) -> Result<Self, CredentialError> {
        if password.is_empty() || password.len() > 4096 {
            return Err(CredentialError::InvalidPassword);
        }
        Ok(Self(password))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretPassword([redacted])")
    }
}

/// The page incarnation whose origin and account are shown in the native confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialTarget {
    page_id: u64,
    load_revision: u64,
    origin: WebOrigin,
}

impl CredentialTarget {
    /// Capture the exact native page before opening the credential dialog.
    ///
    /// # Errors
    /// Rejects zero page IDs and addresses without a valid web origin.
    pub fn new(page_id: u64, load_revision: u64, address: &str) -> Result<Self, CredentialError> {
        if page_id == 0 {
            return Err(CredentialError::StalePage);
        }
        Ok(Self {
            page_id,
            load_revision,
            origin: WebOrigin::parse(address)?,
        })
    }

    #[must_use]
    pub const fn page_id(&self) -> u64 {
        self.page_id
    }

    #[must_use]
    pub const fn load_revision(&self) -> u64 {
        self.load_revision
    }

    #[must_use]
    pub const fn origin(&self) -> &WebOrigin {
        &self.origin
    }

    /// Recheck before store access and again before filling after an asynchronous read.
    ///
    /// # Errors
    /// Refuses another origin, closed/replaced pages and any intervening navigation.
    pub fn validate_current(&self, current: &Self) -> Result<(), CredentialError> {
        if self.origin != current.origin {
            return Err(CredentialError::WrongOrigin);
        }
        if self.page_id != current.page_id || self.load_revision != current.load_revision {
            return Err(CredentialError::StalePage);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialDecision {
    Confirm,
    Cancel,
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("Use a valid HTTP or HTTPS origin without embedded credentials.")]
    InvalidOrigin,
    #[error("Enter an account of 1–512 bytes without control characters.")]
    InvalidAccount,
    #[error("Enter a password of 1–4096 bytes.")]
    InvalidPassword,
    #[error("The browser origin changed. Open the credential dialog again.")]
    WrongOrigin,
    #[error("The browser page changed. Open the credential dialog again.")]
    StalePage,
    #[error("Credential action cancelled.")]
    Cancelled,
    #[error("Choose the account to fill.")]
    AccountRequired,
    #[error("No saved credential exists for this origin and account.")]
    NotFound,
    #[error("The credential store is locked or requires existing access.")]
    Locked,
    #[error("The credential store is unavailable.")]
    Unavailable,
    #[error("Credential storage is unsupported on this platform.")]
    Unsupported,
    #[error("This page has no unambiguous sign-in form to fill.")]
    FillUnavailable,
    #[error("This origin has too many saved accounts to show safely.")]
    TooManyAccounts,
    #[error("The credential store could not complete the action.")]
    StoreFailed,
}
