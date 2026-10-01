use bootty_browser::{AddressError, BrowserBounds, BrowserView, NativeBrowserError};
use rstest::{fixture, rstest};
use wry::raw_window_handle::{HandleError, HasWindowHandle, WindowHandle};

struct UnavailableWindow;

impl HasWindowHandle for UnavailableWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

#[fixture]
fn unavailable_window() -> UnavailableWindow {
    UnavailableWindow
}

#[rstest]
fn unavailable_parent_is_a_reported_error(unavailable_window: UnavailableWindow) {
    let (events, _) = async_channel::unbounded();
    let result = BrowserView::new(
        &unavailable_window,
        "https://example.com/",
        BrowserBounds {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        events,
    );
    assert!(matches!(result, Err(NativeBrowserError::Platform(_))));
}

#[rstest]
#[case("javascript:alert(1)")]
#[case("file:///etc/passwd")]
fn native_boundary_rejects_unsupported_addresses(
    unavailable_window: UnavailableWindow,
    #[case] address: &str,
) {
    let (events, _) = async_channel::unbounded();
    let result = BrowserView::new(
        &unavailable_window,
        address,
        BrowserBounds {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        events,
    );
    assert!(matches!(
        result,
        Err(NativeBrowserError::Address(AddressError::UnsupportedScheme))
    ));
}
