mod address;
mod native;

pub use address::{AddressError, normalize_address};
pub use native::{
    BrowserBounds, BrowserEvent, BrowserShortcut, BrowserView, NativeBrowserError,
    poll_platform_events,
};
