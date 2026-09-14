pub mod changes;
pub mod facts;
mod favorite_paths;
pub mod project;
pub mod runner;
pub mod worktree;

pub use facts::{
    BranchStatus, GitFacts, GitFactsCache, GitSessionFacts, GitSessionFactsInput,
    WorktreeRevisionCache,
};
pub use project::{
    Git, ProjectPickerEntry, WorktreePickerEntry, WorktreeStatus, add_worktree, delete_branch,
    detach_head, diff_counts, discover_project_picker_entries, discover_worktree_picker_entries,
    display_path, head_branch, home_dir, home_dir_from, main_worktree, mark_occupied_worktrees,
    remove_worktree, status, suggested_session_name, toggle_favorite_project_path, trunk_branch,
    worktree_count, worktree_root,
};
pub use runner::{CommandOutput, CommandRunner, SystemCommandRunner};

pub use worktree::WorktreeRequest;
