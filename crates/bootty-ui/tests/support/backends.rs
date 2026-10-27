use std::sync::Arc;

use bootty_mux::provider::MuxBackendRegistry;

/// The desktop mux backends, shared by every test host.
///
/// # Panics
/// Panics when a desktop backend fails to register.
#[must_use]
pub fn backends() -> Arc<MuxBackendRegistry> {
    Arc::new(MuxBackendRegistry::desktop().expect("complete test backend registry"))
}
