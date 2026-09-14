use std::path::PathBuf;

use bootty_config::ApplicationIdentity;

pub fn endpoint_path() -> anyhow::Result<PathBuf> {
    endpoint_path_for(ApplicationIdentity::for_process())
}
/// # Errors
/// Returns an error if the identity has no usable local endpoint directory.
pub fn endpoint_path_for(identity: ApplicationIdentity) -> anyhow::Result<PathBuf> {
    Ok(
        rmux_ipc::endpoint_for_label(socket_name(identity, rmux_proto::RMUX_WIRE_VERSION))?
            .into_path(),
    )
}

/// Two builds can share one endpoint exactly when their wire versions match.
#[must_use]
pub fn socket_name(identity: ApplicationIdentity, wire_version: u32) -> String {
    match identity {
        ApplicationIdentity::Production => format!("bootty-wire{wire_version}"),
        ApplicationIdentity::Development => {
            format!("{}-wire{wire_version}", identity.namespace())
        }
    }
}
