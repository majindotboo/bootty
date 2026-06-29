use std::{
    fs, io,
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

fn favorite_project_paths_file() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".config/tmux/.session-favorites"))
}

fn read_favorite_project_paths() -> Vec<PathBuf> {
    favorite_project_paths_file()
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

pub fn toggle_favorite_project_path(project_path: &str) -> io::Result<bool> {
    let Some(path) = favorite_project_paths_file() else {
        return Ok(false);
    };
    toggle_favorite_project_path_at(&path, project_path)
}

fn toggle_favorite_project_path_at(favorites_file: &Path, project_path: &str) -> io::Result<bool> {
    let content = match fs::read_to_string(favorites_file) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let selected = PathBuf::from(project_path);
    let mut lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(index) = lines
        .iter()
        .position(|line| same_project_path(&expand_home_path(line), &selected))
    {
        lines.remove(index);
        write_favorite_project_paths(favorites_file, &lines)?;
        return Ok(false);
    }
    lines.push(project_path.to_owned());
    write_favorite_project_paths(favorites_file, &lines)?;
    Ok(true)
}

fn write_favorite_project_paths(favorites_file: &Path, lines: &[String]) -> io::Result<()> {
    if let Some(parent) = favorites_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(favorites_file, content)
}

fn same_project_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
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

    #[test]
    fn toggle_favorite_project_path_adds_and_removes_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let favorites = dir.path().join("nested/.session-favorites");
        let project = dir.path().join("project");
        fs::create_dir(&project).expect("project dir");
        let project = project.to_string_lossy().into_owned();

        assert!(toggle_favorite_project_path_at(&favorites, &project).expect("favorite"));
        assert_eq!(
            fs::read_to_string(&favorites).expect("favorites file"),
            format!("{project}\n")
        );

        assert!(!toggle_favorite_project_path_at(&favorites, &project).expect("unfavorite"));
        assert_eq!(fs::read_to_string(&favorites).expect("favorites file"), "");
    }

    #[test]
    fn toggle_favorite_project_path_matches_existing_canonical_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let favorites = dir.path().join(".session-favorites");
        let project = dir.path().join("project");
        fs::create_dir(&project).expect("project dir");
        fs::write(&favorites, format!("{}\n", project.display())).expect("write favorites");

        let selected = project.join(".").to_string_lossy().into_owned();

        assert!(!toggle_favorite_project_path_at(&favorites, &selected).expect("unfavorite"));
        assert_eq!(fs::read_to_string(&favorites).expect("favorites file"), "");
    }
}
