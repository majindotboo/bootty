use async_channel::Sender;
use thiserror::Error;
#[cfg(not(target_os = "linux"))]
use wry::dpi::{LogicalPosition, LogicalSize};
#[cfg(target_os = "linux")]
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{
    NewWindowResponse, PageLoadEvent, PermissionResponse, Rect, WebView,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

use crate::{AddressError, BrowserProfile, normalize_address};

mod annotations;
mod credentials;
#[cfg(target_os = "macos")]
mod input;
mod snapshot;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowserBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Physical pixels per logical host coordinate.
    pub scale_factor: f64,
}

impl From<BrowserBounds> for Rect {
    fn from(bounds: BrowserBounds) -> Self {
        #[cfg(target_os = "linux")]
        {
            // X11 screen DPI and GTK's integer scale can differ from GPUI's scale. Explicit
            // device coordinates keep initial creation and subsequent layout on the same grid.
            Self {
                position: PhysicalPosition::new(
                    bounds.x * bounds.scale_factor,
                    bounds.y * bounds.scale_factor,
                )
                .into(),
                size: PhysicalSize::new(
                    bounds.width.max(1.0) * bounds.scale_factor,
                    bounds.height.max(1.0) * bounds.scale_factor,
                )
                .into(),
            }
        }
        #[cfg(not(target_os = "linux"))]
        Self {
            position: LogicalPosition::new(bounds.x, bounds.y).into(),
            size: LogicalSize::new(bounds.width.max(1.0), bounds.height.max(1.0)).into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserEvent {
    Annotation(crate::AnnotationEvent),
    PageFocused,
    PointerEntered,
    Shortcut(BrowserShortcut),
    LoadStarted(String),
    LoadFinished(String),
    TitleChanged(String),
    OpenTab(String),
    Notice(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserShortcut {
    Palette,
    NewSession,
    Address,
    Reload,
    NewTab,
    CloseTab,
}

#[derive(Debug, Error)]
pub enum NativeBrowserError {
    #[error("Native browser input is currently available on macOS only")]
    InputUnavailable,
    #[error("Wry child views do not support Command shortcuts through background native input")]
    CommandShortcutUnavailable,
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(
        "Embedded browsing is unavailable for this window backend. Open links in your default browser."
    )]
    UnsupportedWindow,
    #[cfg(target_os = "linux")]
    #[error(
        "Embedded browsing needs XWayland. Enable XWayland in your compositor and restart Bootty."
    )]
    XwaylandRequired,
    #[error("The browser is unavailable: {0}")]
    Platform(String),
    #[error(transparent)]
    WebView(#[from] wry::Error),
}

/// A main-thread native child view. Its owner drops it before releasing the parent window.
pub struct BrowserView {
    view: WebView,
    #[cfg(target_os = "linux")]
    linux_host: crate::linux_host::LinuxBrowserHost,
    bounds: BrowserBounds,
    visible: bool,
    annotation_captures: annotations::CaptureResponses,
}

impl BrowserView {
    /// Send `AppKit` input directly to this visible `WKWebView`, without activating the app.
    /// # Errors
    /// Rejects invalid input, hidden/closed views and unsupported platforms.
    pub fn input(&self, action: &crate::BrowserInput) -> Result<(), NativeBrowserError> {
        action.validate().map_err(NativeBrowserError::Platform)?;
        if !self.visible {
            return Err(NativeBrowserError::Platform(
                "Select the browser page before sending input".into(),
            ));
        }
        #[cfg(target_os = "macos")]
        return input::dispatch(&self.view, action);
        #[cfg(not(target_os = "macos"))]
        Err(NativeBrowserError::InputUnavailable)
    }
    /// Begin a user-requested element selection; form values are never collected.
    ///
    /// # Errors
    /// Reports native script dispatch failure.
    pub fn pick_annotation(&self, dark: bool) -> Result<(), NativeBrowserError> {
        self.view
            .evaluate_script(&format!("window.__boottyAnnotations?.pick({dark})"))?;
        Ok(())
    }

    /// Open a floating page editor for validated local annotation data.
    ///
    /// # Errors
    /// Reports invalid annotation metadata or script dispatch failure.
    pub fn edit_annotation(
        &self,
        annotation: &crate::Annotation,
        dark: bool,
        conversation_available: bool,
    ) -> Result<(), NativeBrowserError> {
        if self.current_address()? != annotation.address {
            return Err(NativeBrowserError::Platform(
                "This annotation belongs to a different page address.".into(),
            ));
        }
        annotation
            .validate()
            .map_err(|error| NativeBrowserError::Platform(error.to_string()))?;
        let serde_json::Value::Object(mut payload) = serde_json::to_value(annotation)
            .map_err(|error| NativeBrowserError::Platform(error.to_string()))?
        else {
            return Err(NativeBrowserError::Platform(
                "The annotation data is invalid.".into(),
            ));
        };
        _ = payload.insert(
            "id".to_owned(),
            serde_json::Value::String(annotation.id.to_string()),
        );
        let payload = serde_json::Value::Object(payload);
        self.view.evaluate_script(&format!(
            "window.__boottyAnnotations?.edit({payload}, {dark}, {conversation_available})"
        ))?;
        Ok(())
    }

    /// Publish host conversation availability; page state cannot authorize an attachment.
    ///
    /// # Errors
    /// Reports native script dispatch failure.
    pub fn set_annotation_conversation(&self, available: bool) -> Result<(), NativeBrowserError> {
        self.view.evaluate_script(&format!(
            "window.__boottyAnnotations?.conversation({available})"
        ))?;
        Ok(())
    }
    /// Close a saved editor only after its local commit, or display the observed write error.
    ///
    /// # Errors
    /// Reports native script dispatch failure.
    pub fn annotation_edit_result(
        &self,
        id: u64,
        error: Option<&str>,
    ) -> Result<(), NativeBrowserError> {
        let payload = serde_json::json!([
            id.to_string(),
            error.map(|error| error.chars().take(256).collect::<String>())
        ]);
        self.view
            .evaluate_script(&format!("window.__boottyAnnotations?.result(...{payload})"))?;
        Ok(())
    }

    /// Creates a child webview in a live native parent window.
    ///
    /// # Errors
    /// Reports unsupported window handles, platform initialization, and webview creation failures.
    pub fn new(
        window: &impl HasWindowHandle,
        address: &str,
        bounds: BrowserBounds,
        events: Sender<BrowserEvent>,
        profile: &mut BrowserProfile,
        persist_site_data: bool,
    ) -> Result<Self, NativeBrowserError> {
        let address = normalize_address(address)?;
        let handle = window
            .window_handle()
            .map_err(|error| NativeBrowserError::Platform(error.to_string()))?;
        let supported = match handle.as_raw() {
            RawWindowHandle::AppKit(_) => cfg!(target_os = "macos"),
            RawWindowHandle::Win32(_) => cfg!(target_os = "windows"),
            RawWindowHandle::Xlib(_) => cfg!(target_os = "linux"),
            RawWindowHandle::Xcb(_) => cfg!(target_os = "linux"),
            #[cfg(target_os = "linux")]
            RawWindowHandle::Wayland(_) => return Err(NativeBrowserError::XwaylandRequired),
            _ => false,
        };
        if !supported {
            return Err(NativeBrowserError::UnsupportedWindow);
        }
        #[cfg(target_os = "linux")]
        let linux_host = crate::linux_host::LinuxBrowserHost::new(handle.as_raw(), bounds)?;
        let title_events = events.clone();
        let popup_events = events.clone();
        let permission_events = events.clone();
        let download_events = events.clone();
        let shortcut_events = events.clone();
        let annotation_captures = annotations::CaptureResponses::default();
        let capture_responses = annotation_captures.clone();
        let builder = profile
            .builder(persist_site_data)
            .with_url(&address)
            .with_bounds(view_bounds(bounds))
            .with_visible(false)
            .with_focused(false)
            .with_initialization_script(
                r"
                for (const type of ['pointerdown', 'focusin']) {
                    document.addEventListener(type, event => {
                        if (event.isTrusted) window.ipc.postMessage('browser-focus');
                    }, true);
                }
                document.documentElement.addEventListener('pointerenter', event => {
                    if (event.isTrusted) window.ipc.postMessage('browser-pointer-enter');
                });
                document.addEventListener('keydown', event => {
                    if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
                    const key = event.key.toLowerCase();
                    if (!['k', 'n', 'l', 'r', 't', 'w'].includes(key)) return;
                    event.preventDefault();
                    event.stopImmediatePropagation();
                    window.ipc.postMessage('browser-key:' + key);
                }, true);
            ",
            )
            .with_ipc_handler(move |request| {
                if !annotations::receive_capture(request.body(), &capture_responses) {
                    handle_ipc(request.body(), &shortcut_events);
                }
            })
            .with_initialization_script(include_str!("credentials.js"))
            .with_initialization_script(include_str!("annotations.js"))
            .with_navigation_handler(|url| normalize_address(&url).is_ok())
            .with_document_title_changed_handler(move |title| {
                _ = title_events.try_send(BrowserEvent::TitleChanged(
                    title.chars().take(256).collect(),
                ));
            })
            .with_on_page_load_handler(move |event, url| {
                let event = match event {
                    PageLoadEvent::Started => BrowserEvent::LoadStarted(url),
                    PageLoadEvent::Finished => BrowserEvent::LoadFinished(url),
                };
                _ = events.try_send(event);
            })
            .with_new_window_req_handler(move |url, _| {
                _ = popup_events.try_send(BrowserEvent::OpenTab(url));
                NewWindowResponse::Deny
            })
            .with_permission_handler(move |_| {
                _ = permission_events.try_send(BrowserEvent::Notice(
                    "This browser cannot access the camera, microphone, or location.".to_owned(),
                ));
                PermissionResponse::Deny
            })
            .with_download_started_handler(move |_, _| {
                _ = download_events.try_send(BrowserEvent::Notice(
                    "Open this page in your default browser to download files.".to_owned(),
                ));
                false
            });
        #[cfg(target_os = "linux")]
        let view = linux_host.build(builder)?;
        #[cfg(not(target_os = "linux"))]
        let view = builder.build_as_child(window)?;
        Ok(Self {
            view,
            #[cfg(target_os = "linux")]
            linux_host,
            bounds,
            visible: false,
            annotation_captures,
        })
    }

    /// Returns the native page address, including navigation not yet published to the host.
    ///
    /// # Errors
    /// Reports a failed native address lookup.
    pub fn current_address(&self) -> Result<String, NativeBrowserError> {
        Ok(self.view.url()?)
    }

    /// Read the actual native child rectangle, including platform rounding.
    /// # Errors
    /// Reports a failed native geometry lookup.
    #[cfg(target_os = "macos")]
    pub fn capture_bounds(&self) -> Result<BrowserBounds, NativeBrowserError> {
        let rect = self.view.bounds()?;
        let position = rect.position.to_logical::<f64>(self.bounds.scale_factor);
        let size = rect.size.to_logical::<f64>(self.bounds.scale_factor);
        Ok(BrowserBounds {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            scale_factor: self.bounds.scale_factor,
        })
    }

    /// Moves the child view to panel bounds in logical window coordinates.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn set_bounds(&mut self, bounds: BrowserBounds) -> Result<(), NativeBrowserError> {
        if self.bounds != bounds {
            #[cfg(target_os = "linux")]
            self.linux_host.set_bounds(bounds);
            self.view.set_bounds(crate::native::view_bounds(bounds))?;
            self.bounds = bounds;
        }
        Ok(())
    }

    /// Shows or hides the child view, returning focus to its parent when hidden.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn set_visible(&mut self, visible: bool) -> Result<(), NativeBrowserError> {
        if self.visible != visible {
            if !visible {
                self.focus_parent()?;
            }
            self.view.set_visible(visible)?;
            #[cfg(target_os = "linux")]
            self.linux_host.set_visible(visible);
            // GTK can restore a stale child allocation when showing the tab again.
            #[cfg(target_os = "linux")]
            if visible {
                self.linux_host.set_bounds(self.bounds);
            }
            self.visible = visible;
        }
        Ok(())
    }

    /// Starts loading an address in the existing native tab.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn navigate(&self, address: &str) -> Result<(), NativeBrowserError> {
        Ok(self.view.load_url(&normalize_address(address)?)?)
    }

    /// Requests the previous native history entry.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn back(&self) -> Result<(), NativeBrowserError> {
        Ok(self.view.go_back()?)
    }

    /// Requests the next native history entry.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn forward(&self) -> Result<(), NativeBrowserError> {
        Ok(self.view.go_forward()?)
    }

    /// Requests a reload of the current native page.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn reload(&self) -> Result<(), NativeBrowserError> {
        Ok(self.view.reload()?)
    }

    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.view.can_go_back().unwrap_or(false)
    }

    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.view.can_go_forward().unwrap_or(false)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn request_clear_site_data(
        &self,
        completion: Sender<Result<(), String>>,
    ) -> Result<(), NativeBrowserError> {
        use webkit2gtk::{WebContextExt as _, WebViewExt as _, WebsiteDataManagerExtManual as _};
        use wry::WebViewExtUnix as _;
        let manager = self
            .view
            .webview()
            .context()
            .and_then(|context| context.website_data_manager())
            .ok_or_else(|| {
                NativeBrowserError::Platform("The browser data store is unavailable.".into())
            })?;
        manager.clear(
            webkit2gtk::WebsiteDataTypes::ALL,
            gtk::glib::TimeSpan::from_seconds(0),
            None::<&gtk::gio::Cancellable>,
            move |result| {
                _ = completion.try_send(result.map_err(|error| error.to_string()));
            },
        );
        Ok(())
    }

    /// Stops loading the current page.
    ///
    /// # Errors
    /// Reports a failed native script dispatch.
    pub fn stop(&self) -> Result<(), NativeBrowserError> {
        Ok(self.view.evaluate_script("window.stop()")?)
    }

    /// Returns the native responder to the host before GPUI starts editing.
    ///
    /// # Errors
    /// Reports a failed native focus operation.
    pub fn focus_parent(&self) -> Result<(), NativeBrowserError> {
        #[cfg(target_os = "linux")]
        self.linux_host.focus_parent();
        #[cfg(not(target_os = "linux"))]
        self.view.focus_parent()?;
        Ok(())
    }

    /// Moves native keyboard focus to the page.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn focus(&self) -> Result<(), NativeBrowserError> {
        Ok(self.view.focus()?)
    }
}

/// GPUI owns the event loop; service pending `WebKitGTK` callbacks without blocking it.
#[cfg(target_os = "linux")]
pub fn poll_platform_events() {
    if !gtk::is_initialized_main_thread() {
        return;
    }
    for _ in 0..32 {
        if !gtk::events_pending() {
            break;
        }
        gtk::main_iteration_do(false);
    }
}

/// Native desktop event loops already dispatch the webview callbacks on these platforms.
#[cfg(not(target_os = "linux"))]
pub const fn poll_platform_events() {}

fn handle_ipc(message: &str, events: &Sender<BrowserEvent>) {
    if let Some(message) = message.strip_prefix("browser-annotation:") {
        if let Some(event) = crate::AnnotationEvent::parse(message) {
            _ = events.try_send(BrowserEvent::Annotation(event));
        }
        return;
    }
    if message == "browser-focus" {
        _ = events.try_send(BrowserEvent::PageFocused);
    } else if message == "browser-pointer-enter" {
        _ = events.try_send(BrowserEvent::PointerEntered);
    } else {
        let shortcut = match message {
            "browser-key:k" => BrowserShortcut::Palette,
            "browser-key:n" => BrowserShortcut::NewSession,
            "browser-key:l" => BrowserShortcut::Address,
            "browser-key:r" => BrowserShortcut::Reload,
            "browser-key:t" => BrowserShortcut::NewTab,
            "browser-key:w" => BrowserShortcut::CloseTab,
            _ => return,
        };
        _ = events.try_send(BrowserEvent::Shortcut(shortcut));
    }
}

/// GTK positions the embedding host; its child starts at the host origin.
#[cfg(target_os = "linux")]
fn view_bounds(bounds: BrowserBounds) -> Rect {
    BrowserBounds {
        x: 0.0,
        y: 0.0,
        ..bounds
    }
    .into()
}

#[cfg(not(target_os = "linux"))]
fn view_bounds(bounds: BrowserBounds) -> Rect {
    bounds.into()
}
