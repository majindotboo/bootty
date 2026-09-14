pub use bootty_host::ssh;
pub use bootty_host::{
    REMOTE_DAEMON_PROGRAM, REMOTE_DAEMON_PROTOCOL_VERSION, run_remote_command, shell_quote,
};
pub mod space {
    pub use bootty_mux::remote_space::*;
}
pub mod space_protocol {
    pub use bootty_mux::remote_space::{decode_command, encode_command};
}
