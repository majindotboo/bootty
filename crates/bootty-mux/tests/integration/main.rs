#![cfg(test)]

#[path = "../native_backend_behavior.rs"]
mod native_backend_behavior;

#[path = "../native_server.rs"]
mod native_server;

#[path = "../pane_layout.rs"]
mod pane_layout;

#[path = "../session_snapshots.rs"]
mod session_snapshots;

#[path = "../terminal_behavior.rs"]
mod terminal_behavior;

#[path = "../terminal_cwd.rs"]
mod terminal_cwd;

#[path = "../workspace_repository.rs"]
mod workspace_repository;

#[path = "../workspace_session_membership.rs"]
mod workspace_session_membership;

mod concurrent_pane_launch;
