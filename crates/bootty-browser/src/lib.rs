mod address;
mod annotation;
mod native;
mod profile;
#[cfg(target_os = "linux")]
mod x11_parent;

pub use address::{AddressError, normalize_address, resolve_address};
pub use native::{
    BrowserBounds, BrowserEvent, BrowserShortcut, BrowserView, NativeBrowserError,
    poll_platform_events,
};

pub use profile::BrowserProfile;

pub use annotation::BrowserElement;
