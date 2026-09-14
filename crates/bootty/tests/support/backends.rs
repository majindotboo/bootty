use std::sync::Arc;

use bootty_mux::provider::MuxBackendRegistry;

pub fn backends() -> Arc<MuxBackendRegistry> {
    Arc::new(MuxBackendRegistry::desktop().expect("complete executable backend registry"))
}
