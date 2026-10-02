mod backend;
mod bridge;
mod local;
#[cfg(feature = "terminal-runtime")]
mod pane;
mod pane_io;
mod provider;
mod remote;

#[cfg(feature = "terminal-runtime")]
pub use backend::rmux_capabilities;
pub use backend::{
    RmuxBackend, RmuxControl, numeric_session_id, session_tag_option, tag_option_id,
};
pub use bridge::{prepare_local_rmux_daemon, run_embedded_rmux_daemon};
pub use local::{endpoint_path_for, socket_name};
#[cfg(feature = "terminal-runtime")]
pub use pane::RmuxPanePolicy;
pub use provider::RmuxProvider;
pub use remote::{RemoteRmuxRequest, run_remote_rmux_command};
pub use rmux_client::INTERNAL_DAEMON_FLAG;
