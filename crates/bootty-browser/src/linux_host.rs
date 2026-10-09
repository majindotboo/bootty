use crate::{BrowserBounds, NativeBrowserError};
use gtk::prelude::*;
use num_traits::ToPrimitive as _;
use wry::{WebView, WebViewBuilder, WebViewBuilderExtUnix, raw_window_handle::RawWindowHandle};

/// GTK owns the X11 child; Wry owns its page. Neither changes the parent's handle.
pub struct LinuxBrowserHost {
    plug: gtk::Plug,
    fixed: gtk::Fixed,
}

impl LinuxBrowserHost {
    pub(crate) fn new(
        parent: RawWindowHandle,
        bounds: BrowserBounds,
    ) -> Result<Self, NativeBrowserError> {
        let xid = match parent {
            RawWindowHandle::Xcb(parent) => parent.window.get().into(),
            RawWindowHandle::Xlib(parent) => parent.window,
            _ => return Err(NativeBrowserError::UnsupportedWindow),
        };
        // gtk-rs currently exposes signed XIDs. Keep this check until its binding uses X11's range.
        let xid =
            gtk::xlib::Window::try_from(xid).map_err(|_| NativeBrowserError::UnsupportedWindow)?;
        if !gtk::is_initialized() {
            gtk::gdk::set_allowed_backends("x11");
        }
        gtk::init().map_err(|error| NativeBrowserError::Platform(error.to_string()))?;
        if gtk::gdk::Display::default()
            .is_none_or(|display| display.type_().name() != "GdkX11Display")
        {
            return Err(NativeBrowserError::XwaylandRequired);
        }
        let plug = gtk::Plug::new(xid);
        let fixed = gtk::Fixed::new();
        plug.add(&fixed);
        plug.realize();
        let host = Self { plug, fixed };
        host.set_bounds(bounds);
        Ok(host)
    }

    pub(crate) fn build(&self, builder: WebViewBuilder<'_>) -> Result<WebView, NativeBrowserError> {
        Ok(builder.build_gtk(&self.fixed)?)
    }

    pub(crate) fn set_bounds(&self, bounds: BrowserBounds) {
        // GDK child geometry is physical; GTK allocations use its integer device scale.
        let (Some(x), Some(y), Some(width), Some(height)) = (
            (bounds.x * bounds.scale_factor).round().to_i32(),
            (bounds.y * bounds.scale_factor).round().to_i32(),
            (bounds.width.max(1.0) * bounds.scale_factor)
                .round()
                .to_i32(),
            (bounds.height.max(1.0) * bounds.scale_factor)
                .round()
                .to_i32(),
        ) else {
            // Retain the last geometry when the host cannot represent the allocation.
            return;
        };
        if let Some(window) = self.plug.window() {
            window.move_resize(x, y, width.max(1), height.max(1));
        }
    }

    pub(crate) fn focus_parent(&self) {
        if let Some(parent) = self.plug.window().and_then(|window| window.parent()) {
            parent.focus(0);
        }
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        if visible {
            self.plug.show_all();
        } else {
            self.plug.hide();
        }
    }
}

impl Drop for LinuxBrowserHost {
    fn drop(&mut self) {
        self.plug.hide();
        self.plug.unrealize();
    }
}
