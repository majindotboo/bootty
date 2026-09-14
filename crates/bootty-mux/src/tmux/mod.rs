mod backend;
#[cfg(feature = "terminal-runtime")]
mod control;
pub mod protocol;
mod provider;

#[cfg(unix)]
pub use backend::clear_dead_socket;
pub use backend::{DefaultTmuxRunner, TmuxBackend, tmux_server_exited};
#[cfg(feature = "terminal-runtime")]
pub use backend::{TmuxPanePolicy, local_server_args, tmux_capabilities};
#[cfg(feature = "terminal-runtime")]
pub use control::TmuxControlRunner;
pub use protocol::{
    TmuxClientSessionChangedNotification, TmuxControlNotification, TmuxControlParser,
    TmuxIdNameNotification, TmuxLayoutChangeNotification, TmuxOutputNotification, TmuxParseError,
    TmuxSessionChangedNotification, TmuxWindowPaneChangedNotification,
};
pub use provider::TmuxProvider;
