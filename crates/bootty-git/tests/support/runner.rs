//! Real Git for behavior checks, without the host cancellation polling layer.
#[path = "process.rs"]
mod process;

use bootty_git::Git;
pub use process::RepositoryRunner;

pub const fn git() -> Git<RepositoryRunner> {
    Git::with_runner(RepositoryRunner)
}
