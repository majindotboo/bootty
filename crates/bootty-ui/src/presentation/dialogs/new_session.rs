//! Directory, worktree, and branch creation states for the new-session workflow.

use super::{NEW_SESSION_ID, normalized_name};
use crate::gpui::{
    DialogAction, DialogField, DialogFieldKind, DialogIntent, DialogRole, DialogRow, DialogSpec,
    RowId,
};
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
    project_picker: Option<ProjectPicker>,
    checkout: Option<String>,
    draft: String,
    mode: LaunchMode,
    provider: bootty_agents::AgentKind,
}

#[derive(Clone, Copy)]
enum LaunchMode {
    Agent,
    Terminal,
}

enum NewSessionStep {
    Project(ProjectPicker),
    Worktree {
        project: ProjectPickerEntry,
        main: String,
        list: SearchableList<WorktreePickerEntry>,
    },
    BranchName(WorktreeDraft),
    Launch {
        cwd: String,
    },
}

struct ProjectPicker {
    projects: Vec<ProjectPickerEntry>,
    list: SearchableList<ProjectPickerEntry>,
}

struct WorktreeDraft {
    repo: String,
    main: String,
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
            project_picker: None,
            checkout: None,
            draft: String::new(),
            mode: if remote {
                LaunchMode::Terminal
            } else {
                LaunchMode::Agent
            },
            provider: bootty_agents::AgentKind::Codex,
        }
    }

    pub fn set_directory(&mut self, path: String) {
        if self.is_remote() {
            return;
        }
        if let NewSessionStep::Project(picker) = &mut self.step {
            picker.inject_direct_project(&path, false);
            picker.list.apply(SearchableIntent::SetFilter(path));
        }
    }

    pub fn set_checkout(&mut self, cwd: String) {
        self.checkout = Some(cwd.clone());
        if let NewSessionStep::Project(picker) =
            std::mem::replace(&mut self.step, NewSessionStep::Launch { cwd })
        {
            self.project_picker = Some(picker);
        }
    }

    pub fn poll(&mut self) -> Option<NewSessionPickerEvent> {
        let result = self.worker.as_mut()?.poll()?;
        let remote = self.is_remote();
        match result {
            Ok(NewSessionOutcome::Projects(projects)) => {
                let picker = match &mut self.step {
                    NewSessionStep::Project(picker) => Some(picker),
                    _ => self.project_picker.as_mut(),
                };
                if let Some(picker) = picker {
                    picker.projects = projects;
                    picker.replace_entries(remote);
                    if !remote {
                        let filter = picker.list.filter().to_owned();
                        picker.inject_direct_project(&filter, false);
                    }
                }
            }
            Ok(NewSessionOutcome::Worktrees(worktrees)) => {
                if let NewSessionStep::Worktree { main, list, .. } = &mut self.step {
                    if let Some(path) = worktrees
                        .iter()
                        .find(|entry| !entry.is_new)
                        .and_then(|entry| entry.path.as_ref())
                    {
                        main.clone_from(path);
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
            Ok(NewSessionOutcome::CreatedWorktree(cwd)) => self.set_checkout(cwd),
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
            NewSessionStep::Launch { cwd } => {
                return launch_spec(cwd, remote, &self.draft, self.mode, self.provider, busy);
            }
            NewSessionStep::Project(picker) => (
                picker.rows(remote, !busy),
                "Choose project",
                "folder",
                "Choose a project   Ctrl+Shift+F favorite   Esc close",
                match (busy, remote) {
                    (false, _) => "no matching directories",
                    (true, true) => "loading remote projects…",
                    (true, false) => "loading directories…",
                },
                "Search projects or enter a folder path…",
                picker.list.filter(),
            ),
            NewSessionStep::Worktree { list, .. } => (
                list.rows()
                    .into_iter()
                    .map(|row| {
                        worktree_row(
                            worktree_row_id(row.value),
                            if row.value.is_new {
                                "plus"
                            } else {
                                "git-branch"
                            },
                            row.value.label.clone(),
                            !busy,
                            row.value,
                        )
                    })
                    .collect(),
                "Choose checkout",
                "git-branch",
                "Enter choose checkout   Esc close",
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
        if matches!(self.step, NewSessionStep::Project(_))
            && let Some(cwd) = &self.checkout
            && filter.is_empty()
        {
            let mut row = picker_row(
                RowId::new("current-checkout"),
                "arrow-left",
                "Use selected checkout".to_owned(),
                true,
            );
            row.detail = Some(display_project_path(cwd, remote));
            spec.rows.insert(0, row);
        }
        if let NewSessionStep::Worktree { project, .. } = &self.step {
            spec.footer = Some(display_project_path(&project.path, remote));
        }
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
                if matches!(self.step, NewSessionStep::Launch { .. }) {
                    match (field.as_str(), value.as_str()) {
                        ("mode", "Agent") if !self.is_remote() => self.mode = LaunchMode::Agent,
                        ("mode", "Terminal") => self.mode = LaunchMode::Terminal,
                        ("provider", "Codex") => self.provider = bootty_agents::AgentKind::Codex,
                        ("provider", "Claude") => self.provider = bootty_agents::AgentKind::Claude,
                        ("provider", "Pi") => self.provider = bootty_agents::AgentKind::Pi,
                        _ => {}
                    }
                } else if let NewSessionStep::BranchName(draft) = &mut self.step {
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
            DialogIntent::Activate { row, .. }
                if !self.worker_busy()
                    || matches!(self.step, NewSessionStep::Launch { .. })
                        && !matches!(row.0.as_str(), "choose-project" | "choose-checkout") =>
            {
                if row.0 == "current-checkout" {
                    self.set_checkout(self.checkout.clone()?);
                    return None;
                }
                if row.0 == "browse-directory" && !self.is_remote() {
                    return Some(NewSessionPickerEvent::BrowseDirectory);
                }
                if let NewSessionStep::Launch { cwd } = &self.step {
                    if row.0 == "choose-project" {
                        self.step = NewSessionStep::Project(self.project_picker.take()?);
                        return None;
                    }
                    if row.0 == "choose-checkout" {
                        return self.activate_project(
                            ProjectPickerEntry {
                                path: cwd.clone(),
                                favorite: false,
                                icon: None,
                            },
                            open_cwds,
                        );
                    }
                    if row.0 != "submit" {
                        return None;
                    }
                    return Some(match self.mode {
                        LaunchMode::Terminal => NewSessionPickerEvent::CreateSession {
                            cwd: cwd.clone(),
                            command: (!self.draft.trim().is_empty()).then(|| self.draft.clone()),
                        },
                        LaunchMode::Agent if !self.is_remote() => {
                            NewSessionPickerEvent::CreateAgentSession {
                                cwd: cwd.clone(),
                                provider: self.provider,
                                prompt: self.draft.clone(),
                            }
                        }
                        LaunchMode::Agent => return None,
                    });
                }
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
                picker.inject_direct_project(value, remote);
            }
            NewSessionStep::Worktree { list, .. } => {
                list.apply(SearchableIntent::SetFilter(value.to_owned()));
            }
            NewSessionStep::Launch { .. } => value.clone_into(&mut self.draft),
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
                let index = list
                    .rows()
                    .iter()
                    .position(|entry| worktree_row_id(entry.value) == *row)?;
                list.apply(SearchableIntent::Select(index));
            }
            NewSessionStep::BranchName(_) | NewSessionStep::Launch { .. } => {}
        }
        Some(())
    }

    fn activate_selected(&mut self, open_cwds: &[String]) -> Option<NewSessionPickerEvent> {
        match &self.step {
            NewSessionStep::Project(picker) => {
                self.activate_project(picker.list.selected_value()?.clone(), open_cwds)
            }
            NewSessionStep::Worktree {
                project,
                main,
                list,
            } => {
                let worktree = list.selected_value()?;
                if worktree.is_new {
                    self.step = NewSessionStep::BranchName(WorktreeDraft {
                        repo: project.path.clone(),
                        main: main.clone(),
                        branch: generated_task_branch(&self.draft),
                        folder: String::new(),
                        start_ref: String::new(),
                        error: None,
                    });
                } else {
                    let cwd = worktree.path.clone()?;
                    self.set_checkout(cwd);
                }
                None
            }
            NewSessionStep::BranchName(_) => self.create_worktree(),
            NewSessionStep::Launch { .. } => None,
        }
    }

    fn activate_project(
        &mut self,
        project: ProjectPickerEntry,
        open_cwds: &[String],
    ) -> Option<NewSessionPickerEvent> {
        let mut main = project.path.clone();
        let list = if let Some(worker) = &mut self.worker {
            worker.start(NewSessionEffect::ListWorktrees(
                project.path.clone(),
                open_cwds.to_vec(),
            ));
            SearchableList::new(Vec::new())
        } else {
            let worktrees = discover_worktree_picker_entries(&project.path);
            if let Some(path) = worktrees
                .iter()
                .find(|entry| !entry.is_new)
                .and_then(|entry| entry.path.as_ref())
            {
                main.clone_from(path);
            }
            let mut list = SearchableList::new(worktree_entries(&worktrees));
            select_source(
                &mut list,
                default_worktree_selection(&worktrees, open_cwds, false),
            );
            list
        };
        if let NewSessionStep::Project(picker) = std::mem::replace(
            &mut self.step,
            NewSessionStep::Worktree {
                project,
                main,
                list,
            },
        ) {
            self.project_picker = Some(picker);
        }
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
    fn inject_direct_project(&mut self, filter: &str, remote: bool) {
        let Some(project) = direct_project_entry(filter, remote) else {
            self.list
                .replace_entries(project_entries(&self.projects, remote));
            return;
        };
        let mut projects = self.projects.clone();
        if !projects
            .iter()
            .any(|existing| same_dir(&existing.path, &project.path, remote))
        {
            projects.insert(0, project);
        }
        self.list
            .replace_entries(project_entries(&projects, remote));
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
                icon: None,
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
            let mut item = picker_row(
                project_row_id(&project.path),
                if project.favorite { "star" } else { "folder" },
                project_name(&project.path),
                enabled,
            );
            item.artwork = project.icon.clone().map(std::sync::Arc::new);
            item.detail = Some(display_project_path(&project.path, remote));
            item.trailing = project.favorite.then(|| "Favorite".to_owned());
            target.push(item);
        }

        let mut rows = Vec::with_capacity(
            favorites
                .len()
                .saturating_add(directories.len())
                .saturating_add(2),
        );
        if !remote && enabled && self.list.filter().is_empty() {
            rows.push(picker_row(
                RowId::new("browse-directory"),
                "folder-open",
                "Choose folder…".to_owned(),
                enabled,
            ));
        }
        if !favorites.is_empty() {
            rows.push(DialogRow::section("favorites", "Favorites"));
            rows.extend(favorites);
        }
        if !directories.is_empty() {
            rows.push(DialogRow::section("directories", "Projects"));
            rows.extend(directories);
        }
        rows
    }
}

impl WorktreeDraft {
    fn spec(&self, remote: bool, busy: bool) -> DialogSpec {
        let mut spec = DialogSpec::prompt(
            NEW_SESSION_ID,
            "New worktree",
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
        let request = bootty_git::WorktreeRequest {
            branch: self.branch.trim().to_owned(),
            name: normalized_name(&self.folder),
            start_ref: normalized_name(&self.start_ref),
        };
        spec.footer = Some(request.destination(&self.main).map_or_else(
            |error| error,
            |path| format!("Destination: {}", display_project_path(&path, remote)),
        ));
        if let Some(row) = spec.rows.first_mut() {
            (if busy {
                "Creating…"
            } else {
                "Create worktree"
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
    BrowseDirectory,
    Error(String),
    CreateWorktree {
        repo: String,
        request: bootty_git::WorktreeRequest,
    },
    CreateSession {
        cwd: String,
        command: Option<String>,
    },
    CreateAgentSession {
        cwd: String,
        provider: bootty_agents::AgentKind,
        prompt: String,
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
            let mut entry =
                SearchableEntry::new(project.clone(), display_project_path(&project.path, remote));
            // Display paths may abbreviate home; absolute path input must still find the project.
            entry.keywords.push(project.path.clone());
            entry
        })
        .collect()
}

fn picker_row(id: RowId, icon: &str, label: String, enabled: bool) -> DialogRow {
    DialogRow {
        id,
        artwork: None,
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

fn direct_project_entry(filter: &str, remote: bool) -> Option<ProjectPickerEntry> {
    let filter = filter.trim();
    if !looks_like_directory_path(filter) {
        return None;
    }
    let path = if remote {
        filter.to_owned()
    } else {
        crate::strings::expand_home_path(filter)
            .to_string_lossy()
            .into_owned()
    };
    Some(ProjectPickerEntry {
        path,
        favorite: false,
        icon: None,
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

fn same_dir(a: &str, b: &str, _remote: bool) -> bool {
    a.trim_end_matches(['/', '\\']) == b.trim_end_matches(['/', '\\'])
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

fn project_name(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn worktree_row(
    id: RowId,
    icon: &str,
    label: String,
    enabled: bool,
    worktree: &WorktreePickerEntry,
) -> DialogRow {
    let mut row = picker_row(id, icon, label, enabled);
    row.detail.clone_from(&worktree.path);
    row.trailing = worktree.occupied.then(|| "Open session".to_owned());
    row
}

fn launch_spec(
    cwd: &str,
    remote: bool,
    draft: &str,
    mode: LaunchMode,
    provider: bootty_agents::AgentKind,
    catalog_loading: bool,
) -> DialogSpec {
    let provider_name = match provider {
        bootty_agents::AgentKind::Codex => "Codex",
        bootty_agents::AgentKind::Claude => "Claude",
        bootty_agents::AgentKind::Pi => "Pi",
    };
    let agent = matches!(mode, LaunchMode::Agent);
    let command = !agent && !draft.trim().is_empty();
    let label = if agent {
        format!("Start {provider_name}")
    } else if command {
        "Start command".to_owned()
    } else {
        "Open terminal".to_owned()
    };
    let mut spec = DialogSpec::prompt(
        NEW_SESSION_ID,
        "New session",
        draft,
        if agent {
            "What would you like the agent to do?"
        } else {
            "Enter a command, or leave empty to open a shell…"
        },
        DialogAction::new("start-session"),
    );
    spec.role = DialogRole::SessionLaunch;
    spec.text_label = Some(
        if agent {
            "Prompt"
        } else {
            "Command (optional)"
        }
        .to_owned(),
    );
    spec.icon = Some("terminal".to_owned());
    spec.footer = Some(display_project_path(cwd, remote));
    spec.hint = Some(
        if agent {
            "The agent starts in the selected checkout."
        } else if command {
            "Run this command in the selected checkout."
        } else {
            "Open a shell in the selected checkout."
        }
        .to_owned(),
    );
    if let Some(submit) = spec.rows.first_mut() {
        submit.label = label;
    }
    spec.fields = vec![DialogField {
        id: "mode".to_owned(),
        label: "Session".to_owned(),
        value: if agent { "Agent" } else { "Terminal" }.to_owned(),
        placeholder: String::new(),
        kind: DialogFieldKind::Choice(if remote {
            vec!["Terminal".to_owned()]
        } else {
            vec!["Agent".to_owned(), "Terminal".to_owned()]
        }),
    }];
    if agent {
        spec.fields.push(DialogField {
            id: "provider".to_owned(),
            label: "Provider".to_owned(),
            value: provider_name.to_owned(),
            placeholder: String::new(),
            kind: DialogFieldKind::Choice(vec![
                "Codex".to_owned(),
                "Claude".to_owned(),
                "Pi".to_owned(),
            ]),
        });
    }
    spec.rows.push(picker_row(
        RowId::new("choose-project"),
        "folder",
        project_name(cwd),
        !catalog_loading,
    ));
    spec.rows.push(picker_row(
        RowId::new("choose-checkout"),
        "git-branch",
        "Choose checkout or new worktree…".to_owned(),
        !catalog_loading,
    ));
    spec
}

fn generated_task_branch(draft: &str) -> String {
    let words = draft
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    let slug = words
        .get(..words.len().min(48))
        .unwrap_or(&words)
        .trim_end_matches('-');
    format!("task/{}", if slug.is_empty() { "new-task" } else { slug })
}

fn worktree_row_id(entry: &WorktreePickerEntry) -> RowId {
    RowId::new(if entry.is_new {
        "new-worktree".to_owned()
    } else {
        format!("worktree:{}", entry.path.as_deref().unwrap_or(&entry.label))
    })
}
