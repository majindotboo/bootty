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
    projects: Vec<ProjectPickerEntry>,
    purpose: NewSessionPurpose,
    form: Option<crate::presentation::new_session_form::NewSessionForm>,
    launch: Option<PendingCreation>,
    catalog_request: Option<bootty_control::CommandInvocation>,
    catalog_reply: Option<std::sync::mpsc::Receiver<bootty_control::CommandOutcome>>,
    naming: Option<PendingNames>,
    starting: bool,
    launch_direction: LaunchDirection,
    empty_enter_armed: bool,
    shell_fallback: bool,
    surface_request_id: Option<u64>,
    registration_target: Option<bootty_control::CommandTarget>,
}

struct PendingNames {
    action: String,
    reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
    cancellation: bootty_control::CommandCancellation,
}

struct PendingCreation {
    kind: CreationCommand,
    reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CreationCommand {
    Session,
    Worktree,
}

impl Drop for PendingNames {
    fn drop(&mut self) {
        _ = self.cancellation.cancel();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchDirection {
    Foreground,
    Background,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewSessionPurpose {
    CreateSession,
    NativeAgentTab,
    AddProject,
}

enum NewSessionStep {
    Form,
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
            catalog_request: None,
            catalog_reply: None,
            naming: None,
            starting: false,
            launch_direction: LaunchDirection::Foreground,
            empty_enter_armed: false,
            shell_fallback: false,
            surface_request_id: None,
            registration_target: None,
        }
    }

    pub fn open_form(
        form: crate::presentation::new_session_form::NewSessionForm,
        repaint: &RepaintHandle,
    ) -> Self {
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
        form: crate::presentation::new_session_form::NewSessionForm,
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

    const fn form_with_worker(
        form: crate::presentation::new_session_form::NewSessionForm,
        worker: NewSessionWorker,
    ) -> Self {
        Self {
            step: NewSessionStep::Form,
            worker: Some(worker),
            projects: Vec::new(),
            purpose: NewSessionPurpose::CreateSession,
            form: Some(form),
            launch: None,
            catalog_request: None,
            catalog_reply: None,
            naming: None,
            starting: false,
            launch_direction: LaunchDirection::Foreground,
            empty_enter_armed: false,
            shell_fallback: false,
            surface_request_id: None,
            registration_target: None,
        }
    }

    pub fn open_native_form(
        mut form: crate::presentation::new_session_form::NewSessionForm,
        repaint: &RepaintHandle,
    ) -> Self {
        form.draft.mode = crate::presentation::new_session_form::NewSessionMode::Agent;
        form.draft.isolated = false;
        let mut dialog = Self::open_form(form, repaint);
        dialog.purpose = NewSessionPurpose::NativeAgentTab;
        dialog
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

    pub(crate) const fn with_surface_request(mut self, request_id: u64) -> Self {
        self.surface_request_id = Some(request_id);
        self
    }

    pub(crate) const fn surface_request_id(&self) -> Option<u64> {
        self.surface_request_id
    }

    pub(crate) fn retains_creation_draft(&self) -> bool {
        self.purpose != NewSessionPurpose::NativeAgentTab
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
        self.starting = false;
        self.launch = Some(PendingCreation {
            kind: CreationCommand::Session,
            reply: launch,
        });
    }

    pub fn worktree_started(
        &mut self,
        reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
    ) {
        self.starting = false;
        self.launch = Some(PendingCreation {
            kind: CreationCommand::Worktree,
            reply,
        });
    }

    /// Leave creation as soon as its persisted conversation owns a real mux pane.
    pub(crate) fn native_placed(
        &mut self,
        record: &bootty_agents::NativeSessionRecord,
        terminal: &bootty_control::CommandTarget,
    ) -> Option<(
        NewSessionPickerEvent,
        std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
    )> {
        let draft = self.draft()?;
        if matches!(
            record.snapshot.status,
            bootty_agents::NativeSessionStatus::Error | bootty_agents::NativeSessionStatus::Stopped
        ) || self
            .launch
            .as_ref()
            .is_none_or(|launch| launch.kind != CreationCommand::Session)
            || record.task_identity.as_deref() != Some(draft.identity.as_str())
            || record.binding_id != draft.scope.persistence_value().to_string()
        {
            return None;
        }
        let reply = self.launch.take()?.reply;
        self.starting = false;
        Some((
            NewSessionPickerEvent::Started {
                value: serde_json::json!({"native": record, "terminal": terminal}),
                warnings: Vec::new(),
                foreground: self.launch_direction == LaunchDirection::Foreground,
            },
            reply,
        ))
    }

    pub(crate) fn background_naming(
        &self,
    ) -> Option<(
        bootty_control::CommandInvocation,
        bootty_mux::controller::SpaceId,
        String,
        String,
    )> {
        let form = self.form.as_ref()?;
        if self.shell_fallback || form.draft.isolated {
            return None;
        }
        Some((
            form.naming_invocation().ok()??,
            form.draft.scope,
            form.draft.identity.clone(),
            form.title(),
        ))
    }

    pub fn catalog_started(
        &mut self,
        reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
    ) {
        self.catalog_reply = Some(reply);
    }

    pub fn naming_started(
        &mut self,
        action: String,
        reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
        cancellation: bootty_control::CommandCancellation,
    ) {
        self.naming = Some(PendingNames {
            action,
            reply,
            cancellation,
        });
    }

    fn poll_names(&mut self) -> Option<NewSessionPickerEvent> {
        let reply = match self.naming.as_ref()?.reply.try_recv() {
            Ok(reply) => reply,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.naming = None;
                self.failed("Session naming stopped".into());
                return None;
            }
        };
        let action = self.naming.take()?.action.clone();
        self.starting = false;
        let names = match reply {
            bootty_control::CommandOutcome::Success { value, .. } => {
                serde_json::from_value(value).map_err(|_| "Invalid generated names".to_owned())
            }
            outcome => Err(crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "Session naming failed".to_owned())),
        };
        let result = names.and_then(|names| {
            self.form
                .as_mut()
                .ok_or("Session form closed")?
                .set_generated_names(names)
        });
        if let Err(error) = result {
            self.failed(error);
            return None;
        }
        self.start_form(&action)
    }

    fn poll_catalog(&mut self) -> Option<NewSessionPickerEvent> {
        if !matches!(self.step, NewSessionStep::Form) || self.is_in_flight() {
            return None;
        }
        let form = self.form.as_mut()?;
        let request = form.model_catalog_invocation();
        if request != self.catalog_request {
            self.catalog_request.clone_from(&request);
            self.catalog_reply = None;
            form.model_options.clear();
            form.model_error = None;
            form.models_loading = request.is_some();
            return request.map(NewSessionPickerEvent::Catalog);
        }
        let reply = match self.catalog_reply.as_ref()?.try_recv() {
            Ok(reply) => reply,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.catalog_reply = None;
                form.set_model_catalog(Err("Provider catalog owner stopped".to_owned()));
                return None;
            }
        };
        self.catalog_reply = None;
        let models = match reply {
            bootty_control::CommandOutcome::Success { value, .. } => {
                serde_json::from_value(value).map_err(|error| error.to_string())
            }
            outcome => Err(crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "Provider catalog unavailable".to_owned())),
        };
        form.set_model_catalog(models);
        None
    }

    pub fn failed(&mut self, error: String) {
        self.starting = false;
        if let NewSessionStep::BranchName(draft) = &mut self.step {
            draft.error = Some(error);
        } else if let Some(form) = &mut self.form {
            form.error = Some(error);
        }
    }

    pub(crate) fn sync_registered_projects(&mut self, projects: Vec<ProjectPickerEntry>) {
        if self.purpose == NewSessionPurpose::CreateSession && self.form.is_some() {
            self.projects = projects;
        }
    }

    pub fn poll(&mut self) -> Option<NewSessionPickerEvent> {
        if self.naming.is_some() {
            return self.poll_names();
        }
        if let Some(event) = self.poll_catalog() {
            return Some(event);
        }
        if self.launch.is_some() {
            return self.poll_launch();
        }
        let result = self.worker.as_mut()?.poll()?;
        let remote = self.is_remote();
        match result {
            Ok(NewSessionOutcome::Projects(projects)) => {
                self.projects.clone_from(&projects);
                if let Some(form) = &self.form
                    && matches!(self.step, NewSessionStep::Form)
                    && form
                        .destination()
                        .is_some_and(|destination| destination.worktrees)
                    && !form.draft.cwd.is_empty()
                    && let Some(worker) = &mut self.worker
                {
                    worker.start(NewSessionEffect::ListWorktrees(
                        form.draft.cwd.clone(),
                        Vec::new(),
                    ));
                }
                if let NewSessionStep::Project(picker) = &mut self.step {
                    picker.projects = projects;
                    picker.replace_entries(remote);
                    if self.purpose == NewSessionPurpose::AddProject {
                        let filter = picker.list.filter().to_owned();
                        picker.inject_direct_project(&filter, remote);
                    }
                }
            }
            Ok(NewSessionOutcome::Worktrees(worktrees)) => {
                if let Some(form) = &mut self.form {
                    form.set_worktree_catalog(&worktrees);
                    return None;
                }
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
            Ok(NewSessionOutcome::ProjectAdded(path)) => {
                return Some(NewSessionPickerEvent::ProjectAdded { path });
            }
            Err(error) => {
                self.starting = false;
                match &mut self.step {
                    NewSessionStep::BranchName(draft) => draft.error = Some(error),
                    NewSessionStep::Form => {
                        if let Some(form) = &mut self.form {
                            form.error = Some(error);
                        }
                    }
                    _ => return Some(NewSessionPickerEvent::Error(error)),
                }
            }
        }
        None
    }

    fn poll_launch(&mut self) -> Option<NewSessionPickerEvent> {
        let outcome = match self.launch.as_ref()?.reply.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bootty_control::CommandOutcome::Failed {
                    code: "shutdown".to_owned(),
                    message: "Session command host stopped".to_owned(),
                }
            }
        };
        let kind = self.launch.take()?.kind;
        if kind == CreationCommand::Worktree {
            return match outcome {
                bootty_control::CommandOutcome::Success { value, .. } => {
                    if let Some(cwd) = value.as_str() {
                        self.created_worktree(cwd.to_owned())
                    } else {
                        self.failed("The worktree command returned no checkout".into());
                        None
                    }
                }
                outcome => {
                    self.failed(
                        crate::commands::command_outcome_message(&outcome)
                            .unwrap_or_else(|| "Worktree creation failed".into()),
                    );
                    None
                }
            };
        }
        if let bootty_control::CommandOutcome::Success { value, warnings } = outcome {
            return Some(NewSessionPickerEvent::Started {
                value,
                warnings,
                foreground: self.launch_direction == LaunchDirection::Foreground,
            });
        }
        if let Some(form) = &mut self.form {
            form.error = crate::commands::command_outcome_message(&outcome);
            if self.purpose == NewSessionPurpose::CreateSession {
                form.draft.identity = bootty_mux::snapshot::new_session_identity();
                form.draft.suffix.clone_from(&form.draft.identity);
            }
        }
        None
    }

    fn created_worktree(&mut self, cwd: String) -> Option<NewSessionPickerEvent> {
        let Some(form) = &mut self.form else {
            return Some(NewSessionPickerEvent::CreateSession { cwd });
        };
        // Retain the successful checkout on later launch failure. Retry uses it.
        form.set_directory(cwd.clone());
        form.draft.isolated = false;
        let invocation = if self.shell_fallback {
            form.shell_invocation(&cwd)
        } else {
            form.invocation(&cwd)
        };
        match invocation {
            Ok(invocation) => Some(NewSessionPickerEvent::Submit(invocation)),
            Err(error) => {
                form.error = Some(error);
                None
            }
        }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let busy = self.worker_busy();
        let remote = self.is_remote();
        let (rows, title, icon, hint, empty, text_hint, filter) = match &self.step {
            NewSessionStep::Form => return self.form_spec(busy),
            NewSessionStep::BranchName(draft) => {
                return draft.spec(remote, busy || self.is_in_flight());
            }
            NewSessionStep::Project(picker) => (
                picker.rows(remote, !busy, self.form.is_some()),
                if self.purpose == NewSessionPurpose::AddProject {
                    "Add Project"
                } else if self.form.is_some() {
                    "Projects"
                } else {
                    "Directory"
                },
                "folder",
                if self.purpose == NewSessionPurpose::AddProject {
                    "Enter add   Esc close"
                } else if self.form.is_some() {
                    "Enter select   Esc back"
                } else {
                    "Enter open   Ctrl+Shift+F favorite   Esc close"
                },
                match (busy, remote) {
                    (false, _) if self.form.is_some() => "No registered projects",
                    (false, _) => "no matching directories",
                    (true, true) => "loading remote projects…",
                    (true, false) => "loading directories…",
                },
                if self.form.is_some() {
                    "Find project…"
                } else {
                    "filter directories…"
                },
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

    fn form_spec(&self, busy: bool) -> DialogSpec {
        let Some(form) = self.form.as_ref() else {
            return DialogSpec::searchable(NEW_SESSION_ID, "Session unavailable", "", Vec::new());
        };
        let mut spec = form.spec(self.is_in_flight());
        spec.projects.clone_from(&self.projects);
        spec.selected_project = Some(form.draft.cwd.clone());
        if self.purpose == NewSessionPurpose::NativeAgentTab {
            "Agent".clone_into(&mut spec.title);
            spec.fields.retain(|field| {
                matches!(
                    field.id.as_str(),
                    "provider" | "profile" | "model" | "reasoning" | "permissions"
                )
            });
            spec.rows.retain(|row| row.id.0 != "choose-project");
            let project = form
                .draft
                .cwd
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or("project");
            spec.footer = Some(project.to_owned());
        }
        if self.empty_enter_armed {
            spec.hint = Some("Press Enter again to open a terminal".to_owned());
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

    pub fn apply(
        &mut self,
        intent: &DialogIntent,
        open_cwds: &[String],
    ) -> Option<NewSessionPickerEvent> {
        if intent.dialog_id().0 != NEW_SESSION_ID {
            return None;
        }
        // Accepted Start owns its destination until the observed result. Keep the form open.
        if self.launch.is_some() || self.starting {
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
                if self.purpose != NewSessionPurpose::AddProject
                    && self.form.is_none()
                    && matches!(self.step, NewSessionStep::Project(_))
                    && !self.worker_busy() =>
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
            NewSessionStep::Form => {}
            NewSessionStep::Project(picker) => {
                picker
                    .list
                    .apply(SearchableIntent::SetFilter(value.to_owned()));
                if self.form.is_none() {
                    picker.inject_direct_project(value, remote);
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
            NewSessionStep::Form | NewSessionStep::BranchName(_) => {}
        }
        Some(())
    }

    fn activate_selected(&mut self, open_cwds: &[String]) -> Option<NewSessionPickerEvent> {
        match &self.step {
            NewSessionStep::Form => None,
            NewSessionStep::Project(picker) => {
                let project = picker.list.selected_value()?.clone();
                if self.purpose == NewSessionPurpose::AddProject {
                    Some(NewSessionPickerEvent::AddProject { path: project.path })
                } else {
                    self.activate_project(project, open_cwds)
                }
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

    fn apply_form(&mut self, intent: &DialogIntent) -> Option<NewSessionPickerEvent> {
        match intent {
            DialogIntent::Dismiss { .. } => Some(NewSessionPickerEvent::Close),
            DialogIntent::TextChanged { value, .. } if !self.is_in_flight() => {
                let form = self.form.as_mut()?;
                let previous = match form.draft.mode {
                    crate::presentation::new_session_form::NewSessionMode::Agent => {
                        &form.draft.prompt
                    }
                    crate::presentation::new_session_form::NewSessionMode::Terminal => {
                        &form.draft.command
                    }
                };
                if previous != value {
                    self.empty_enter_armed = false;
                    form.change_text(value);
                }
                None
            }
            DialogIntent::FieldChanged { field, value, .. } if !self.is_in_flight() => {
                if field == "project" {
                    if self.purpose == NewSessionPurpose::NativeAgentTab || self.worker_busy() {
                        return None;
                    }
                    let project = self
                        .projects
                        .iter()
                        .find(|project| project.path == *value)?
                        .clone();
                    self.empty_enter_armed = false;
                    return self.activate_project(project, &[]);
                }
                if field == "model-favorite" {
                    let form = self.form.as_ref()?;
                    if self.catalog_reply.is_some()
                        || !form.model_options.iter().any(|model| model.id == *value)
                    {
                        return None;
                    }
                    let mut invocation = form.model_catalog_invocation()?;
                    "agents.native.catalog-favorite".clone_into(&mut invocation.command);
                    invocation.arguments.push(value.clone());
                    return Some(NewSessionPickerEvent::Catalog(invocation));
                }
                if self.purpose == NewSessionPurpose::NativeAgentTab
                    && !matches!(
                        field.as_str(),
                        "provider" | "profile" | "model" | "reasoning" | "permissions"
                    )
                {
                    return None;
                }
                self.empty_enter_armed = false;
                let form = self.form.as_mut()?;
                if form.change_field(field, value) {
                    self.projects.clear();
                    self.worker.as_mut()?.retarget(
                        form.destination()
                            .and_then(|destination| destination.remote.clone()),
                        form.draft.scope,
                    );
                }
                None
            }
            DialogIntent::ApplicationsChanged { applications, .. } if !self.is_in_flight() => {
                let form = self.form.as_mut()?;
                if applications != &form.draft.applications {
                    form.draft.applications.clone_from(applications);
                    self.empty_enter_armed = false;
                }
                None
            }
            DialogIntent::AttachmentsChanged { attachments, .. } if !self.is_in_flight() => {
                let form = self.form.as_mut()?;
                if attachments == &form.draft.attachments {
                    return None;
                }
                self.empty_enter_armed = false;
                if attachments.len() > 16 {
                    form.error = Some("Attach at most 16 files".into());
                    return None;
                }
                form.draft.attachments.clone_from(attachments);
                form.error = None;
                None
            }
            DialogIntent::Activate { action, .. }
                if !self.worker_busy() && !self.is_in_flight() =>
            {
                if action.0 == "reload-models" {
                    self.catalog_request = None;
                    return None;
                }
                self.start_form(&action.0)
            }
            _ => None,
        }
    }

    fn start_form(&mut self, action: &str) -> Option<NewSessionPickerEvent> {
        if !matches!(
            action,
            "start-session" | "start-session-background" | "enter-session"
        ) {
            return None;
        }
        let form = self.form.as_mut()?;
        let empty = match form.draft.mode {
            crate::presentation::new_session_form::NewSessionMode::Agent => &form.draft.prompt,
            crate::presentation::new_session_form::NewSessionMode::Terminal => &form.draft.command,
        }
        .trim()
        .is_empty()
            && (form.draft.mode == crate::presentation::new_session_form::NewSessionMode::Terminal
                || (form.draft.attachments.is_empty() && form.draft.applications.is_empty()));
        let shell_fallback =
            action == "enter-session" && empty && self.purpose != NewSessionPurpose::NativeAgentTab;
        if shell_fallback && !self.empty_enter_armed {
            self.empty_enter_armed = true;
            return None;
        }
        self.launch_direction = if action == "start-session-background" {
            LaunchDirection::Background
        } else {
            LaunchDirection::Foreground
        };
        self.shell_fallback = shell_fallback;
        self.empty_enter_armed = false;
        let invocation = if self.shell_fallback {
            form.shell_invocation(&form.draft.cwd)
        } else {
            form.invocation(&form.draft.cwd)
        };
        let invocation = match invocation {
            Ok(mut invocation) => {
                if self.purpose == NewSessionPurpose::NativeAgentTab {
                    "agents.native.tab".clone_into(&mut invocation.command);
                    if let Some(id) = self.surface_request_id {
                        "surface.create_agent".clone_into(&mut invocation.command);
                        invocation.arguments.insert(0, id.to_string());
                        invocation.target = None;
                    }
                }
                invocation
            }
            Err(error) => {
                form.error = Some(error);
                return None;
            }
        };
        match form.naming_invocation() {
            Ok(Some(invocation)) if !self.shell_fallback && form.draft.isolated => {
                self.starting = true;
                return Some(NewSessionPickerEvent::Names {
                    invocation,
                    action: action.to_owned(),
                });
            }
            Err(error) if !self.shell_fallback && form.draft.isolated => {
                form.error = Some(error);
                return None;
            }
            _ => {}
        }
        if empty && form.draft.isolated && form.draft.branch.is_empty() {
            let names = bootty_agents::GeneratedSessionNames {
                title: form.title(),
                slug: format!("terminal-{}", form.draft.suffix),
            };
            if let Err(error) = form.set_generated_names(names) {
                form.error = Some(error);
                return None;
            }
        }
        match form.worktree_request() {
            Ok(Some(request)) => {
                self.starting = true;
                Some(NewSessionPickerEvent::CreateWorktree {
                    repo: form.draft.cwd.clone(),
                    request,
                })
            }
            Ok(None) => {
                self.starting = true;
                Some(NewSessionPickerEvent::Submit(invocation))
            }
            Err(error) => {
                form.error = Some(error);
                None
            }
        }
    }

    fn activate_project(
        &mut self,
        project: ProjectPickerEntry,
        open_cwds: &[String],
    ) -> Option<NewSessionPickerEvent> {
        if let Some(form) = &mut self.form {
            form.set_directory(project.path.clone());
            self.step = NewSessionStep::Form;
            if let Some(worker) = &mut self.worker {
                worker.start(NewSessionEffect::ListWorktrees(project.path, Vec::new()));
            }
            return None;
        }
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
        Some(NewSessionPickerEvent::CreateWorktree { repo, request })
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

    pub(crate) fn start_create_worktree(
        &mut self,
        repo: String,
        request: bootty_git::WorktreeRequest,
    ) -> Result<bootty_control::CommandInvocation, String> {
        request.validate()?;
        let target = self
            .form
            .as_ref()
            .and_then(crate::presentation::new_session_form::NewSessionForm::destination)
            .map(|destination| destination.target.clone())
            .or_else(|| self.registration_target.clone())
            .ok_or("The worktree host binding is unavailable")?;
        let mut invocation = bootty_control::CommandInvocation::new(
            "worktree.create",
            vec![
                repo,
                request.branch,
                request.name.unwrap_or_default(),
                request.start_ref.unwrap_or_default(),
            ],
            bootty_control::Caller::Internal,
        );
        invocation.target = Some(target);
        self.starting = true;
        Ok(invocation)
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
    Submit(bootty_control::CommandInvocation),
    Catalog(bootty_control::CommandInvocation),
    Names {
        invocation: bootty_control::CommandInvocation,
        action: String,
    },
    Started {
        value: serde_json::Value,
        warnings: Vec<bootty_control::CommandWarning>,
        foreground: bool,
    },
    Error(String),
    AddProject {
        path: String,
    },
    ProjectAdded {
        path: String,
    },
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
