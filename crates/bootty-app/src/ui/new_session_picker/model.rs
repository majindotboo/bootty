use crate::{
    project_catalog::ProjectPickerEntry, strings::display_path,
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
}
