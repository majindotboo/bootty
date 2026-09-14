#[cfg(feature = "terminal-runtime")]
mod catalog;
mod space;
pub mod protocol {
    pub use super::space_protocol::*;
}
mod space_protocol;

#[cfg(feature = "terminal-runtime")]
pub use catalog::{
    REMOTE_SPACE_CATALOG_VERSION, create, create_remote_with_runner,
    create_remote_worktree_request_with_runner, create_remote_worktree_with_runner, execute, list,
    list_remote, list_remote_projects_with_runner, list_remote_with_runner,
    list_remote_worktrees_with_runner, snapshot, toggle_remote_project_favorite_with_runner,
};
pub use space::RemoteSpaceBackend;
pub use space_protocol::{decode_command, encode_command};
