use std::path::Path;

use crate::{CredentialAccount, CredentialError, CredentialStore, SecretPassword, WebOrigin};

/// Local credentials are isolated by the owning browser profile directory.
/// Linux and Windows remain unsupported until an installed safe platform API exists.
#[derive(Debug)]
pub struct PlatformCredentialStore {
    #[cfg(target_os = "macos")]
    namespace: String,
}

impl PlatformCredentialStore {
    #[must_use]
    #[cfg(target_os = "macos")]
    pub fn new(profile_directory: &Path) -> Self {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;
        let digest = Sha256::digest(profile_directory.as_os_str().as_encoded_bytes());
        let mut namespace = String::from("dev.bootty.browser.credentials.");
        for byte in digest {
            let _ = write!(namespace, "{byte:02x}");
        }
        Self { namespace }
    }

    #[must_use]
    #[cfg(not(target_os = "macos"))]
    pub const fn new(_profile_directory: &Path) -> Self {
        Self {}
    }
}

#[cfg(not(target_os = "macos"))]
impl CredentialStore for PlatformCredentialStore {
    fn accounts(&self, _origin: &WebOrigin) -> Result<Vec<CredentialAccount>, CredentialError> {
        Err(CredentialError::Unsupported)
    }

    fn save(
        &self,
        _origin: &WebOrigin,
        _account: &CredentialAccount,
        _password: &SecretPassword,
    ) -> Result<(), CredentialError> {
        Err(CredentialError::Unsupported)
    }

    fn read(
        &self,
        _origin: &WebOrigin,
        _account: &CredentialAccount,
    ) -> Result<SecretPassword, CredentialError> {
        Err(CredentialError::Unsupported)
    }

    fn delete(
        &self,
        _origin: &WebOrigin,
        _account: &CredentialAccount,
    ) -> Result<(), CredentialError> {
        Err(CredentialError::Unsupported)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Mutex;

    use security_framework::{
        base::Error,
        item::{ItemClass, ItemSearchOptions},
        os::macos::keychain::SecKeychain,
        passwords::{
            PasswordOptions, delete_generic_password_options, generic_password,
            set_generic_password_options,
        },
    };

    use super::{
        CredentialAccount, CredentialError, CredentialStore, PlatformCredentialStore,
        SecretPassword, WebOrigin,
    };

    // The safe SDK suppression guard changes process-wide state. Serialize this owner's access.
    static KEYCHAIN_ACCESS: Mutex<()> = Mutex::new(());
    const ACCOUNT_LIMIT: usize = 64;

    impl PlatformCredentialStore {
        fn service(&self, origin: &WebOrigin) -> String {
            format!("{}:{}", self.namespace, origin.as_str())
        }

        fn options(&self, origin: &WebOrigin, account: &CredentialAccount) -> PasswordOptions {
            let mut options =
                PasswordOptions::new_generic_password(&self.service(origin), account.as_str());
            options.set_access_synchronized(Some(false));
            options
        }
    }

    impl CredentialStore for PlatformCredentialStore {
        fn accounts(&self, origin: &WebOrigin) -> Result<Vec<CredentialAccount>, CredentialError> {
            without_prompts(|| {
                let mut query = ItemSearchOptions::new();
                query
                    .class(ItemClass::generic_password())
                    .service(&self.service(origin))
                    .cloud_sync(Some(false))
                    .load_attributes(true)
                    .load_data(false)
                    .load_refs(false)
                    .limit(65_i64);
                let results = match query.search() {
                    Ok(results) => results,
                    Err(error) if error.code() == -25300 => return Ok(Vec::new()),
                    Err(error) => return Err(store_error(error)),
                };
                if results.len() > ACCOUNT_LIMIT {
                    return Err(CredentialError::TooManyAccounts);
                }
                results
                    .into_iter()
                    .map(|result| {
                        let mut attributes =
                            result.simplify_dict().ok_or(CredentialError::StoreFailed)?;
                        let account = attributes
                            .remove("acct")
                            .ok_or(CredentialError::StoreFailed)?;
                        CredentialAccount::new(account).map_err(|_| CredentialError::StoreFailed)
                    })
                    .collect()
            })
        }

        fn save(
            &self,
            origin: &WebOrigin,
            account: &CredentialAccount,
            password: &SecretPassword,
        ) -> Result<(), CredentialError> {
            without_prompts(|| {
                let mut options = self.options(origin, account);
                options.set_label(&format!("Bootty Browser — {}", origin.as_str()));
                set_generic_password_options(password.expose().as_bytes(), options)
                    .map_err(store_error)
            })
        }

        fn read(
            &self,
            origin: &WebOrigin,
            account: &CredentialAccount,
        ) -> Result<SecretPassword, CredentialError> {
            without_prompts(|| {
                let bytes = generic_password(self.options(origin, account)).map_err(store_error)?;
                let password =
                    String::from_utf8(bytes).map_err(|_| CredentialError::StoreFailed)?;
                SecretPassword::new(password).map_err(|_| CredentialError::StoreFailed)
            })
        }

        fn delete(
            &self,
            origin: &WebOrigin,
            account: &CredentialAccount,
        ) -> Result<(), CredentialError> {
            without_prompts(|| {
                delete_generic_password_options(self.options(origin, account)).map_err(store_error)
            })
        }
    }

    fn without_prompts<T>(
        action: impl FnOnce() -> Result<T, CredentialError>,
    ) -> Result<T, CredentialError> {
        let lock = KEYCHAIN_ACCESS
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        // Preserve an existing disabled interaction state; never enable access or unlock the store.
        let suppression = if SecKeychain::user_interaction_allowed().map_err(store_error)? {
            Some(SecKeychain::disable_user_interaction().map_err(store_error)?)
        } else {
            None
        };
        let result = action();
        drop(suppression);
        drop(lock);
        result
    }

    // Keep OS status and native error descriptions out of command/UI logs.
    const fn store_error(error: Error) -> CredentialError {
        match error.code() {
            -25300 => CredentialError::NotFound,
            -25308 | -25293 => CredentialError::Locked,
            -128 => CredentialError::Cancelled,
            -25291 => CredentialError::Unavailable,
            _ => CredentialError::StoreFailed,
        }
    }
}
