//! The retained new-session draft. Selecting a destination never starts work.

use std::collections::{BTreeMap, HashMap};

use bootty_config::config::RemoteConfig;
use bootty_control::{Caller, CommandInvocation, CommandTarget};
use bootty_mux::controller::SpaceId;

use crate::gpui::{DialogAction, DialogField, DialogFieldKind, DialogRow, DialogSpec};
use crate::presentation::dialogs::NEW_SESSION_ID;

#[derive(Clone, Debug)]
pub struct SessionDestination {
    pub scope: SpaceId,
    pub label: String,
    pub icon: String,
    pub color: [u8; 3],
    pub cwd: String,
    pub remote: Option<RemoteConfig>,
    pub target: CommandTarget,
}

/// Kept by the window after cancellation; successful Start consumes its contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSessionDraft {
    pub scope: SpaceId,
    pub cwd: String,
    pub projectless: bool,
    pub isolated: bool,
    pub isolation_preference: bool,
    pub branch: String,
    pub folder: String,
    pub start_ref: String,
    pub suffix: String,
    pub identity: String,
    pub directories: HashMap<SpaceId, String>,
}

pub struct NewSessionForm {
    restored: bool,
    pub draft: NewSessionDraft,
    destinations: Vec<SessionDestination>,
    pub worktree_available: bool,
    worktree_repository: Option<String>,
    worktree_directory: Option<String>,
    branches: Vec<bootty_git::changes::GitBranch>,
    checkout_root: Option<String>,
    branch_error: Option<String>,
    /// The configured `session.branch-prefix`, for projects without their own.
    branch_prefix: String,
    pub error: Option<String>,
    project_defaults: Vec<bootty_mux::repository::RegisteredProject>,
}

impl NewSessionForm {
    #[must_use]
    pub fn new(draft: NewSessionDraft, destinations: Vec<SessionDestination>) -> Self {
        let mut form = Self {
            restored: false,
            draft,
            destinations,
            worktree_available: false,
            worktree_repository: None,
            worktree_directory: None,
            branches: Vec::new(),
            checkout_root: None,
            branch_error: None,
            branch_prefix: bootty_config::config::DEFAULT_BRANCH_PREFIX.to_owned(),
            error: None,
            project_defaults: Vec::new(),
        };
        if form.draft.isolated && form.destination().is_some() {
            form.change_field("isolation", "New worktree");
        } else {
            form.draft.isolated = false;
        }
        form
    }

    #[must_use]
    pub fn destination(&self) -> Option<&SessionDestination> {
        self.destinations
            .iter()
            .find(|destination| destination.scope == self.draft.scope)
    }

    pub fn set_worktree_catalog(&mut self, worktrees: &[bootty_git::WorktreePickerEntry]) {
        if self.draft.projectless {
            return;
        }
        // The main checkout names the repository folder under the worktree directory.
        self.worktree_repository = worktrees
            .iter()
            .find(|entry| !entry.is_new)
            .and_then(|entry| entry.path.as_deref())
            .map(str::to_owned);
        self.apply_project_defaults();
        self.set_worktree_available(
            self.destination().is_some() && worktrees.iter().any(|entry| entry.is_new),
        );
    }

    pub const fn set_worktree_available(&mut self, available: bool) {
        self.worktree_available = available && !self.draft.projectless;
        if !available {
            self.draft.isolated = false;
        }
    }

    #[must_use]
    pub fn branch_catalog_invocation(&self) -> Option<CommandInvocation> {
        if !self.worktree_available {
            return None;
        }
        let mut invocation = CommandInvocation::new(
            "git.overview",
            vec![self.draft.cwd.clone(), "1".into()],
            Caller::Internal,
        );
        invocation.target = Some(self.destination()?.target.clone());
        Some(invocation)
    }

    pub fn set_branches_for(
        &mut self,
        invocation: &CommandInvocation,
        result: Result<bootty_git::changes::RepositoryOverview, String>,
    ) {
        if invocation.arguments.first() != Some(&self.draft.cwd)
            || invocation.target.as_ref()
                != self.destination().map(|destination| &destination.target)
        {
            return;
        }
        match result {
            Ok(overview) => {
                if (!self.draft.isolated || matches!(self.draft.start_ref.as_str(), "" | "HEAD"))
                    && let Some(current) = overview.branches.iter().find(|branch| branch.current)
                {
                    self.draft.start_ref.clone_from(&current.name);
                }
                self.branches = overview.branches;
                self.checkout_root = Some(overview.root);
                self.branch_error = None;
            }
            Err(error) => self.branch_error = Some(format!("Could not load branches: {error}")),
        }
    }

    /// Reuse a branch's checkout, or request a switch through the binding's Git owner.
    /// # Errors
    /// Rejects choices outside the loaded local branch catalog.
    pub fn select_checkout_branch(
        &mut self,
        name: &str,
    ) -> Result<Option<CommandInvocation>, String> {
        let branch = self
            .branches
            .iter()
            .find(|branch| branch.name == name)
            .ok_or("Select an existing branch")?;
        self.branch_error = None;
        if branch.current {
            return Ok(None);
        }
        if let Some(cwd) = branch.worktree_path.clone() {
            let project = self.project_path().to_owned();
            let mut branches = std::mem::take(&mut self.branches);
            for branch in &mut branches {
                branch.current = branch.name == name;
            }
            self.update_directory(cwd.clone());
            self.worktree_repository = Some(project);
            self.checkout_root = Some(cwd);
            self.branches = branches;
            name.clone_into(&mut self.draft.start_ref);
            self.set_worktree_available(true);
            return Ok(None);
        }
        let mut invocation = CommandInvocation::new(
            "git.branch-checkout",
            vec![self.draft.cwd.clone(), name.to_owned()],
            Caller::Internal,
        );
        invocation.target = Some(self.destination().ok_or("Select a host")?.target.clone());
        Ok(Some(invocation))
    }

    pub fn branch_checked_out(&mut self, name: &str) {
        name.clone_into(&mut self.draft.start_ref);
        for branch in &mut self.branches {
            if branch.current {
                branch.worktree_path = None;
            }
            branch.current = branch.name == name;
            if branch.current {
                branch.worktree_path = Some(self.draft.cwd.clone());
            }
        }
        self.branch_error = None;
        self.error = None;
    }

    pub fn branch_checkout_failed(&mut self, name: &str, error: &str) {
        self.branch_error = Some(format!("Could not switch to “{name}”: {error}"));
    }

    pub fn set_directory(&mut self, cwd: String) {
        self.restored = false;
        self.draft.projectless = false;
        self.update_directory(cwd);
    }

    pub(crate) fn set_project_path(&mut self, project: Option<String>) {
        self.restored = true;
        self.worktree_repository = project;
    }

    /// Git's main checkout identifies the project; `draft.cwd` identifies where work runs.
    #[must_use]
    pub fn project_path(&self) -> &str {
        self.worktree_repository
            .as_deref()
            .unwrap_or(&self.draft.cwd)
    }

    pub fn accept_worktree(&mut self, cwd: String) {
        let project = self.project_path().to_owned();
        self.update_directory(cwd);
        self.worktree_repository = Some(project);
        self.draft.isolated = false;
    }

    pub fn set_no_project(&mut self, cwd: String) {
        self.draft.projectless = true;
        self.draft.isolated = false;
        self.update_directory(cwd);
    }

    fn update_directory(&mut self, cwd: String) {
        self.draft.cwd = cwd;
        self.branches.clear();
        self.checkout_root = None;
        self.branch_error = None;
        self.worktree_available = false;
        self.worktree_repository = None;
        self.error = None;
        self.apply_project_defaults();
    }

    pub fn set_project_defaults(
        &mut self,
        projects: Vec<bootty_mux::repository::RegisteredProject>,
    ) {
        self.project_defaults = projects;
        self.apply_project_defaults();
    }

    /// The configured `session.worktree-directory`, shown in the destination preview.
    pub fn set_worktree_directory(&mut self, directory: Option<String>) {
        self.worktree_directory = directory.filter(|directory| !directory.trim().is_empty());
    }

    /// The configured `session.branch-prefix`, which a project's own prefix overrides.
    pub fn set_branch_prefix(&mut self, prefix: String) {
        self.branch_prefix = prefix;
    }

    fn project_settings(&self) -> Option<&bootty_mux::repository::ProjectSettings> {
        if self.draft.projectless {
            return None;
        }
        self.project_defaults
            .iter()
            .find(|project| project.scope == self.draft.scope && project.cwd == self.project_path())
            .map(|project| &project.settings)
    }

    fn apply_project_defaults(&mut self) {
        if self.restored {
            return;
        }
        let Some(settings) = self.project_settings().cloned() else {
            return;
        };
        self.draft.isolation_preference = settings.isolated;
        self.draft.isolated = settings.isolated && self.destination().is_some();
        self.draft.start_ref = settings.start_ref;
    }

    /// Returns whether host discovery must be restarted. All text drafts survive switching.
    pub fn change_field(&mut self, field: &str, value: &str) -> bool {
        self.error = None;
        match field {
            "host" => {
                self.restored = false;
                let Some(destination) = self
                    .destinations
                    .iter()
                    .find(|destination| destination.label == value)
                else {
                    return false;
                };
                if destination.scope == self.draft.scope {
                    return false;
                }
                self.draft
                    .directories
                    .insert(self.draft.scope, self.draft.cwd.clone());
                self.draft.scope = destination.scope;
                self.draft.cwd = self
                    .draft
                    .directories
                    .get(&destination.scope)
                    .cloned()
                    .unwrap_or_else(|| destination.cwd.clone());
                self.worktree_available = false;
                self.worktree_repository = None;
                self.branches.clear();
                self.branch_error = None;
                return true;
            }
            "isolation" => {
                self.draft.isolated = value == "New worktree";
                self.draft.isolation_preference = self.draft.isolated;
                if self.draft.isolated && self.draft.start_ref.is_empty() {
                    "HEAD".clone_into(&mut self.draft.start_ref);
                }
            }
            "branch" => value.clone_into(&mut self.draft.branch),
            "folder" => value.clone_into(&mut self.draft.folder),
            "start-ref" => value.clone_into(&mut self.draft.start_ref),
            _ => {}
        }
        false
    }

    /// The new worktree's branch and folder. Empty fields fall back to a name from the draft's
    /// unique suffix, so a worktree never needs typed names.
    fn worktree_names(&self) -> (String, String) {
        let generated = format!("terminal-{}", self.draft.suffix);
        let branch = trimmed(&self.draft.branch)
            .unwrap_or_else(|| format!("{}{generated}", self.branch_prefix()));
        let folder = trimmed(&self.draft.folder).unwrap_or_else(|| {
            if self.draft.branch.trim().is_empty() {
                generated
            } else {
                branch.replace('/', "-")
            }
        });
        (branch, folder)
    }

    /// # Errors
    /// Returns invalid worktree fields or unavailable host support.
    pub fn worktree_request(&self) -> Result<Option<bootty_git::WorktreeRequest>, String> {
        if !self.draft.isolated {
            return Ok(None);
        }
        if !self.worktree_available {
            return Err("New worktrees are unavailable for this project and host".to_owned());
        }
        let (branch, folder) = self.worktree_names();
        let request = bootty_git::WorktreeRequest {
            branch,
            name: Some(folder),
            start_ref: trimmed(&self.draft.start_ref),
            directory: None,
        };
        request.validate()?;
        Ok(Some(request))
    }

    /// # Errors
    /// Returns an unavailable destination or an empty directory.
    pub fn invocation(&self, cwd: &str) -> Result<CommandInvocation, String> {
        let destination = self
            .destination()
            .ok_or("The selected host is unavailable")?;
        if cwd.trim().is_empty() {
            return Err("Choose a project directory".to_owned());
        }
        let mut invocation = CommandInvocation::new(
            "session.create",
            vec![
                format!("task-{}", self.draft.suffix),
                cwd.to_owned(),
                "[]".to_owned(),
                self.draft.identity.clone(),
                self.title(),
                self.draft.projectless.to_string(),
            ],
            Caller::Internal,
        );
        invocation.target = Some(destination.target.clone());
        Ok(invocation)
    }

    #[must_use]
    pub fn title(&self) -> String {
        bounded_title(&format!("Terminal in {}", directory_name(&self.draft.cwd)))
    }

    fn branch_prefix(&self) -> &str {
        self.project_settings()
            .map(|settings| settings.branch_prefix.as_str())
            .filter(|prefix| !prefix.is_empty())
            .unwrap_or(&self.branch_prefix)
    }

    fn project_labels(&self) -> BTreeMap<String, String> {
        self.project_defaults
            .iter()
            .filter(|project| {
                project.scope == self.draft.scope && !project.settings.name.is_empty()
            })
            .map(|project| (project.cwd.clone(), project.settings.name.clone()))
            .collect()
    }

    fn submission_error(&self) -> Option<String> {
        self.error
            .clone()
            .or_else(|| self.invocation(&self.draft.cwd).err())
    }

    #[must_use]
    pub fn spec(&self, busy: bool) -> DialogSpec {
        let mut spec = DialogSpec::prompt(
            NEW_SESSION_ID,
            "New session",
            "",
            "",
            DialogAction::new("start-session"),
        );
        spec.branches.clone_from(&self.branches);
        spec.project_labels = self.project_labels();
        spec.selected_project = Some(if self.draft.projectless {
            String::new()
        } else {
            self.project_path().to_owned()
        });
        spec.hint = self.worktree_hint();
        spec.busy = busy;
        if let Some(destination) = self.destination() {
            spec.fields.push(choice(
                "host",
                "Space",
                &destination.label,
                self.destinations
                    .iter()
                    .map(|destination| destination.label.clone())
                    .collect(),
            ));
        }
        spec.spaces = self
            .destinations
            .iter()
            .map(|destination| crate::gpui::DialogSpaceChoice {
                label: destination.label.clone(),
                icon: destination.icon.clone(),
                color: destination.color,
            })
            .collect();
        spec.fields.extend(self.worktree_fields());
        spec.footer = Some(self.destination_path());
        if let Some(row) = spec.rows.first_mut() {
            (if busy { "Starting…" } else { "Start" }).clone_into(&mut row.label);
            row.detail = self.submission_error();
            row.enabled = !busy
                && self.invocation(&self.draft.cwd).is_ok()
                && (!self.draft.isolated || self.worktree_available);
        }
        spec.rows.push(self.project_choice(busy));
        spec
    }

    fn worktree_hint(&self) -> Option<String> {
        self.branch_error.as_ref().map(|error| {
            if self.draft.isolated {
                format!("{error}. Search for a ref to use it directly.")
            } else {
                error.clone()
            }
        })
    }

    fn project_choice(&self, busy: bool) -> DialogRow {
        let mut project = DialogRow::action(
            "choose-project",
            if self.draft.projectless {
                "No project…".into()
            } else {
                format!(
                    "{}…",
                    self.project_settings()
                        .map(|settings| settings.name.as_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| directory_name(self.project_path()))
                )
            },
            DialogAction::new("choose-project"),
        );
        project.enabled = !busy;
        project
    }

    fn destination_path(&self) -> String {
        if !self.draft.isolated {
            return self.draft.cwd.clone();
        }
        // Mirrors bootty_git's host-side default; the host resolves `~`.
        let namespace = bootty_config::ApplicationIdentity::for_process().namespace();
        let directory = self.worktree_directory.clone().unwrap_or_else(|| {
            if cfg!(windows) {
                format!("%LOCALAPPDATA%\\{namespace}\\worktrees")
            } else {
                format!("~/.local/share/{namespace}/worktrees")
            }
        });
        let repository = directory_name(self.project_path());
        let repository = repository.strip_suffix(".git").unwrap_or(repository);
        let (_, folder) = self.worktree_names();
        format!("{directory}/{repository}/{folder}")
    }

    fn worktree_fields(&self) -> Vec<DialogField> {
        if !self.worktree_available {
            return Vec::new();
        }
        let current = if self
            .checkout_root
            .as_deref()
            .is_some_and(|root| root != self.project_path())
        {
            "Current worktree"
        } else {
            "Current checkout"
        };
        let mut fields = vec![choice(
            "isolation",
            "Checkout",
            if self.draft.isolated {
                "New worktree"
            } else {
                current
            },
            vec![current.to_owned(), "New worktree".to_owned()],
        )];
        let branch_names = || {
            self.branches
                .iter()
                .map(|branch| branch.name.clone())
                .collect::<Vec<_>>()
        };
        if !self.draft.isolated {
            fields.push(DialogField {
                id: "checkout-branch".into(),
                label: "Branch".into(),
                value: self
                    .branches
                    .iter()
                    .find(|branch| branch.current)
                    .map_or_else(String::new, |branch| branch.name.clone()),
                placeholder: "Select branch…".into(),
                kind: DialogFieldKind::Choice(branch_names()),
            });
            return fields;
        }
        let mut branches = branch_names();
        if !matches!(self.draft.start_ref.as_str(), "" | "HEAD")
            && !branches.contains(&self.draft.start_ref)
        {
            branches.insert(0, self.draft.start_ref.clone());
        }
        fields.push(DialogField {
            id: "start-ref".into(),
            label: "Base branch".into(),
            value: self.draft.start_ref.clone(),
            placeholder: "Select a base branch…".into(),
            kind: DialogFieldKind::Choice(branches),
        });
        let (branch, folder) = self.worktree_names();
        for (id, label, value, placeholder) in [
            ("branch", "Branch", &self.draft.branch, branch),
            ("folder", "Folder", &self.draft.folder, folder),
        ] {
            fields.push(DialogField {
                id: id.to_owned(),
                label: label.to_owned(),
                value: value.clone(),
                placeholder,
                kind: DialogFieldKind::Text,
            });
        }
        fields
    }
}

fn bounded_title(title: &str) -> String {
    let mut bytes = 0_usize;
    title
        .chars()
        .take(72)
        .take_while(|character| {
            bytes = bytes.saturating_add(character.len_utf8());
            bytes <= 256
        })
        .collect()
}

fn choice(id: &str, label: &str, value: &str, options: Vec<String>) -> DialogField {
    DialogField {
        id: id.to_owned(),
        label: label.to_owned(),
        value: value.to_owned(),
        placeholder: String::new(),
        kind: DialogFieldKind::Choice(options),
    }
}
fn trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
fn directory_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("project")
}
