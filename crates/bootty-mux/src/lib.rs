#![recursion_limit = "256"]

pub use bootty_config::config::{
    MultiplexerBackendConfig as MuxBackendKind, MultiplexerConfig as MuxBindingConfig,
    SshRemoteConfig as SshTarget,
};
#[cfg(feature = "app")]
pub use controller::RepaintHandle;
pub use remote_catalog::RemoteSpaceSummary;
pub mod backend;
#[cfg(feature = "app")]
pub mod capability;
pub mod command;
#[cfg(feature = "app")]
pub mod controller;
pub mod herdr;
pub mod membership;
#[cfg(feature = "app")]
pub mod native;
pub mod process;
pub mod project;
pub mod provider;
pub mod remote_catalog;
pub mod remote_space;
#[cfg(feature = "app")]
pub mod repository;
pub mod rmux;
pub mod session_membership;
pub mod snapshot;
#[cfg(feature = "app")]
pub mod terminal;
pub mod tmux;
pub mod tmux_compatible_layout;

#[cfg(feature = "app")]
pub use repository::{
    BackendMembership, BindingMembershipMutation, DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON,
    PendingBindingMembershipMutation, RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride,
    WorkspaceBinding, WorkspaceBindingSelection, WorkspacePersistenceError, WorkspaceRepository,
    WorkspaceResult, WorkspaceSnapshot, WorkspaceSpace,
};
pub use session_membership::{SessionMembership, WorkspaceSession};
