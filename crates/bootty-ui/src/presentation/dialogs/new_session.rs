//! Directory and project states for the new-session workflow.

use super::NEW_SESSION_ID;
use crate::gpui::{
    DialogAction, DialogIntent, DialogProgressState, DialogProgressStep, DialogRow, DialogSpec,
    RowId,
};
use crate::new_session::{NewSessionEffect, NewSessionOutcome, NewSessionWorker};
use crate::presentation::new_session_form::NewSessionForm;
use crate::product_dialogs::searchable::{SearchableEntry, SearchableIntent, SearchableList};
use crate::strings::home_dir;
use bootty_config::config::RemoteConfig;
use bootty_git::{
    self as project, ProjectPickerEntry, discover_project_picker_entries,
    toggle_favorite_project_path,
};
use bootty_mux::RepaintHandle;
use std::path::Path;

pub struct NewSessionDialog {
    step: NewSessionStep,
    worker: Option<NewSessionWorker>,
    projects: Vec<ProjectPickerEntry>,
    purpose: NewSessionPurpose,
    form: Option<NewSessionForm>,
    launch: Option<std::sync::mpsc::Receiver<bootty_control::CommandOutcome>>,
    starting: bool,
    progress: Vec<DialogProgressStep>,
    failure: Option<String>,
    registration_target: Option<bootty_control::CommandTarget>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewSessionPurpose {
    CreateSession,
    AddProject,
}

enum NewSessionStep {
    Form,
    Project(ProjectPicker),
}

struct ProjectPicker {
    projects: Vec<ProjectPickerEntry>,
    list: SearchableList<ProjectPickerEntry>,
}

impl NewSessionDialog {
    #[must_use]
    pub fn open() -> Self {
        Self::from_projects(discover_project_picker_entries(
            project::home_dir().as_deref(),
        ))
    }

    pub fn open_local(repaint: RepaintHandle) -> Self {
        Self::new(
            Vec::new(),
            Some(NewSessionWorker::local(repaint)),
            NewSessionPurpose::CreateSession,
        )
    }

    /// Build a local picker from an already-owned project catalog.
    #[must_use]
    pub fn from_projects(projects: Vec<ProjectPickerEntry>) -> Self {
        Self::new(projects, None, NewSessionPurpose::CreateSession)
    }

    /// Build an add-project picker from an already-owned project catalog.
    #[must_use]
    pub fn from_projects_for_add(projects: Vec<ProjectPickerEntry>) -> Self {
        Self::new(projects, None, NewSessionPurpose::AddProject)
    }

    pub fn open_add_project_local(repaint: RepaintHandle) -> Self {
        Self::new(
            Vec::new(),
            Some(NewSessionWorker::local(repaint)),
            NewSessionPurpose::AddProject,
        )
    }

    pub fn open_remote(remote: RemoteConfig, repaint: RepaintHandle) -> Self {
        Self::new(
            Vec::new(),
            Some(NewSessionWorker::remote(remote, repaint)),
            NewSessionPurpose::CreateSession,
        )
    }

    pub fn open_add_project_remote(remote: RemoteConfig, repaint: RepaintHandle) -> Self {
        Self::new(
            Vec::new(),
            Some(NewSessionWorker::remote(remote, repaint)),
            NewSessionPurpose::AddProject,
        )
    }

    fn new(
        projects: Vec<ProjectPickerEntry>,
        worker: Option<NewSessionWorker>,
        purpose: NewSessionPurpose,
    ) -> Self {
        let remote = worker.as_ref().is_some_and(NewSessionWorker::is_remote);
        Self {
            step: NewSessionStep::Project(ProjectPicker {
                list: SearchableList::new(project_entries(&projects, remote)),
                projects,
            }),
            worker,
            projects: Vec::new(),
            purpose,
            form: None,
            launch: None,
            starting: false,
            progress: Vec::new(),
            failure: None,
            registration_target: None,
        }
    }

    pub fn open_form(form: NewSessionForm, repaint: &RepaintHandle) -> Self {
        let worker = form
            .destination()
            .and_then(|destination| destination.remote.clone())
            .map_or_else(
                || NewSessionWorker::local(repaint.clone()),
                |remote| NewSessionWorker::remote(remote, repaint.clone()),
            );
        Self::form_with_worker(form, worker)
    }

    pub fn open_registered_form(
        form: NewSessionForm,
        repaint: &RepaintHandle,
        repository: bootty_mux::repository::WorkspaceRepository,
    ) -> Self {
        let remote = form
            .destination()
            .and_then(|destination| destination.remote.clone());
        let worker =
            NewSessionWorker::registered(remote, repository, form.draft.scope, repaint.clone());
        Self::form_with_worker(form, worker)
    }

    const fn form_with_worker(form: NewSessionForm, worker: NewSessionWorker) -> Self {
        Self {
            step: NewSessionStep::Form,
            worker: Some(worker),
            projects: Vec::new(),
            purpose: NewSessionPurpose::CreateSession,
            form: Some(form),
            launch: None,
            starting: false,
            progress: Vec::new(),
            failure: None,
            registration_target: None,
        }
    }

    pub(crate) fn with_registration_target(
        mut self,
        target: Option<bootty_control::CommandTarget>,
    ) -> Self {
        self.registration_target = target;
        self
    }

    pub(crate) fn registration_target(&self) -> Option<bootty_control::CommandTarget> {
        self.registration_target.clone()
    }

    pub(crate) const fn is_creation_form(&self) -> bool {
        matches!(self.step, NewSessionStep::Form)
    }

    pub(crate) const fn is_in_flight(&self) -> bool {
        self.starting || self.launch.is_some()
    }

    #[must_use]
    pub fn draft(&self) -> Option<&crate::presentation::new_session_form::NewSessionDraft> {
        self.form.as_ref().map(|form| &form.draft)
    }

    pub fn started(&mut self, launch: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>) {
        let detail = self.form.as_ref().map(NewSessionForm::display_cwd);
        self.start_stage("session", "Start session".to_owned(), detail);
        self.starting = false;
        self.launch = Some(launch);
    }

    pub fn failed(&mut self, error: String) {
        self.starting = false;
        if let Some(step) = self
            .progress
            .iter_mut()
            .find(|step| step.state == DialogProgressState::Running)
        {
            step.state = DialogProgressState::Failed;
        } else {
            self.progress.push(DialogProgressStep {
                id: "preparation".into(),
                label: "Prepare session".into(),
                detail: self.form.as_ref().map(NewSessionForm::display_cwd),
                state: DialogProgressState::Failed,
            });
        }
        self.failure = Some(error);
    }

    fn start_stage(&mut self, id: &str, label: String, detail: Option<String>) {
        let step = DialogProgressStep {
            id: id.to_owned(),
            label,
            detail,
            state: DialogProgressState::Running,
        };
        if let Some(pending) = self.progress.iter_mut().find(|step| step.id == id) {
            *pending = step;
        } else {
            self.progress.push(step);
        }
    }

    fn complete_stage(&mut self, id: &str) {
        if let Some(step) = self
            .progress
            .iter_mut()
            .rev()
            .find(|step| step.id == id && step.state == DialogProgressState::Running)
        {
            step.state = DialogProgressState::Complete;
        }
    }

    pub(crate) fn sync_registered_projects(&mut self, projects: Vec<ProjectPickerEntry>) {
        if self.purpose == NewSessionPurpose::CreateSession && self.form.is_some() {
            self.projects = projects;
        }
    }

    pub fn poll(&mut self) -> Option<NewSessionPickerEvent> {
        if self.launch.is_some() {
            return self.poll_launch();
        }
        let result = self.worker.as_mut()?.poll()?;
        let remote = self.is_remote();
        match result {
            Ok(NewSessionOutcome::HomeDirectory(path)) => {
                if let Some(form) = &mut self.form {
                    form.set_no_project(path);
                }
            }
            Ok(NewSessionOutcome::Projects(projects)) => {
                self.projects.clone_from(&projects);
                if let NewSessionStep::Project(picker) = &mut self.step {
                    picker.projects = projects;
                    picker.replace_entries(remote);
                    if self.purpose == NewSessionPurpose::AddProject {
                        let filter = picker.list.filter().to_owned();
                        picker.inject_direct_project(&filter, remote);
                    }
                }
            }
            Ok(NewSessionOutcome::Favorite { path, favorite }) => {
                if let NewSessionStep::Project(picker) = &mut self.step {
                    picker.set_favorite(&path, favorite, remote);
                }
            }
            Ok(NewSessionOutcome::ProjectAdded(path)) => {
                return Some(NewSessionPickerEvent::ProjectAdded { path });
            }
            Err(error) => {
                self.starting = false;
                match (&self.step, &mut self.form) {
                    (NewSessionStep::Form, Some(form)) => form.error = Some(error),
                    _ => return Some(NewSessionPickerEvent::Error(error)),
                }
            }
        }
        None
    }

    fn poll_launch(&mut self) -> Option<NewSessionPickerEvent> {
        let outcome = match self.launch.as_ref()?.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bootty_control::CommandOutcome::Failed {
                    code: "shutdown".to_owned(),
                    message: "Session command host stopped".to_owned(),
                }
            }
        };
        self.launch = None;
        if let bootty_control::CommandOutcome::Success { value, warnings } = outcome {
            self.complete_stage("session");
            return Some(NewSessionPickerEvent::Started { value, warnings });
        }
        self.failed(
            crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "The session command stopped without a result".to_owned()),
        );
        if let Some(form) = &mut self.form
            && self.purpose == NewSessionPurpose::CreateSession
        {
            form.draft.identity = bootty_mux::snapshot::new_session_identity();
        }
        None
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let busy = self.worker_busy();
        let remote = self.is_remote();
        let NewSessionStep::Project(picker) = &self.step else {
            return self.form_spec(busy);
        };
        let add = self.purpose == NewSessionPurpose::AddProject;
        let registered = self.form.is_some();
        let mut spec = DialogSpec::searchable(
            NEW_SESSION_ID,
            if add {
                "Add Project"
            } else if registered {
                "Projects"
            } else {
                "Directory"
            },
            picker.list.filter(),
            picker.rows(remote, !busy, registered),
        );
        spec.icon = Some("folder".to_owned());
        spec.hint = Some(
            if add {
                "Enter add   Esc close"
            } else if registered {
                "Enter select   Esc back"
            } else {
                "Enter open   Ctrl+Shift+F favorite   Esc close"
            }
            .to_owned(),
        );
        match (busy, remote) {
            (false, _) if registered => "No registered projects",
            (false, _) => "no matching directories",
            (true, true) => "loading remote projects…",
            (true, false) => "loading directories…",
        }
        .clone_into(&mut spec.empty_text);
        spec.text_hint = Some(
            if registered {
                "Find project…"
            } else {
                "filter directories…"
            }
            .to_owned(),
        );
        spec
    }

    fn form_spec(&self, busy: bool) -> DialogSpec {
        let Some(form) = self.form.as_ref() else {
            return DialogSpec::searchable(NEW_SESSION_ID, "Session unavailable", "", Vec::new());
        };
        let mut spec = form.spec(self.is_in_flight());
        spec.progress.clone_from(&self.progress);
        spec.projects.clone_from(&self.projects);
        if let Some(error) = &self.failure {
            if let Some(submit) = spec.rows.first_mut() {
                submit.detail = Some(error.clone());
            }
            spec.hint = Some("Resolve the reported issue, then try again.".to_owned());
        }
        if busy {
            for row in &mut spec.rows {
                row.enabled = false;
            }
            if !self.starting
                && let Some(row) = spec.rows.first_mut()
            {
                "Checking project…".clone_into(&mut row.label);
            }
        }
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<NewSessionPickerEvent> {
        if intent.dialog_id().0 != NEW_SESSION_ID {
            return None;
        }
        // Accepted Start owns its destination until the observed result. Keep the form open.
        if self.is_in_flight() {
            return None;
        }
        if matches!(self.step, NewSessionStep::Form) {
            return self.apply_form(intent);
        }
        match intent {
            DialogIntent::Dismiss { .. } if self.form.is_some() => {
                self.step = NewSessionStep::Form;
                None
            }
            DialogIntent::Dismiss { .. } => Some(NewSessionPickerEvent::Close),
            DialogIntent::TextChanged { value, .. } => {
                self.change_text(value);
                None
            }
            DialogIntent::SelectionChanged { row, .. } => {
                self.select_row(row);
                None
            }
            DialogIntent::ToggleFavorite { row, .. }
                if self.purpose != NewSessionPurpose::AddProject
                    && self.form.is_none()
                    && !self.worker_busy() =>
            {
                self.select_row(row)?;
                self.toggle_project_favorite()
            }
            DialogIntent::Activate { row, .. } if !self.worker_busy() => {
                self.select_row(row)?;
                self.activate_selected()
            }
            _ => None,
        }
    }

    fn change_text(&mut self, value: &str) {
        let remote = self.is_remote();
        if let NewSessionStep::Project(picker) = &mut self.step {
            picker
                .list
                .apply(SearchableIntent::SetFilter(value.to_owned()));
            if self.form.is_none() {
                picker.inject_direct_project(value, remote);
            }
        }
    }

    fn select_row(&mut self, row: &RowId) -> Option<()> {
        let remote = self.is_remote();
        let NewSessionStep::Project(picker) = &mut self.step else {
            return Some(());
        };
        let index = picker.index_for_path(row.0.strip_prefix("project:")?, remote)?;
        picker.list.apply(SearchableIntent::Select(index));
        Some(())
    }

    fn activate_selected(&mut self) -> Option<NewSessionPickerEvent> {
        let NewSessionStep::Project(picker) = &self.step else {
            return None;
        };
        let project = picker.list.selected_value()?.clone();
        if self.purpose == NewSessionPurpose::AddProject {
            Some(NewSessionPickerEvent::AddProject { path: project.path })
        } else {
            self.activate_project(project)
        }
    }

    fn change_form_field(&mut self, field: &str, value: &str) -> Option<NewSessionPickerEvent> {
        if field == "project" {
            return self.change_project(value);
        }
        let form = self.form.as_mut()?;
        if form.change_field(field, value) {
            self.failure = None;
            self.progress.clear();
            self.projects.clear();
            self.worker.as_mut()?.retarget(
                form.destination()
                    .and_then(|destination| destination.remote.clone()),
                form.draft.scope,
            );
        }
        None
    }

    fn apply_form(&mut self, intent: &DialogIntent) -> Option<NewSessionPickerEvent> {
        match intent {
            DialogIntent::Dismiss { .. } => Some(NewSessionPickerEvent::Close),
            DialogIntent::FieldChanged { field, value, .. } => self.change_form_field(field, value),
            DialogIntent::Activate { action, .. }
                if !self.worker_busy() && action.0 == "start-session" =>
            {
                self.start_form()
            }
            _ => None,
        }
    }

    fn change_project(&mut self, value: &str) -> Option<NewSessionPickerEvent> {
        if self.worker_busy() {
            return None;
        }
        if value.is_empty() {
            if let Some(worker) = &mut self.worker {
                worker.start(NewSessionEffect::ResolveHome);
            } else if let Some(form) = &mut self.form {
                form.set_no_project(home_dir()?.to_str()?.to_owned());
            }
            return None;
        }
        let project = self
            .projects
            .iter()
            .find(|project| project.path == value)?
            .clone();
        self.activate_project(project)
    }

    fn start_form(&mut self) -> Option<NewSessionPickerEvent> {
        self.reset_failed_attempt();
        let form = self.form.as_mut()?;
        match form.invocation(&form.draft.cwd) {
            Ok(invocation) => {
                self.starting = true;
                Some(NewSessionPickerEvent::Submit(invocation))
            }
            Err(error) => {
                form.error = Some(error);
                None
            }
        }
    }

    fn reset_failed_attempt(&mut self) {
        if self
            .progress
            .iter()
            .any(|step| step.state == DialogProgressState::Failed)
        {
            self.progress
                .retain(|step| step.state == DialogProgressState::Complete);
        }
        self.failure = None;
        if let Some(form) = &mut self.form {
            form.error = None;
        }
    }

    fn activate_project(&mut self, project: ProjectPickerEntry) -> Option<NewSessionPickerEvent> {
        let Some(form) = &mut self.form else {
            return Some(NewSessionPickerEvent::CreateSession { cwd: project.path });
        };
        form.set_directory(project.path);
        self.progress.clear();
        self.failure = None;
        self.step = NewSessionStep::Form;
        None
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

    pub(crate) fn start_add_project(&mut self, path: String) -> Result<(), String> {
        if self.purpose != NewSessionPurpose::AddProject {
            return Err("the active picker does not add projects".to_owned());
        }
        let Some(worker) = &mut self.worker else {
            return Err("project discovery is unavailable".to_owned());
        };
        worker.start(NewSessionEffect::AddFavorite(path));
        Ok(())
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
        // Remote paths are admitted by their host when selected, never by local metadata.
        let project = if remote {
            let path = filter.trim();
            looks_like_directory_path(path).then(|| ProjectPickerEntry {
                path: path.to_owned(),
                favorite: false,
            })
        } else {
            direct_project_entry(filter)
        };
        let Some(project) = project else {
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
    fn rows(&self, remote: bool, enabled: bool, registered: bool) -> Vec<DialogRow> {
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
                if registered {
                    project
                        .path
                        .trim_end_matches(['/', '\\'])
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(&project.path)
                        .to_owned()
                } else {
                    display_project_path(&project.path, remote)
                },
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
            rows.push(DialogRow::section(
                "directories",
                if registered {
                    "Projects"
                } else {
                    "Directories"
                },
            ));
            rows.extend(directories);
        }
        rows
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewSessionPickerEvent {
    Close,
    Submit(bootty_control::CommandInvocation),
    Started {
        value: serde_json::Value,
        warnings: Vec<bootty_control::CommandWarning>,
    },
    Error(String),
    AddProject {
        path: String,
    },
    ProjectAdded {
        path: String,
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
            let mut entry =
                SearchableEntry::new(project.clone(), display_project_path(&project.path, remote));
            // Display paths may abbreviate home; absolute path input must still find the project.
            entry.keywords.push(project.path.clone());
            if cfg!(windows) && !remote {
                // Windows accepts either separator, including when home is abbreviated.
                entry.keywords.push(project.path.replace('\\', "/"));
                entry
                    .keywords
                    .push(display_project_path(&project.path, false).replace('\\', "/"));
            }
            entry
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
