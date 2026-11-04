//! The operating-system boundary Bootty owns: local IPC endpoints, account identity, isolated
//! process trees and detached helper spawning.
//!
//! Adapted from the `rmux-ipc` and `rmux-os` crates of rmux
//! (<https://github.com/luan/rmux> at `1d56e30`, MIT OR Apache-2.0), keeping only what Bootty
//! calls. Unix code is safe Rust over `rustix`; Win32 calls live in `windows`, the one module
//! that allows `unsafe`.

pub mod daemon;
pub mod identity;
pub mod ipc;
pub mod process_tree;
pub mod spawn;
#[cfg(windows)]
mod windows;
