use std::{
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::favorite_paths;

pub use crate::worktree::{
    Git, WorktreeStatus, add_worktree, delete_branch, detach_head, diff_counts,
    discover_worktree_picker_entries, head_branch, main_worktree, mark_occupied_worktrees,
    remove_worktree, status, suggested_session_name, trunk_branch, worktree_count, worktree_root,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProjectPickerEntry {
    pub path: String,
    pub favorite: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorktreePickerEntry {
    pub label: String,
    pub path: Option<String>,
    pub is_new: bool,
    #[serde(default)]
    pub occupied: bool,
}

pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(|name| env::var_os(name))
}

pub fn home_dir_from(mut var: impl FnMut(&str) -> Option<OsString>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(profile) = non_empty_path(var("USERPROFILE")) {
            return Some(profile);
        }
        Some(non_empty_path(var("HOMEDRIVE"))?.join(non_empty_path(var("HOMEPATH"))?))
    }

    #[cfg(not(windows))]
    {
        non_empty_path(var("HOME"))
    }
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

pub fn discover_project_picker_entries(home: Option<&Path>) -> Vec<ProjectPickerEntry> {
    let mut entries = Vec::new();
    for path in read_favorite_project_paths(home) {
        push_project_entry(&mut entries, &path, true);
    }

    if let Some(home) = home {
        for name in ["dotfiles", ".claude", "blueprints"] {
            push_project_entry(&mut entries, &home.join(name), false);
        }
        push_project_entry(&mut entries, home, false);
        for parent in [home.join("src"), home.join(".config")] {
            push_project_children(&mut entries, &parent);
        }
    }
    entries
}

pub fn toggle_favorite_project_path(home: Option<&Path>, project_path: &str) -> io::Result<bool> {
    let Some(path) = favorite_project_paths_file(home) else {
        return Ok(false);
    };
    favorite_paths::toggle_favorite_project_path_at(&path, home, project_path)
}

fn push_project_entry(entries: &mut Vec<ProjectPickerEntry>, path: &Path, favorite: bool) {
    if !path.is_dir() {
        return;
    }
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
            push_project_entry(entries, &child_path, false);
        }
    }
}

fn is_linked_worktree(dir: &Path) -> bool {
    dir.join(".git").is_file()
}

fn is_hidden_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name != ".config")
}

fn favorite_project_paths_file(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|home| home.join(".config/tmux/.session-favorites"))
}

fn read_favorite_project_paths(home: Option<&Path>) -> Vec<PathBuf> {
    favorite_project_paths_file(home)
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| expand_home_path(home, line))
                .collect()
        })
        .unwrap_or_default()
}

fn expand_home_path(home: Option<&Path>, path: &str) -> PathBuf {
    path.strip_prefix("~/")
        .or_else(|| path.strip_prefix(r"~\"))
        .and_then(|path| home.map(|home| home.join(path)))
        .unwrap_or_else(|| PathBuf::from(path))
}

pub(crate) fn main_worktree_entry(project_path: &str) -> WorktreePickerEntry {
    WorktreePickerEntry {
        label: format!("{} (main)", session_name_for_path(project_path)),
        path: Some(project_path.to_owned()),
        ..WorktreePickerEntry::default()
    }
}

pub(crate) fn session_name_for_path(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bootty")
        .trim_end_matches(".git")
}

pub fn display_path(path: &str, home: Option<&Path>) -> String {
    let path = Path::new(path);
    if let Some(home) = home
        && let Ok(relative) = path.strip_prefix(home)
    {
        return Path::new("~").join(relative).display().to_string();
    }
    path.display().to_string()
}
