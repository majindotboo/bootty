//! Directory, worktree, and branch creation states for the new-session workflow.

use super::{NEW_SESSION_ID, normalized_name, parse_index};
use crate::gpui::{
    DialogAction, DialogIntent, DialogProgressState, DialogProgressStep, DialogRow, DialogSpec,
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
    projects: Vec<ProjectPickerEntry>,
    purpose: NewSessionPurpose,
    form: Option<crate::presentation::new_session_form::NewSessionForm>,
    launch: Option<PendingCreation>,
    /// The last `git.overview` requested for the form's branch pickers, and its reply.
    branches_requested: Option<bootty_control::CommandInvocation>,
    branches_pending: Option<PendingCommand>,
    checkout: Option<PendingCommand>,
    starting: bool,
    progress: Vec<DialogProgressStep>,
    failure: Option<String>,
    /// The repository this dialog made a new worktree from, whose setup scripts the session runs
    /// once it starts.
    worktree_repository: Option<String>,
    registration_target: Option<bootty_control::CommandTarget>,
}

struct PendingCommand {
    invocation: bootty_control::CommandInvocation,
    reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum NewSessionPurpose {
    CreateSession,
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
            branches_requested: None,
            branches_pending: None,
            checkout: None,
            starting: false,
            progress: Vec::new(),
            failure: None,
            worktree_repository: None,
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
            branches_requested: None,
            branches_pending: None,
            checkout: None,
            starting: false,
            progress: Vec::new(),
            failure: None,
            worktree_repository: None,
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
        self.starting || self.launch.is_some() || self.checkout.is_some()
    }

    pub fn checkout_started(
        &mut self,
        invocation: bootty_control::CommandInvocation,
        reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
    ) {
        self.checkout = Some(PendingCommand { invocation, reply });
    }

    pub fn checkout_failed(&mut self, invocation: &bootty_control::CommandInvocation, error: &str) {
        if let Some(form) = &mut self.form {
            form.branch_checkout_failed(
                invocation
                    .arguments
                    .get(1)
                    .map_or("selected branch", String::as_str),
                error,
            );
        }
    }

    fn poll_checkout(&mut self) {
        let Some(pending) = &self.checkout else {
            return;
        };
        let outcome = match pending.reply.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bootty_control::CommandOutcome::Failed {
                    code: "shutdown".into(),
                    message: "The Git command host stopped".into(),
                }
            }
        };
        let Some(pending) = self.checkout.take() else {
            return;
        };
        if matches!(outcome, bootty_control::CommandOutcome::Success { .. }) {
            if let Some(form) = &mut self.form
                && let Some(branch) = pending.invocation.arguments.get(1)
            {
                form.branch_checked_out(branch);
            }
            self.invalidate_branch_catalog();
        } else {
            self.checkout_failed(
                &pending.invocation,
                &crate::commands::command_outcome_message(&outcome)
                    .unwrap_or_else(|| "The branch command stopped without a result".into()),
            );
        }
    }

    fn invalidate_branch_catalog(&mut self) {
        self.branches_requested = None;
        self.branches_pending = None;
    }

    #[must_use]
    pub fn draft(&self) -> Option<&crate::presentation::new_session_form::NewSessionDraft> {
        self.form.as_ref().map(|form| &form.draft)
    }

    pub(crate) fn retained_draft(&self) -> Option<crate::state::creation_draft::CreationDraft> {
        let form = self.form.as_ref()?;
        Some(crate::state::creation_draft::CreationDraft {
            draft: form.draft.clone(),
            project: (!form.draft.projectless).then(|| form.project_path().to_owned()),
            title: form.title(),
        })
    }

    pub fn started(&mut self, launch: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>) {
        let detail = self.form.as_ref().map(|form| form.draft.cwd.clone());
        self.start_stage("session", "Start session".to_owned(), detail);
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
        let detail = self.form.as_ref().and_then(|form| form.spec(false).footer);
        self.start_stage("worktree", "Create worktree".to_owned(), detail);
        self.starting = false;
        self.launch = Some(PendingCreation {
            kind: CreationCommand::Worktree,
            reply,
        });
    }

    pub fn branches_started(
        &mut self,
        invocation: bootty_control::CommandInvocation,
        reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
    ) {
        self.branches_pending = Some(PendingCommand { invocation, reply });
    }

    pub fn branches_failed(
        &mut self,
        invocation: &bootty_control::CommandInvocation,
        error: String,
    ) {
        if let Some(form) = &mut self.form {
            form.set_branches_for(invocation, Err(error));
        }
    }

    /// Requests the form's branch catalog once per checkout, and applies its reply.
    fn poll_branches(&mut self) -> Option<NewSessionPickerEvent> {
        if !matches!(self.step, NewSessionStep::Form) || self.is_in_flight() {
            return None;
        }
        if let Some(pending) = &self.branches_pending {
            let result = match pending.reply.try_recv() {
                Ok(bootty_control::CommandOutcome::Success { value, .. }) => {
                    serde_json::from_value(value).map_err(|error| error.to_string())
                }
                Ok(outcome) => Err(crate::commands::command_outcome_message(&outcome)
                    .unwrap_or_else(|| "The branch catalog is unavailable".to_owned())),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("The Git command host stopped".to_owned())
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            };
            let pending = self.branches_pending.take()?;
            if let Some(form) = &mut self.form {
                form.set_branches_for(&pending.invocation, result);
            }
            return None;
        }
        let invocation = self.form.as_ref()?.branch_catalog_invocation()?;
        if self.branches_requested.as_ref() == Some(&invocation) {
            return None;
        }
        self.branches_requested = Some(invocation.clone());
        Some(NewSessionPickerEvent::Branches(invocation))
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
                detail: self.form.as_ref().map(|form| form.draft.cwd.clone()),
                state: DialogProgressState::Failed,
            });
        }
        if let NewSessionStep::BranchName(draft) = &mut self.step {
            draft.error = Some(error);
        } else {
            self.failure = Some(error);
        }
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
        if id == "worktree" && !self.progress.iter().any(|step| step.id == "session") {
            self.progress.push(DialogProgressStep {
                id: "session".to_owned(),
                label: "Start session".to_owned(),
                detail: None,
                state: DialogProgressState::Pending,
            });
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
        if self.checkout.is_some() {
            self.poll_checkout();
            return None;
        }
        if let Some(event) = self.poll_branches() {
            return Some(event);
        }
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
                if let Some(form) = &self.form
                    && matches!(self.step, NewSessionStep::Form)
                    && form.destination().is_some()
                    && !form.draft.cwd.is_empty()
                    && !form.draft.projectless
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
            self.complete_stage("session");
            return Some(NewSessionPickerEvent::Started {
                value,
                warnings,
                setup: self.worktree_repository.take(),
            });
        }
        self.failed(
            crate::commands::command_outcome_message(&outcome)
                .unwrap_or_else(|| "The session command stopped without a result".to_owned()),
        );
        if let Some(form) = &mut self.form
            && self.purpose == NewSessionPurpose::CreateSession
        {
            form.draft.identity = bootty_mux::snapshot::new_session_identity();
            form.draft.suffix.clone_from(&form.draft.identity);
        }
        None
    }

    fn created_worktree(&mut self, cwd: String) -> Option<NewSessionPickerEvent> {
        self.complete_stage("worktree");
        let Some(form) = &mut self.form else {
            return Some(NewSessionPickerEvent::CreateSession { cwd });
        };
        self.worktree_repository
            .get_or_insert_with(|| form.project_path().to_owned());
        // Retain the successful checkout on later launch failure. Retry uses it.
        form.accept_worktree(cwd.clone());
        match form.invocation(&cwd) {
            Ok(invocation) => Some(NewSessionPickerEvent::Submit(invocation)),
            Err(error) => {
                self.failed(error);
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
        if let Some(pending) = &self.checkout {
            spec.hint = Some(format!(
                "Switching to “{}”…",
                pending
                    .invocation
                    .arguments
                    .get(1)
                    .map_or("selected branch", String::as_str),
            ));
        }
        spec.progress.clone_from(&self.progress);
        spec.projects.clone_from(&self.projects);
        if let Some(error) = &self.failure {
            if let Some(submit) = spec.rows.first_mut() {
                submit.detail = Some(error.clone());
            }
            spec.hint =
                Some(
                    if self.progress.iter().any(|step| {
                        step.id == "worktree" && step.state == DialogProgressState::Complete
                    }) && !form.draft.isolated
                    {
                        "Checkout created. Retry uses this checkout to start the session."
                    } else if self.progress.iter().any(|step| {
                        step.id == "worktree" && step.state == DialogProgressState::Failed
                    }) {
                        "Review the branch, starting ref and destination above, then try again."
                    } else {
                        "Your draft is saved. Resolve the reported issue, then try again."
                    }
                    .to_owned(),
                );
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

    fn change_form_field(&mut self, field: &str, value: &str) -> Option<NewSessionPickerEvent> {
        if field == "project" {
            return self.change_project(value);
        }
        if field == "checkout-branch" && self.purpose == NewSessionPurpose::CreateSession {
            let previous = self.form.as_ref()?.draft.cwd.clone();
            let result = self.form.as_mut()?.select_checkout_branch(value);
            return match result {
                Ok(Some(invocation)) => Some(NewSessionPickerEvent::CheckoutBranch(invocation)),
                Ok(None) => {
                    if self.form.as_ref()?.draft.cwd != previous {
                        self.invalidate_branch_catalog();
                    }
                    None
                }
                Err(error) => {
                    self.form.as_mut()?.branch_checkout_failed(value, &error);
                    None
                }
            };
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
        self.activate_project(project, &[])
    }

    fn start_form(&mut self) -> Option<NewSessionPickerEvent> {
        self.reset_failed_attempt();
        let form = self.form.as_mut()?;
        let invocation = match form.invocation(&form.draft.cwd) {
            Ok(invocation) => invocation,
            Err(error) => {
                form.error = Some(error);
                return None;
            }
        };
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

    fn activate_project(
        &mut self,
        project: ProjectPickerEntry,
        open_cwds: &[String],
    ) -> Option<NewSessionPickerEvent> {
        if let Some(form) = &mut self.form {
            form.set_directory(project.path.clone());
            self.worktree_repository = None;
            self.progress.clear();
            self.failure = None;
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
            directory: None,
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

    /// The command that makes a worktree of `repo` for the session, which then starts in it
    /// and runs the project's setup scripts.
    /// # Errors
    /// Rejects an invalid request, or a dialog without a host to make it on.
    pub fn start_create_worktree(
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
        self.worktree_repository = Some(
            self.form
                .as_ref()
                .filter(|form| form.draft.cwd == repo)
                .map_or(repo.as_str(), |form| form.project_path())
                .to_owned(),
        );
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
    /// Load the form's branch catalog through `git.overview`.
    Branches(bootty_control::CommandInvocation),
    CheckoutBranch(bootty_control::CommandInvocation),
    Started {
        value: serde_json::Value,
        warnings: Vec<bootty_control::CommandWarning>,
        /// The repository of the new worktree the session started in, to run its setup scripts.
        setup: Option<String>,
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
