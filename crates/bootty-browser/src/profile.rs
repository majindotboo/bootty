use sha2::{Digest as _, Sha256};
use std::path::PathBuf;
use wry::{WebContext, WebViewBuilder};

/// One identity's site data. Native views must be dropped before this owner.
pub struct BrowserProfile {
    directory: PathBuf,
    identifier: [u8; 16],
    context: Option<WebContext>,
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
        }
    }

    pub(crate) fn builder(&mut self) -> WebViewBuilder<'_> {
        let identifier = self.identifier;
        let builder = WebViewBuilder::new_with_web_context(self.context());
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewBuilderExtDarwin as _;
            // macOS 14+ has a named store; macOS 13 uses the app bundle's identity.
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
