use std::path::{Path, PathBuf};

use crate::{
    project_catalog::ProjectPickerEntry,
    strings::{display_path, expand_home_path},
    worktree_catalog::WorktreePickerEntry,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewMuxSessionStep {
    Project,
    Worktree,
    BranchName,
}

pub(super) fn filtered_project_entries(
    entries: &[ProjectPickerEntry],
    filter: &str,
) -> Vec<ProjectPickerEntry> {
    let filter = filter.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| {
            filter.is_empty()
                || display_path(&entry.path)
                    .to_ascii_lowercase()
                    .contains(&filter)
        })
        .cloned()
        .collect()
}

pub(super) fn project_entries_for_filter(
    entries: &[ProjectPickerEntry],
    filter: &str,
) -> Vec<ProjectPickerEntry> {
    let mut filtered = filtered_project_entries(entries, filter);
    if let Some(entry) = direct_project_entry(filter)
        && !filtered
            .iter()
            .any(|existing| same_project_path(&existing.path, &entry.path))
    {
        filtered.insert(0, entry);
    }
    filtered
}

fn direct_project_entry(filter: &str) -> Option<ProjectPickerEntry> {
    let filter = filter.trim();
    if !looks_like_directory_path(filter) {
        return None;
    }
    let path = expand_home_path(filter);
    path.is_dir().then(|| ProjectPickerEntry {
        path: normalize_path_for_session(&path),
        favorite: false,
    })
}

fn looks_like_directory_path(filter: &str) -> bool {
    Path::new(filter).has_root()
        || filter.starts_with("~/")
        || filter.starts_with("./")
        || filter.starts_with("../")
        || looks_like_windows_relative_path(filter)
}

#[cfg(windows)]
fn looks_like_windows_relative_path(filter: &str) -> bool {
    filter.starts_with(r"~\") || filter.starts_with(r".\") || filter.starts_with(r"..\")
}

#[cfg(not(windows))]
fn looks_like_windows_relative_path(_filter: &str) -> bool {
    false
}

fn normalize_path_for_session(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn same_project_path(a: &str, b: &str) -> bool {
    let a = PathBuf::from(a);
    let b = PathBuf::from(b);
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub(super) fn filtered_worktree_entries(
    entries: &[WorktreePickerEntry],
    filter: &str,
) -> Vec<WorktreePickerEntry> {
    let filter = filter.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| filter.is_empty() || entry.label.to_ascii_lowercase().contains(&filter))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(path: &str, favorite: bool) -> ProjectPickerEntry {
        ProjectPickerEntry {
            path: path.to_owned(),
            favorite,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bootty-new-session-picker-{name}-{}",
            std::process::id()
        ));
        _ = std::fs::remove_dir(&path);
        std::fs::create_dir(&path).expect("create temp dir");
        path
    }

    #[test]
    fn project_filter_matches_path_substring_case_insensitively() {
        let entries = vec![
            project("/Users/luan/src/bootty", true),
            project("/Users/luan/src/dotfiles", false),
        ];

        assert_eq!(filtered_project_entries(&entries, "BOOT").len(), 1);
        assert_eq!(filtered_project_entries(&entries, "src").len(), 2);
        assert_eq!(filtered_project_entries(&entries, "missing").len(), 0);
        assert_eq!(filtered_project_entries(&entries, "").len(), 2);
    }

    #[test]
    fn project_filter_keeps_plain_search_terms_as_filters() {
        let path = temp_dir("plain");

        let entries = project_entries_for_filter(&[], path.file_name().unwrap().to_str().unwrap());

        assert!(entries.is_empty());
        _ = std::fs::remove_dir(path);
    }

    #[test]
    fn project_filter_allows_opening_direct_directory_paths() {
        let path = temp_dir("direct");

        let entries = project_entries_for_filter(&[], path.to_str().expect("utf-8 temp path"));

        assert_eq!(entries.len(), 1);
        assert_eq!(
            PathBuf::from(&entries[0].path),
            path.canonicalize().unwrap()
        );
        _ = std::fs::remove_dir(path);
    }

    #[cfg(windows)]
    #[test]
    fn project_filter_treats_windows_path_syntax_as_direct_paths() {
        assert!(looks_like_directory_path(r"C:\Users\bootty"));
        assert!(looks_like_directory_path(r".\bootty"));
        assert!(looks_like_directory_path(r"..\bootty"));
        assert!(looks_like_directory_path(r"~\src"));
    }

    #[test]
    fn project_filter_does_not_duplicate_matching_direct_paths() {
        let path = temp_dir("known");
        let known = project(path.to_str().expect("utf-8 temp path"), true);

        let entries = project_entries_for_filter(&[known], path.to_str().expect("utf-8 temp path"));

        assert_eq!(entries.len(), 1);
        _ = std::fs::remove_dir(path);
    }

    #[test]
    fn project_filter_prioritizes_direct_directory_paths_over_substring_matches() {
        let path = temp_dir("priority");
        let known = project(&format!("{}/child", path.display()), true);

        let entries = project_entries_for_filter(&[known], path.to_str().expect("utf-8 temp path"));

        assert_eq!(
            PathBuf::from(&entries[0].path),
            path.canonicalize().unwrap()
        );
        _ = std::fs::remove_dir(path);
    }
}
