// Embedded rmux server futures exceed the default auto-trait recursion depth.
#![recursion_limit = "256"]

pub use bootty_config::config::{
    MultiplexerBackendConfig as MuxBackendKind, MultiplexerConfig as MuxBindingConfig,
    RemoteConfig as RemoteTarget, SshRemoteConfig as SshTarget,
};
#[cfg(feature = "terminal-runtime")]
pub use controller::RepaintHandle;
pub use remote_catalog::RemoteSpaceSummary;
pub mod backend;
#[cfg(feature = "terminal-runtime")]
pub mod capability;
pub mod command;
#[cfg(feature = "terminal-runtime")]
pub mod controller;
#[cfg(feature = "terminal-runtime")]
pub mod executor;
pub mod membership;
pub mod process;
pub mod provider;
pub mod remote_catalog;
pub mod remote_space;
#[cfg(feature = "terminal-runtime")]
pub mod repository;
pub mod session_membership;
pub mod snapshot;
#[cfg(feature = "terminal-runtime")]
pub mod terminal;
pub mod tmux_compatible_layout;
pub mod workflow;

#[cfg(feature = "terminal-runtime")]
pub mod pane_layout;

#[cfg(feature = "terminal-runtime")]
pub mod terminal_config;

#[cfg(feature = "terminal-runtime")]
pub mod native;

pub mod tmux;

pub mod herdr;

pub mod rmux;

pub mod session_names;

#[cfg(feature = "terminal-runtime")]
pub mod target;

#[cfg(feature = "terminal-runtime")]
pub mod workspace;
