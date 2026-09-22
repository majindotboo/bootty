//! Directory, worktree, and branch creation states for the new-session workflow.

use super::{NEW_SESSION_ID, normalized_name, parse_index};
use crate::gpui::{DialogAction, DialogIntent, DialogRow, DialogSpec, RowId};
use crate::new_session::{NewSessionEffect, NewSessionOutcome, NewSessionWorker};
use crate::product_dialogs::searchable::{SearchableEntry, SearchableIntent, SearchableList};
use crate::strings::home_dir;
use bootty_config::config::RemoteConfig;
use bootty_git::{
    self as project, ProjectPickerEntry, WorktreePickerEntry, discover_project_picker_entries,
    discover_worktree_picker_entries, toggle_favorite_project_path,
};
use bootty_mux::RepaintHandle;
use std::path::Path;

pub struct NewSessionDialog {
    step: NewSessionStep,
    worker: Option<NewSessionWorker>,
}

enum NewSessionStep {
    Project(ProjectPicker),
    Worktree {
        project: ProjectPickerEntry,
        list: SearchableList<WorktreePickerEntry>,
    },
    BranchName(WorktreeDraft),
}

struct ProjectPicker {
    projects: Vec<ProjectPickerEntry>,
    list: SearchableList<ProjectPickerEntry>,
}

struct WorktreeDraft {
    repo: String,
    branch: String,
    folder: String,
    start_ref: String,
    error: Option<String>,
}

impl NewSessionDialog {
    #[must_use]
    pub fn open() -> Self {
        Self::from_projects(discover_project_picker_entries(
            project::home_dir().as_deref(),
        ))
    }

    pub fn open_local(repaint: RepaintHandle) -> Self {
        Self::new(Vec::new(), Some(NewSessionWorker::local(repaint)))
    }

    /// Build a local picker from an already-owned project catalog.
    #[must_use]
    pub fn from_projects(projects: Vec<ProjectPickerEntry>) -> Self {
        Self::new(projects, None)
    }

    pub fn open_remote(remote: RemoteConfig, repaint: RepaintHandle) -> Self {
        Self::new(Vec::new(), Some(NewSessionWorker::remote(remote, repaint)))
    }

    fn new(projects: Vec<ProjectPickerEntry>, worker: Option<NewSessionWorker>) -> Self {
        let remote = worker.as_ref().is_some_and(NewSessionWorker::is_remote);
        Self {
            step: NewSessionStep::Project(ProjectPicker {
                list: SearchableList::new(project_entries(&projects, remote)),
                projects,
            }),
            worker,
        }
    }

    pub fn poll(&mut self) -> Option<NewSessionPickerEvent> {
        let result = self.worker.as_mut()?.poll()?;
        let remote = self.is_remote();
        match result {
            Ok(NewSessionOutcome::Projects(projects)) => {
                if let NewSessionStep::Project(picker) = &mut self.step {
                    picker.projects = projects;
                    picker.replace_entries(remote);
                    if !remote {
                        let filter = picker.list.filter().to_owned();
                        picker.inject_direct_project(&filter);
                    }
                }
            }
            Ok(NewSessionOutcome::Worktrees(worktrees)) => {
                if let NewSessionStep::Worktree { list, .. } = &mut self.step {
                    if let Some(cwd) = single_unused_worktree_cwd(&worktrees, &[], true) {
                        return Some(NewSessionPickerEvent::CreateSession { cwd });
                    }
                    list.replace_entries(worktree_entries(&worktrees));
                    select_source(list, default_worktree_selection(&worktrees, &[], true));
                }
            }
            Ok(NewSessionOutcome::Favorite { path, favorite }) => {
                if let NewSessionStep::Project(picker) = &mut self.step {
                    picker.set_favorite(&path, favorite, remote);
                }
            }
            Ok(NewSessionOutcome::CreatedWorktree(cwd)) => {
                return Some(NewSessionPickerEvent::CreateSession { cwd });
            }
            Err(error) => match &mut self.step {
                NewSessionStep::BranchName(draft) => draft.error = Some(error),
                _ => return Some(NewSessionPickerEvent::Error(error)),
            },
        }
        None
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let busy = self.worker_busy();
        let remote = self.is_remote();
        let (rows, title, icon, hint, empty, text_hint, filter) = match &self.step {
            NewSessionStep::BranchName(draft) => return draft.spec(remote, busy),
            NewSessionStep::Project(picker) => (
                picker.rows(remote, !busy),
                "Directory",
                "folder",
                "Enter open   Ctrl+Shift+F favorite   Esc close",
                match (busy, remote) {
                    (false, _) => "no matching directories",
                    (true, true) => "loading remote projects…",
                    (true, false) => "loading directories…",
                },
                "filter directories…",
                picker.list.filter(),
            ),
            NewSessionStep::Worktree { list, .. } => (
                list.rows()
                    .into_iter()
                    .enumerate()
                    .map(|(visible, row)| {
                        picker_row(
                            RowId::new(visible.to_string()),
                            if row.value.is_new {
                                "plus"
                            } else {
                                "git-branch"
                            },
                            row.value.label.clone(),
                            !busy,
                        )
                    })
                    .collect(),
                "Worktree",
                "git-branch",
                "Enter create session   Esc close",
                match (busy, remote) {
                    (false, _) => "no matching worktrees",
                    (true, true) => "loading remote worktrees…",
                    (true, false) => "loading worktrees…",
                },
                "filter worktrees…",
                list.filter(),
            ),
        };
        let mut spec = DialogSpec::searchable(NEW_SESSION_ID, title, filter, rows);
        spec.icon = Some(icon.to_owned());
        spec.hint = Some(hint.to_owned());
        empty.clone_into(&mut spec.empty_text);
        spec.text_hint = Some(text_hint.to_owned());
        spec
    }

    pub fn apply(
        &mut self,
        intent: &DialogIntent,
        open_cwds: &[String],
    ) -> Option<NewSessionPickerEvent> {
        if intent.dialog_id().0 != NEW_SESSION_ID {
            return None;
        }
        match intent {
            DialogIntent::Dismiss { .. } => Some(NewSessionPickerEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.change_text(value);
                None
            }
            DialogIntent::FieldChanged { field, value, .. } => {
                if let NewSessionStep::BranchName(draft) = &mut self.step {
                    match field.as_str() {
                        "folder" => draft.folder.clone_from(value),
                        "start-ref" => draft.start_ref.clone_from(value),
                        _ => return None,
                    }
                    draft.error = None;
                }
                None
            }
            DialogIntent::SelectionChanged { row, .. } => {
                self.select_row(row);
                None
            }
            DialogIntent::ToggleFavorite { row, .. }
                if matches!(self.step, NewSessionStep::Project(_)) && !self.worker_busy() =>
            {
                self.select_row(row)?;
                self.toggle_project_favorite()
            }
            DialogIntent::Activate { row, .. } if !self.worker_busy() => {
                self.select_row(row)?;
                self.activate_selected(open_cwds)
            }
            _ => None,
        }
    }

    fn change_text(&mut self, value: &str) {
        let remote = self.is_remote();
        match &mut self.step {
            NewSessionStep::Project(picker) => {
                picker
                    .list
                    .apply(SearchableIntent::SetFilter(value.to_owned()));
                if !remote {
                    picker.inject_direct_project(value);
                }
            }
            NewSessionStep::Worktree { list, .. } => {
                list.apply(SearchableIntent::SetFilter(value.to_owned()));
            }
            NewSessionStep::BranchName(draft) => {
                value.clone_into(&mut draft.branch);
                draft.error = None;
            }
        }
    }

    fn select_row(&mut self, row: &RowId) -> Option<()> {
        let remote = self.is_remote();
        match &mut self.step {
            NewSessionStep::Project(picker) => {
                let index = picker.index_for_path(row.0.strip_prefix("project:")?, remote)?;
                picker.list.apply(SearchableIntent::Select(index));
            }
            NewSessionStep::Worktree { list, .. } => {
                list.apply(SearchableIntent::Select(parse_index(row)?));
            }
            NewSessionStep::BranchName(_) => {}
        }
        Some(())
    }

    fn activate_selected(&mut self, open_cwds: &[String]) -> Option<NewSessionPickerEvent> {
        match &self.step {
            NewSessionStep::Project(picker) => {
                self.activate_project(picker.list.selected_value()?.clone(), open_cwds)
            }
            NewSessionStep::Worktree { project, list } => {
                let worktree = list.selected_value()?;
                if worktree.is_new {
                    self.step = NewSessionStep::BranchName(WorktreeDraft {
                        repo: project.path.clone(),
                        branch: String::new(),
                        folder: String::new(),
                        start_ref: String::new(),
                        error: None,
                    });
                    None
                } else {
                    Some(
                        worktree
                            .path
                            .clone()
                            .map_or(NewSessionPickerEvent::Close, |cwd| {
                                NewSessionPickerEvent::CreateSession { cwd }
                            }),
                    )
                }
            }
            NewSessionStep::BranchName(_) => self.create_worktree(),
        }
    }

    fn activate_project(
        &mut self,
        project: ProjectPickerEntry,
        open_cwds: &[String],
    ) -> Option<NewSessionPickerEvent> {
        let list = if let Some(worker) = &mut self.worker {
            worker.start(NewSessionEffect::ListWorktrees(
                project.path.clone(),
                open_cwds.to_vec(),
            ));
            SearchableList::new(Vec::new())
        } else {
            let worktrees = discover_worktree_picker_entries(&project.path);
            if let Some(cwd) = single_unused_worktree_cwd(&worktrees, open_cwds, false) {
                return Some(NewSessionPickerEvent::CreateSession { cwd });
            }
            let mut list = SearchableList::new(worktree_entries(&worktrees));
            select_source(
                &mut list,
                default_worktree_selection(&worktrees, open_cwds, false),
            );
            list
        };
        self.step = NewSessionStep::Worktree { project, list };
        None
    }

    fn create_worktree(&mut self) -> Option<NewSessionPickerEvent> {
        let NewSessionStep::BranchName(draft) = &mut self.step else {
            return None;
        };
        let request = bootty_git::WorktreeRequest {
            branch: normalized_name(&draft.branch)?,
            name: normalized_name(&draft.folder),
            start_ref: normalized_name(&draft.start_ref),
        };
        if let Err(error) = request.validate() {
            draft.error = Some(error);
            return None;
        }
        draft.error = None;
        let repo = draft.repo.clone();
        if let Some(worker) = &mut self.worker {
            worker.start(NewSessionEffect::CreateWorktree(repo, request));
            None
        } else {
            // The synchronous, already-owned catalog seam returns work to its caller.
            Some(NewSessionPickerEvent::CreateWorktree { repo, request })
        }
    }

    fn toggle_project_favorite(&mut self) -> Option<NewSessionPickerEvent> {
        let remote = self.is_remote();
        let NewSessionStep::Project(picker) = &mut self.step else {
            return None;
        };
        let project = picker.list.selected_value()?.clone();
        if let Some(worker) = &mut self.worker {
            worker.start(NewSessionEffect::ToggleFavorite(project.path));
            return None;
        }
        match toggle_favorite_project_path(project::home_dir().as_deref(), &project.path) {
            Ok(favorite) => {
                picker.set_favorite(&project.path, favorite, remote);
                None
            }
            Err(error) => Some(NewSessionPickerEvent::Error(format!(
                "favorite {}: {error}",
                display_project_path(&project.path, false)
            ))),
        }
    }

    fn worker_busy(&self) -> bool {
        self.worker.as_ref().is_some_and(NewSessionWorker::is_busy)
    }

    fn is_remote(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(NewSessionWorker::is_remote)
    }
}

impl ProjectPicker {
    fn inject_direct_project(&mut self, filter: &str) {
        let Some(project) = direct_project_entry(filter) else {
            self.list
                .replace_entries(project_entries(&self.projects, false));
            return;
        };
        let mut projects = self.projects.clone();
        if !projects
            .iter()
            .any(|existing| same_dir(&existing.path, &project.path, false))
        {
            projects.insert(0, project);
        }
        self.list.replace_entries(project_entries(&projects, false));
        self.list
            .apply(SearchableIntent::SetFilter(filter.to_owned()));
    }

    fn set_favorite(&mut self, path: &str, favorite: bool, remote: bool) {
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| same_dir(&project.path, path, remote))
        {
            project.favorite = favorite;
        } else if favorite {
            self.projects.push(ProjectPickerEntry {
                path: path.to_owned(),
                favorite,
            });
        }
        self.replace_entries(remote);
    }

    fn replace_entries(&mut self, remote: bool) {
        let selected = self
            .list
            .selected_value()
            .map(|project| project.path.clone());
        self.list
            .replace_entries(project_entries(&self.projects, remote));
        if let Some(index) = selected.and_then(|path| self.index_for_path(&path, remote)) {
            self.list.apply(SearchableIntent::Select(index));
        }
    }

    fn index_for_path(&self, path: &str, remote: bool) -> Option<usize> {
        self.list
            .rows()
            .into_iter()
            .position(|row| same_dir(&row.value.path, path, remote))
    }
    fn rows(&self, remote: bool, enabled: bool) -> Vec<DialogRow> {
        let mut favorites = Vec::new();
        let mut directories = Vec::new();
        for row in self.list.rows() {
            let project = row.value;
            let target = if project.favorite {
                &mut favorites
            } else {
                &mut directories
            };
            target.push(picker_row(
                project_row_id(&project.path),
                if project.favorite { "star" } else { "folder" },
                display_project_path(&project.path, remote),
                enabled,
            ));
        }

        let mut rows = Vec::with_capacity(
            favorites
                .len()
                .saturating_add(directories.len())
                .saturating_add(2),
        );
        if !favorites.is_empty() {
            rows.push(DialogRow::section("favorites", "Favorites"));
            rows.extend(favorites);
        }
        if !directories.is_empty() {
            rows.push(DialogRow::section("directories", "Directories"));
            rows.extend(directories);
        }
        rows
    }
}

impl WorktreeDraft {
    fn spec(&self, remote: bool, busy: bool) -> DialogSpec {
        let repo = display_project_path(&self.repo, remote);
        let mut spec = DialogSpec::prompt(
            NEW_SESSION_ID,
            "New Worktree",
            &self.branch,
            "branch name…",
            DialogAction::new("create-worktree"),
        );
        spec.icon = Some("git-branch".to_owned());
        spec.busy = busy;
        spec.text_label = Some("Branch".to_owned());
        spec.fields = vec![
            crate::gpui::DialogField {
                kind: crate::gpui::DialogFieldKind::Text,
                id: "folder".to_owned(),
                label: "Folder name (optional)".to_owned(),
                value: self.folder.clone(),
                placeholder: "repository-branch".to_owned(),
            },
            crate::gpui::DialogField {
                kind: crate::gpui::DialogFieldKind::Text,
                id: "start-ref".to_owned(),
                label: "Start from".to_owned(),
                value: self.start_ref.clone(),
                placeholder: "HEAD — or a branch, tag or commit".to_owned(),
            },
        ];
        spec.footer = Some(format!("Creates a sibling checkout of {repo}."));
        if let Some(row) = spec.rows.first_mut() {
            (if busy {
                "Creating…"
            } else {
                "Create Worktree"
            })
            .clone_into(&mut row.label);
            row.detail.clone_from(&self.error);
            row.enabled = !self.branch.trim().is_empty() && !busy;
        }
        spec
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewSessionPickerEvent {
    Close,
    Error(String),
    CreateWorktree {
        repo: String,
        request: bootty_git::WorktreeRequest,
    },
    CreateSession {
        cwd: String,
    },
}

fn project_entries(
    projects: &[ProjectPickerEntry],
    remote: bool,
) -> Vec<SearchableEntry<ProjectPickerEntry>> {
    projects
        .iter()
        .filter(|project| project.favorite)
        .chain(projects.iter().filter(|project| !project.favorite))
        .map(|project| {
            SearchableEntry::new(project.clone(), display_project_path(&project.path, remote))
        })
        .collect()
}

fn picker_row(id: RowId, icon: &str, label: String, enabled: bool) -> DialogRow {
    DialogRow {
        id,
        icon: Some(icon.to_owned()),
        label,
        color: None,
        detail: None,
        trailing: None,
        keybinding: None,
        current: false,
        enabled,
        destructive: false,
        action: enabled.then(|| DialogAction::new("activate")),
        preview: None,
    }
}

fn project_row_id(path: &str) -> RowId {
    RowId::new(format!("project:{path}"))
}

fn worktree_entries(
    worktrees: &[WorktreePickerEntry],
) -> Vec<SearchableEntry<WorktreePickerEntry>> {
    worktrees
        .iter()
        .cloned()
        .map(|worktree| SearchableEntry::new(worktree.clone(), worktree.label))
        .collect()
}

fn display_project_path(path: &str, remote: bool) -> String {
    if remote {
        path.to_owned()
    } else {
        bootty_git::project::display_path(path, home_dir().as_deref())
    }
}

fn direct_project_entry(filter: &str) -> Option<ProjectPickerEntry> {
    let filter = filter.trim();
    if !looks_like_directory_path(filter) {
        return None;
    }
    let path = crate::strings::expand_home_path(filter);
    path.is_dir().then(|| ProjectPickerEntry {
        path: path
            .canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned(),
        favorite: false,
    })
}

fn looks_like_directory_path(filter: &str) -> bool {
    Path::new(filter).has_root()
        || filter.starts_with("~/")
        || filter.starts_with("./")
        || filter.starts_with("../")
        || cfg!(windows)
            && (filter.starts_with(r"~\")
                || filter.starts_with(r".\")
                || filter.starts_with(r"..\"))
}

fn same_dir(a: &str, b: &str, remote: bool) -> bool {
    if remote {
        return a.trim_end_matches(['/', '\\']) == b.trim_end_matches(['/', '\\']);
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.trim_end_matches('/') == b.trim_end_matches('/'),
    }
}

fn single_unused_worktree_cwd(
    entries: &[WorktreePickerEntry],
    open_cwds: &[String],
    remote: bool,
) -> Option<String> {
    let mut real = entries.iter().filter(|entry| !entry.is_new);
    let only = real.next()?;
    if real.next().is_some() || worktree_is_open(only, open_cwds, remote) {
        return None;
    }
    only.path.clone()
}

fn default_worktree_selection(
    entries: &[WorktreePickerEntry],
    open_cwds: &[String],
    remote: bool,
) -> usize {
    entries
        .iter()
        .position(|entry| !entry.is_new && !worktree_is_open(entry, open_cwds, remote))
        .unwrap_or(0)
}

fn worktree_is_open(entry: &WorktreePickerEntry, open_cwds: &[String], remote: bool) -> bool {
    if remote {
        return entry.occupied;
    }
    entry
        .path
        .as_deref()
        .is_some_and(|path| open_cwds.iter().any(|cwd| same_dir(cwd, path, false)))
}

fn select_source<T>(list: &mut SearchableList<T>, source: usize) {
    if let Some(visible) = list
        .rows()
        .iter()
        .position(|row| row.source_index == source)
    {
        list.apply(SearchableIntent::Select(visible));
    }
}
