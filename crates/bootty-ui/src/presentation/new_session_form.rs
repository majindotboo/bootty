//! The new-session form. Selecting a destination never starts work.

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

/// The choices of an unstarted session: where it runs and in which directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSessionDraft {
    pub scope: SpaceId,
    pub cwd: String,
    pub projectless: bool,
    pub suffix: String,
    pub identity: String,
    pub directories: HashMap<SpaceId, String>,
}

pub struct NewSessionForm {
    pub draft: NewSessionDraft,
    destinations: Vec<SessionDestination>,
    pub error: Option<String>,
    project_defaults: Vec<bootty_mux::repository::RegisteredProject>,
}

impl NewSessionForm {
    #[must_use]
    pub const fn new(draft: NewSessionDraft, destinations: Vec<SessionDestination>) -> Self {
        Self {
            draft,
            destinations,
            error: None,
            project_defaults: Vec::new(),
        }
    }

    #[must_use]
    pub fn destination(&self) -> Option<&SessionDestination> {
        self.destinations
            .iter()
            .find(|destination| destination.scope == self.draft.scope)
    }

    pub fn set_directory(&mut self, cwd: String) {
        self.draft.projectless = false;
        self.update_directory(cwd);
    }

    #[must_use]
    pub fn project_path(&self) -> &str {
        &self.draft.cwd
    }

    pub fn set_no_project(&mut self, cwd: String) {
        self.draft.projectless = true;
        self.update_directory(cwd);
    }

    fn update_directory(&mut self, cwd: String) {
        self.draft.cwd = cwd;
        self.error = None;
    }

    pub fn set_project_defaults(
        &mut self,
        projects: Vec<bootty_mux::repository::RegisteredProject>,
    ) {
        self.project_defaults = projects;
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

    /// Returns whether host discovery must be restarted.
    pub fn change_field(&mut self, field: &str, value: &str) -> bool {
        self.error = None;
        if field != "host" {
            return false;
        }
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
        true
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
        spec.project_labels = self.project_labels();
        spec.selected_project = Some(if self.draft.projectless {
            String::new()
        } else {
            self.project_path().to_owned()
        });
        // The Start button and its keys explain themselves; the prompt's key hint is noise here.
        spec.hint = None;
        spec.busy = busy;
        if let Some(destination) = self.destination() {
            spec.fields.push(DialogField {
                id: "host".to_owned(),
                label: "Space".to_owned(),
                value: destination.label.clone(),
                placeholder: String::new(),
                kind: DialogFieldKind::Choice(
                    self.destinations
                        .iter()
                        .map(|destination| destination.label.clone())
                        .collect(),
                ),
            });
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
        spec.footer = Some(self.display_cwd());
        if let Some(row) = spec.rows.first_mut() {
            (if busy { "Starting…" } else { "Start" }).clone_into(&mut row.label);
            row.detail = self.submission_error();
            row.enabled = !busy && self.invocation(&self.draft.cwd).is_ok();
        }
        spec.rows.push(self.project_choice(busy));
        spec
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

    /// Dialog text for the session directory. Local paths abbreviate this machine's home; a
    /// remote home is unknown here.
    #[must_use]
    pub fn display_cwd(&self) -> String {
        if self
            .destination()
            .is_some_and(|destination| destination.remote.is_some())
        {
            return self.draft.cwd.clone();
        }
        bootty_git::project::display_path(&self.draft.cwd, crate::strings::home_dir().as_deref())
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

fn directory_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("project")
}
