use wry::raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
};

/// Borrows the same X11 window through the handle flavor accepted by Wry.
pub(crate) struct X11Parent<'a>(pub(crate) WindowHandle<'a>);

impl HasWindowHandle for X11Parent<'_> {
    #[expect(
        unsafe_code,
        reason = "Xcb and Xlib identify the same live X11 window; the original borrow bounds the converted handle."
    )]
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let RawWindowHandle::Xcb(parent) = self.0.as_raw() else {
            return Ok(self.0);
        };
        let mut xlib = XlibWindowHandle::new(parent.window.get().into());
        xlib.visual_id = parent.visual_id.map_or(0, |visual| visual.get().into());
        // SAFETY: This is the parent's existing XID, not a new resource or pointer. Xlib and
        // Xcb share XIDs on the same DISPLAY. Keeping the original WindowHandle retains its
        // lifetime; the child view is destroyed by the host before that parent is released.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(xlib)) })
    }
}
