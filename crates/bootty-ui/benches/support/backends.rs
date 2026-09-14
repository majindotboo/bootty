use std::sync::Arc;

use anyhow::Result;
use bootty_mux::provider::MuxBackendRegistry;

pub fn backends() -> Result<Arc<MuxBackendRegistry>> {
    Ok(Arc::new(MuxBackendRegistry::desktop()?))
}
