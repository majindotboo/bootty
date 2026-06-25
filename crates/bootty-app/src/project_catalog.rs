use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::strings::{expand_home_path, home_dir, is_hidden_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPickerEntry {
    pub path: String,
    pub favorite: bool,
}

pub fn discover_project_picker_entries() -> Vec<ProjectPickerEntry> {
    let mut entries = Vec::new();
    let favorites = read_favorite_project_paths();
    for path in &favorites {
        push_project_entry(&mut entries, path.clone(), true);
    }

    if let Some(home) = home_dir() {
        for name in ["dotfiles", ".claude", "blueprints"] {
            push_project_entry(&mut entries, home.join(name), false);
        }
        push_project_entry(&mut entries, home.clone(), false);
        for parent in [home.join("src"), home.join(".config")] {
            push_project_children(&mut entries, &parent);
        }
    }
    entries
}

fn push_project_entry(entries: &mut Vec<ProjectPickerEntry>, path: PathBuf, favorite: bool) {
    if !path.is_dir() {
        return;
    }
    // Keep the path as discovered: canonicalizing would resolve `~/.config`
    // symlinks into their `~/dotfiles/xdg-configs/*` targets, hiding the real
    // project roots the user navigates by.
    let path = path.to_string_lossy().into_owned();
    if let Some(existing) = entries.iter_mut().find(|entry| entry.path == path) {
        existing.favorite |= favorite;
    } else {
        entries.push(ProjectPickerEntry { path, favorite });
    }
}

fn push_project_children(entries: &mut Vec<ProjectPickerEntry>, parent: &Path) {
    let Ok(children) = fs::read_dir(parent) else {
        return;
    };
    for child in children.flatten() {
        let child_path = child.path();
        if child_path.is_dir() && !is_hidden_path(&child_path) && !is_linked_worktree(&child_path) {
            push_project_entry(entries, child_path, false);
        }
    }
}

/// A linked git worktree records its `.git` as a *file* pointing back at the main
/// repo's `worktrees/<name>` directory; the primary checkout keeps `.git` as a
/// directory. Linked worktrees belong in the per-project worktree picker, not in
/// the top-level project list alongside their root.
fn is_linked_worktree(dir: &Path) -> bool {
    dir.join(".git").is_file()
}

fn read_favorite_project_paths() -> Vec<PathBuf> {
    home_dir()
        .map(|home| home.join(".config/tmux/.session-favorites"))
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(expand_home_path)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_worktrees_are_distinguished_from_primary_checkouts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("repo");
        let linked = dir.path().join("repo.wt1");
        // Primary checkout: `.git` is a directory.
        fs::create_dir_all(primary.join(".git")).expect("primary .git dir");
        // Linked worktree: `.git` is a file pointing back at the main repo.
        fs::create_dir(&linked).expect("linked dir");
        fs::write(linked.join(".git"), "gitdir: /main/.git/worktrees/wt1\n")
            .expect("linked .git file");

        assert!(!is_linked_worktree(&primary));
        assert!(is_linked_worktree(&linked));
    }
}
