#![cfg(test)]

#[path = "../support/mod.rs"]
mod support;

#[path = "../facts_behavior.rs"]
mod facts_behavior;

#[path = "../git_worktree_behavior.rs"]
mod git_worktree_behavior;

#[path = "../project_discovery_behavior.rs"]
mod project_discovery_behavior;

#[path = "../pull_request_status.rs"]
mod pull_request_status;

#[path = "../workflows.rs"]
mod workflows;

#[path = "../worktree_cleanup.rs"]
mod worktree_cleanup;
