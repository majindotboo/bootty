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

/// Keep the temporary native view alive until the observed platform completion.
pub struct SiteDataReset {
    pub view: Option<crate::BrowserView>,
    pub completion: async_channel::Receiver<Result<(), String>>,
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

    #[must_use]
    pub const fn persists_site_data(&self, requested: bool) -> bool {
        requested && self.persistent
    }

    #[must_use]
    pub const fn supports_site_data_reset() -> bool {
        // Windows Wry does not report WebView2's asynchronous clearing result.
        cfg!(any(target_os = "macos", target_os = "linux"))
    }

    /// Drops private contexts and requests removal of this identity's saved site data.
    /// Callers must release every page view before calling this method.
    ///
    /// # Errors
    /// Reports platform dispatch failures; completion arrives through the receiver.
    pub fn request_reset(
        &mut self,
        window: &impl wry::raw_window_handle::HasWindowHandle,
    ) -> Result<SiteDataReset, crate::NativeBrowserError> {
        self.context = None;
        let (sender, receiver) = async_channel::bounded::<Result<(), String>>(1);
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewExtDarwin as _;
            let _ = window;
            if self.persistent {
                wry::WebView::remove_data_store(&self.identifier, move |result| {
                    _ = sender.try_send(result.map_err(|error| error.to_string()));
                });
            } else {
                _ = sender.try_send(Ok(()));
            }
            Ok(SiteDataReset {
                view: None,
                completion: receiver,
            })
        }
        #[cfg(target_os = "linux")]
        {
            // The hidden view selects the same persistent data store even when every page was closed.
            let (events, _) = async_channel::bounded(1);
            let view = crate::BrowserView::new(
                window,
                "about:blank",
                crate::BrowserBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                    scale_factor: 1.0,
                },
                events,
                self,
                true,
            )?;
            view.request_clear_site_data(sender)?;
            Ok(SiteDataReset {
                view: Some(view),
                completion: receiver,
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (window, sender, receiver);
            // Wry's Windows clearing API does not expose WebView2's completion result.
            // Enable reset once a safe public API reports the observed platform completion.
            Err(crate::NativeBrowserError::Platform(
                "Confirmed site-data reset is unavailable on this platform.".into(),
            ))
        }
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
