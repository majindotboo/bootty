use crate::{
    project_catalog::ProjectPickerEntry, strings::display_path,
    worktree_catalog::WorktreePickerEntry,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewMuxSessionStep {
    Project,
    Worktree,
}

pub(super) fn picker_selection_after_navigation(
    selected: usize,
    row_count: usize,
    next: bool,
    previous: bool,
) -> usize {
    if row_count == 0 {
        return 0;
    }
    let mut selected = selected.min(row_count - 1);
    if next {
        selected = (selected + 1).min(row_count - 1);
    }
    if previous {
        selected = selected.saturating_sub(1);
    }
    selected
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

    #[test]
    fn picker_navigation_moves_down_and_up_without_wrapping() {
        assert_eq!(picker_selection_after_navigation(0, 3, true, false), 1);
        assert_eq!(picker_selection_after_navigation(2, 3, true, false), 2);
        assert_eq!(picker_selection_after_navigation(2, 3, false, true), 1);
        assert_eq!(picker_selection_after_navigation(0, 3, false, true), 0);
        assert_eq!(picker_selection_after_navigation(2, 0, true, true), 0);
    }
}
