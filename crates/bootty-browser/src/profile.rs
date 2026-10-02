use sha2::{Digest as _, Sha256};
use std::path::PathBuf;
use wry::{WebContext, WebViewBuilder};

/// One identity's site data. Native views must be dropped before this owner.
pub struct BrowserProfile {
    directory: PathBuf,
    identifier: [u8; 16],
    context: Option<WebContext>,
    persistent: bool,
}

impl BrowserProfile {
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        let digest = Sha256::digest(directory.as_os_str().as_encoded_bytes());
        let (prefix, _) = digest.split_at(16);
        let mut identifier = [0; 16];
        identifier.copy_from_slice(prefix);
        Self {
            directory,
            identifier,
            context: None,
            persistent: supports_named_profiles(),
        }
    }

    /// Platform credential-store key, scoped to this identity and the exact website origin.
    ///
    /// # Errors
    /// Rejects addresses that cannot safely receive saved logins.
    pub fn credential_service(&self, address: &str) -> Result<String, crate::AddressError> {
        use std::fmt::Write as _;
        let origin = crate::login_origin(address)?;
        let mut service = String::from("bootty-browser:");
        for byte in self.identifier {
            _ = write!(service, "{byte:02x}");
        }
        Ok(format!("{service}:{origin}"))
    }

    pub(crate) fn builder(&mut self, persist: bool) -> WebViewBuilder<'_> {
        let private = !persist || !self.persistent;
        let identifier = self.identifier;
        let builder = WebViewBuilder::new_with_web_context(self.context()).with_incognito(private);
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewBuilderExtDarwin as _;
            // Older macOS stays private until named stores can isolate app identities.
            builder.with_data_store_identifier(identifier)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = identifier;
            builder
        }
    }

    fn context(&mut self) -> &mut WebContext {
        self.context
            .get_or_insert_with(|| WebContext::new(Some(self.directory.clone())))
    }
}

#[cfg(target_os = "macos")]
fn supports_named_profiles() -> bool {
    objc2_foundation::NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 14
}

#[cfg(not(target_os = "macos"))]
const fn supports_named_profiles() -> bool {
    true
}
