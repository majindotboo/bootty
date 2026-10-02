use async_channel::Sender;
use std::{cell::Cell, rc::Rc};
use thiserror::Error;
#[cfg(not(target_os = "linux"))]
use wry::dpi::{LogicalPosition, LogicalSize};
#[cfg(target_os = "linux")]
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{
    NewWindowResponse, PageLoadEvent, PermissionResponse, Rect, WebView,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

use crate::{AddressError, BrowserElement, BrowserProfile, normalize_address};

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

#[derive(Clone, Debug, PartialEq)]
pub enum BrowserEvent {
    PageFocused,
    ElementPicked(BrowserElement),
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
    Address,
    Reload,
    NewTab,
    CloseTab,
}

#[derive(Debug, Error)]
pub enum NativeBrowserError {
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
    annotation_enabled: Rc<Cell<bool>>,
    bounds: BrowserBounds,
    visible: bool,
}

impl BrowserView {
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
        gtk::init().map_err(|error| NativeBrowserError::Platform(error.to_string()))?;
        #[cfg(target_os = "linux")]
        let window = &crate::x11_parent::X11Parent(handle);
        let title_events = events.clone();
        let popup_events = events.clone();
        let permission_events = events.clone();
        let download_events = events.clone();
        let shortcut_events = events.clone();
        let annotation_enabled = Rc::new(Cell::new(false));
        let ipc_annotation_enabled = annotation_enabled.clone();
        let load_annotation_enabled = annotation_enabled.clone();
        let view = profile
            .builder()
            .with_url(&address)
            .with_bounds(bounds.into())
            .with_visible(false)
            .with_focused(false)
            .with_initialization_script(
                r"
                for (const type of ['pointerdown', 'focusin']) {
                    document.addEventListener(type, event => {
                        if (event.isTrusted) window.ipc.postMessage('browser-focus');
                    }, true);
                }
                document.addEventListener('keydown', event => {
                    if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
                    const key = event.key.toLowerCase();
                    if (!['k', 'l', 'r', 't', 'w'].includes(key)) return;
                    event.preventDefault();
                    event.stopImmediatePropagation();
                    window.ipc.postMessage('browser-key:' + key);
                }, true);
            ",
            )
            .with_initialization_script(include_str!("annotation.js"))
            .with_ipc_handler(move |request| {
                handle_ipc(request.body(), &ipc_annotation_enabled, &shortcut_events);
            })
            // The profile is scoped to the app identity; private pages never retain site data.
            .with_incognito(!persist_site_data)
            .with_navigation_handler(|url| normalize_address(&url).is_ok())
            .with_document_title_changed_handler(move |title| {
                _ = title_events.try_send(BrowserEvent::TitleChanged(
                    title.chars().take(256).collect(),
                ));
            })
            .with_on_page_load_handler(move |event, url| {
                let event = match event {
                    PageLoadEvent::Started => {
                        load_annotation_enabled.set(false);
                        BrowserEvent::LoadStarted(url)
                    }
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
            })
            .build_as_child(window)?;
        Ok(Self {
            view,
            annotation_enabled,
            bounds,
            visible: false,
        })
    }

    /// Enables one element selection in the main page without activating links.
    ///
    /// # Errors
    /// Reports a failed native script dispatch.
    pub fn set_annotation_mode(&self, enabled: bool) -> Result<(), NativeBrowserError> {
        self.view
            .evaluate_script(&format!("window.__boottyAnnotate?.({enabled});"))?;
        self.annotation_enabled.set(enabled);
        Ok(())
    }

    /// Returns the native page address, including navigation not yet published to the host.
    ///
    /// # Errors
    /// Reports a failed native address lookup.
    pub fn current_address(&self) -> Result<String, NativeBrowserError> {
        Ok(self.view.url()?)
    }

    /// Moves the child view to panel bounds in logical window coordinates.
    ///
    /// # Errors
    /// Reports failure from the native webview operation.
    pub fn set_bounds(&mut self, bounds: BrowserBounds) -> Result<(), NativeBrowserError> {
        if self.bounds != bounds {
            self.view.set_bounds(bounds.into())?;
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
                self.view.focus_parent()?;
            }
            self.view.set_visible(visible)?;
            // GTK can restore a stale child allocation when showing the tab again.
            #[cfg(target_os = "linux")]
            if visible {
                self.view.set_bounds(self.bounds.into())?;
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

fn handle_ipc(message: &str, annotation_enabled: &Cell<bool>, events: &Sender<BrowserEvent>) {
    if message == "browser-focus" {
        _ = events.try_send(BrowserEvent::PageFocused);
    } else if let Some(payload) = message.strip_prefix("browser-element:") {
        if annotation_enabled.get()
            && let Some(element) = BrowserElement::parse(payload)
        {
            annotation_enabled.set(false);
            _ = events.try_send(BrowserEvent::ElementPicked(element));
        }
    } else {
        let shortcut = match message {
            "browser-key:k" => BrowserShortcut::Palette,
            "browser-key:l" => BrowserShortcut::Address,
            "browser-key:r" => BrowserShortcut::Reload,
            "browser-key:t" => BrowserShortcut::NewTab,
            "browser-key:w" => BrowserShortcut::CloseTab,
            _ => return,
        };
        _ = events.try_send(BrowserEvent::Shortcut(shortcut));
    }
}
