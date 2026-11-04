#![allow(
    clippy::disallowed_methods,
    reason = "Developer tooling tests do not share product PTY descriptors"
)]
#![cfg(test)]

#[path = "../benchmark_migration.rs"]
mod benchmark_migration;

#[path = "../build_policy.rs"]
mod build_policy;

#[path = "../daemon_policy.rs"]
mod daemon_policy;

#[path = "../hostile.rs"]
mod hostile;

#[path = "../live_remote.rs"]
mod live_remote;

#[path = "../package_cli.rs"]
mod package_cli;

#[path = "../power.rs"]
mod power;

#[path = "../release_behavior.rs"]
mod release_behavior;

#[path = "../task_cli.rs"]
mod task_cli;

#[path = "../windows_benchmarks.rs"]
mod windows_benchmarks;

#[path = "../windows_packaging.rs"]
mod windows_packaging;
