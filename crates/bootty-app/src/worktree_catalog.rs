use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::strings::session_name_for_path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreePickerEntry {
    pub label: String,
    pub path: Option<String>,
    pub is_new: bool,
}

fn main_worktree_entry(project_path: &str) -> WorktreePickerEntry {
    WorktreePickerEntry {
        label: format!("{} (main)", session_name_for_path(project_path)),
        path: Some(project_path.to_owned()),
        is_new: false,
    }
}

pub fn discover_worktree_picker_entries(project_path: &str) -> Vec<WorktreePickerEntry> {
    let new_worktree = WorktreePickerEntry {
        label: "New worktree".to_owned(),
        path: None,
        is_new: true,
    };
    let mut command = Command::new("git");
    command.args(["-C", project_path, "worktree", "list", "--porcelain"]);
    hide_command_window(&mut command);
    let output = command.output();
    let Ok(output) = output else {
        return vec![main_worktree_entry(project_path)];
    };
    if !output.status.success() {
        return vec![main_worktree_entry(project_path)];
    }
    let mut entries = vec![new_worktree];
    entries.extend(parse_git_worktree_list(&String::from_utf8_lossy(
        &output.stdout,
    )));
    entries
}

#[cfg(windows)]
fn hide_command_window(command: &mut Command) {
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_command_window(_command: &mut Command) {}

fn parse_git_worktree_list(text: &str) -> Vec<WorktreePickerEntry> {
    let mut entries = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(path) = path.take() {
                let branch = branch
                    .take()
                    .and_then(|branch| branch.rsplit('/').next().map(str::to_owned))
                    .unwrap_or_else(|| "detached".to_owned());
                entries.push(WorktreePickerEntry {
                    label: format!("{} ({branch})", session_name_for_path(&path)),
                    path: Some(path),
                    is_new: false,
                });
            }
        } else if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.to_owned());
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_git_directory_does_not_offer_new_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path_string = dir.path().to_string_lossy().into_owned();

        let entries = discover_worktree_picker_entries(&path_string);

        assert_eq!(entries, vec![main_worktree_entry(&path_string)]);
    }
}
