//! The retained new-session draft. Selecting a destination never starts work.

use std::collections::{BTreeMap, HashMap};
use std::{path::PathBuf, sync::Arc};

use bootty_config::config::{AgentProvidersConfig, RemoteConfig};
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
    /// Opaque attachments cannot create a checkout on their host.
    pub worktrees: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewSessionMode {
    #[default]
    Agent,
    Terminal,
}

/// Local draft bytes stay alive until the created session admits them.
#[derive(Clone)]
pub struct NewSessionAttachment {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub prompt_ranges: Vec<std::ops::Range<usize>>,
    pub preview: Option<Arc<gpui_kit::RenderImage>>,
    pub temporary: Option<crate::attachment_source::AttachmentTemporary>,
}

impl std::fmt::Debug for NewSessionAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewSessionAttachment")
            .field("path", &self.path)
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

impl PartialEq for NewSessionAttachment {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.size_bytes == other.size_bytes
            && self.prompt_ranges == other.prompt_ranges
            && self.preview == other.preview
    }
}

impl Eq for NewSessionAttachment {}

/// Kept by the window after cancellation; successful Start consumes its contents.
#[derive(Clone, Debug)]
pub struct NewSessionDraft {
    pub scope: SpaceId,
    pub cwd: String,
    pub mode: NewSessionMode,
    pub prompt: String,
    pub attachments: Vec<NewSessionAttachment>,
    pub applications: Vec<bootty_agents::NativeApplicationMention>,
    pub command: String,
    pub provider: String,
    pub profiles: BTreeMap<String, String>,
    pub model_selection: Option<bootty_agents::NativeModelSelection>,
    pub permissions: bootty_agents::NativePermissionMode,
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
    pub draft: NewSessionDraft,
    destinations: Vec<SessionDestination>,
    providers: AgentProvidersConfig,
    pub worktree_available: bool,
    worktree_parent: Option<String>,
    pub error: Option<String>,
    pub model_options: Vec<bootty_agents::NativeModelOption>,
    pub model_error: Option<String>,
    pub models_loading: bool,
    pub provider_permissions: Option<bootty_agents::NativePermissionMode>,
    generated_names: Option<bootty_agents::GeneratedSessionNames>,
    project_defaults: Vec<bootty_mux::repository::RegisteredProject>,
}

impl NewSessionForm {
    #[must_use]
    pub fn new(
        draft: NewSessionDraft,
        destinations: Vec<SessionDestination>,
        providers: AgentProvidersConfig,
    ) -> Self {
        let mut form = Self {
            draft,
            destinations,
            providers,
            worktree_available: false,
            worktree_parent: None,
            error: None,
            model_options: Vec::new(),
            model_error: None,
            models_loading: false,
            provider_permissions: None,
            generated_names: None,
            project_defaults: Vec::new(),
        };
        if !bootty_agents::AgentKind::ALL.into_iter().any(|provider| {
            bootty_agents::NativeSessionConfig::supports_provider(provider)
                && provider.to_string() == form.draft.provider
        }) {
            "codex".clone_into(&mut form.draft.provider);
        }
        if form.draft.isolated
            && form
                .destination()
                .is_some_and(|destination| destination.worktrees)
        {
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
        self.worktree_parent = worktrees
            .iter()
            .find(|entry| !entry.is_new)
            .and_then(|entry| entry.path.as_deref())
            .and_then(|path| path.trim_end_matches(['/', '\\']).rsplit_once(['/', '\\']))
            .map(|(parent, _)| parent.to_owned());
        self.set_worktree_available(
            self.destination()
                .is_some_and(|destination| destination.worktrees)
                && worktrees.iter().any(|entry| entry.is_new),
        );
    }

    pub const fn set_worktree_available(&mut self, available: bool) {
        self.worktree_available = available;
        if !available {
            self.draft.isolated = false;
        }
    }

    pub fn set_directory(&mut self, cwd: String) {
        self.draft.cwd = cwd;
        self.worktree_available = false;
        self.worktree_parent = None;
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

    fn project_settings(&self) -> Option<&bootty_mux::repository::ProjectSettings> {
        self.project_defaults
            .iter()
            .find(|project| project.scope == self.draft.scope && project.cwd == self.draft.cwd)
            .map(|project| &project.settings)
    }

    fn apply_project_defaults(&mut self) {
        let Some(settings) = self.project_settings().cloned() else {
            return;
        };
        let provider = if settings.provider.is_empty() {
            &self.providers.default_provider
        } else {
            &settings.provider
        };
        if self.draft.provider != *provider
            && self
                .providers
                .provider(provider)
                .is_some_and(|provider| provider.enabled)
        {
            self.draft.provider.clone_from(provider);
            self.reset_provider_permissions();
            self.draft.model_selection = None;
            self.model_options.clear();
        }
        self.draft.isolation_preference = settings.isolated;
        self.draft.isolated = settings.isolated
            && self
                .destination()
                .is_some_and(|destination| destination.worktrees);
        self.draft.start_ref = settings.start_ref;
    }

    /// Returns whether host discovery must be restarted. All text drafts survive switching.
    pub fn change_field(&mut self, field: &str, value: &str) -> bool {
        self.error = None;
        match field {
            "host" => {
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
                self.worktree_parent = None;
                if !destination.worktrees {
                    self.draft.isolated = false;
                }
                return true;
            }
            "mode" => {
                self.draft.mode = if value == "Terminal" {
                    NewSessionMode::Terminal
                } else {
                    NewSessionMode::Agent
                }
            }
            "provider" => self.change_provider(value),
            "profile" => {
                let selected = self
                    .providers
                    .provider(&self.draft.provider)
                    .and_then(|provider| {
                        provider
                            .profiles
                            .iter()
                            .find(|(id, profile)| profile_label(id, &profile.name) == value)
                    })
                    .map_or_else(String::new, |(id, _)| id.clone());
                let previous = self.draft.profiles.get(&self.draft.provider).map_or_else(
                    || {
                        self.providers
                            .provider(&self.draft.provider)
                            .map_or("", |provider| provider.selected.as_str())
                    },
                    String::as_str,
                );
                if previous != selected {
                    self.draft.model_selection = None;
                    self.model_options.clear();
                    self.reset_provider_permissions();
                }
                self.draft
                    .profiles
                    .insert(self.draft.provider.clone(), selected);
            }
            "model" | "reasoning" => self.change_model_field(field, value),
            "permissions" => {
                self.draft.permissions = bootty_agents::NativePermissionMode::from_choice(value)
                    .unwrap_or(self.draft.permissions);
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

    fn change_provider(&mut self, value: &str) {
        let provider = match value {
            "Codex" => "codex",
            "Claude" => "claude",
            "Pi" => "pi",
            id => id,
        };
        if bootty_agents::AgentKind::ALL.into_iter().any(|kind| {
            kind.to_string() == provider
                && bootty_agents::NativeSessionConfig::supports_provider(kind)
        }) && self
            .providers
            .provider(provider)
            .is_some_and(|provider| provider.enabled)
        {
            if self.draft.provider != provider {
                self.draft.model_selection = None;
                self.model_options.clear();
                self.reset_provider_permissions();
            }
            provider.clone_into(&mut self.draft.provider);
        }
    }

    fn change_model_field(&mut self, field: &str, value: &str) {
        match field {
            "model" => {
                if let Some(option) = self.model_options.iter().find(|option| {
                    option.id == value || model_label(option, &self.model_options) == value
                }) {
                    self.draft.model_selection = Some(bootty_agents::NativeModelSelection {
                        model: option.id.clone(),
                        reasoning_effort: option
                            .default_reasoning_effort
                            .clone()
                            .filter(|effort| option.reasoning_efforts.contains(effort)),
                    });
                }
            }
            "reasoning" => {
                let selected = self
                    .draft
                    .model_selection
                    .as_ref()
                    .map(|selection| selection.model.as_str());
                if let Some(option) = self.model_options.iter().find(|option| {
                    selected.map_or(option.is_default, |selected| selected == option.id)
                }) && option
                    .reasoning_efforts
                    .iter()
                    .any(|effort| effort == value)
                {
                    self.draft.model_selection = Some(bootty_agents::NativeModelSelection {
                        model: option.id.clone(),
                        reasoning_effort: Some(value.to_owned()),
                    });
                }
            }
            _ => {}
        }
    }

    pub fn change_text(&mut self, value: &str) {
        if let Some(names) = self.generated_names.take() {
            if self.draft.branch == format!("{}{}", self.branch_prefix(), names.slug) {
                self.draft.branch.clear();
            }
            if self.draft.folder == format!("{}-{}", directory_name(&self.draft.cwd), names.slug) {
                self.draft.folder.clear();
            }
        }
        value.clone_into(match self.draft.mode {
            NewSessionMode::Agent => &mut self.draft.prompt,
            NewSessionMode::Terminal => &mut self.draft.command,
        });
        self.error = None;
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
        let request = bootty_git::WorktreeRequest {
            branch: self.draft.branch.trim().to_owned(),
            name: Some(trimmed(&self.draft.folder).ok_or("Enter a worktree folder name")?),
            start_ref: trimmed(&self.draft.start_ref),
        };
        request.validate()?;
        Ok(Some(request))
    }

    /// # Errors
    /// Returns an unavailable destination, directory, provider, or profile.
    pub fn invocation(&self, cwd: &str) -> Result<CommandInvocation, String> {
        let destination = self
            .destination()
            .ok_or("The selected host is unavailable")?;
        if cwd.trim().is_empty() {
            return Err("Choose a project directory".to_owned());
        }
        let name = format!("task-{}", self.draft.suffix);
        let mut invocation = match self.draft.mode {
            NewSessionMode::Terminal => {
                let argv = if self.draft.command.trim().is_empty() {
                    Vec::new()
                } else if cfg!(windows) && destination.remote.is_none() {
                    vec![
                        "powershell.exe".to_owned(),
                        "-NoProfile".to_owned(),
                        "-Command".to_owned(),
                        self.draft.command.clone(),
                    ]
                } else {
                    vec![
                        "/bin/sh".to_owned(),
                        "-lc".to_owned(),
                        self.draft.command.clone(),
                    ]
                };
                CommandInvocation::new(
                    "session.create",
                    vec![
                        name,
                        cwd.to_owned(),
                        serde_json::to_string(&argv).map_err(|error| error.to_string())?,
                        self.draft.identity.clone(),
                        self.title(),
                    ],
                    Caller::Internal,
                )
            }
            NewSessionMode::Agent => {
                if !bootty_agents::AgentKind::ALL.into_iter().any(|provider| {
                    bootty_agents::NativeSessionConfig::supports_provider(provider)
                        && provider.to_string() == self.draft.provider
                }) {
                    return Err("Choose a supported native provider".to_owned());
                }
                let provider = self
                    .providers
                    .provider(&self.draft.provider)
                    .filter(|provider| provider.enabled)
                    .ok_or("Choose an enabled provider")?;
                let selected = self
                    .draft
                    .profiles
                    .get(&self.draft.provider)
                    .map_or(provider.selected.as_str(), String::as_str);
                let profile = if selected.is_empty() {
                    None
                } else {
                    Some(
                        provider
                            .profiles
                            .get(selected)
                            .ok_or("The selected profile is unavailable")?,
                    )
                };
                let arguments = profile.map_or_else(Vec::new, |profile| profile.arguments.clone());
                CommandInvocation::new(
                    "agents.native.start",
                    vec![
                        self.draft.provider.clone(),
                        cwd.to_owned(),
                        provider.program.clone(),
                        serde_json::to_string(&arguments).map_err(|error| error.to_string())?,
                        name,
                        selected.to_owned(),
                        self.draft.identity.clone(),
                        self.title(),
                        self.draft.prompt.clone(),
                    ],
                    Caller::Internal,
                )
            }
        };
        self.append_native_options(&mut invocation)?;
        invocation.target = Some(destination.target.clone());
        Ok(invocation)
    }

    fn append_native_options(&self, invocation: &mut CommandInvocation) -> Result<(), String> {
        if self.draft.mode == NewSessionMode::Agent && !self.draft.attachments.is_empty() {
            if self.draft.attachments.len() > 16
                || self
                    .draft
                    .attachments
                    .iter()
                    .any(|attachment| !attachment.path.is_absolute())
            {
                return Err("Choose at most 16 local files".into());
            }
            invocation.arguments.push(String::new());
            invocation.arguments.push(
                serde_json::to_string(
                    &self
                        .draft
                        .attachments
                        .iter()
                        .map(|attachment| &attachment.path)
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| error.to_string())?,
            );
        }
        if self.draft.mode == NewSessionMode::Agent
            && let Some(selection) = &self.draft.model_selection
        {
            invocation.arguments.resize(11, String::new());
            invocation
                .arguments
                .push(serde_json::to_string(selection).map_err(|error| error.to_string())?);
        }
        if self.draft.mode == NewSessionMode::Agent && !self.draft.applications.is_empty() {
            invocation.arguments.resize(12, String::new());
            invocation
                .arguments
                .push(serde_json::to_string(&self.draft.applications).map_err(|e| e.to_string())?);
        }
        if self.draft.mode == NewSessionMode::Agent && !self.draft.attachments.is_empty() {
            invocation.arguments.resize(13, String::new());
            invocation.arguments.push(
                serde_json::to_string(
                    &self
                        .draft
                        .attachments
                        .iter()
                        .map(|attachment| &attachment.prompt_ranges)
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| error.to_string())?,
            );
        }
        if self.draft.mode == NewSessionMode::Agent
            && self.draft.permissions != bootty_agents::NativePermissionMode::ProviderDefault
        {
            invocation.arguments.resize(14, String::new());
            invocation
                .arguments
                .push(self.draft.permissions.id().to_owned());
        }
        Ok(())
    }

    #[must_use]
    pub fn model_catalog_invocation(&self) -> Option<CommandInvocation> {
        if self.draft.mode != NewSessionMode::Agent {
            return None;
        }
        let mut invocation = self.invocation(&self.draft.cwd).ok()?;
        "agents.native.catalog-info".clone_into(&mut invocation.command);
        invocation.arguments.truncate(6);
        Some(invocation)
    }

    const fn reset_provider_permissions(&mut self) {
        self.draft.permissions = bootty_agents::NativePermissionMode::ProviderDefault;
        self.provider_permissions = None;
    }

    /// Display the selected account policy while leaving inherited launch values untouched.
    #[must_use]
    pub fn permission_selection(&self) -> bootty_agents::NativePermissionMode {
        if self.draft.permissions == bootty_agents::NativePermissionMode::ProviderDefault {
            self.provider_permissions.unwrap_or(self.draft.permissions)
        } else {
            self.draft.permissions
        }
    }

    pub fn set_provider_catalog(
        &mut self,
        catalog: Result<bootty_agents::NativeProviderCatalog, String>,
    ) {
        match catalog {
            Ok(catalog) => {
                self.provider_permissions = catalog.permissions;
                self.set_model_catalog(Ok(catalog.models));
            }
            Err(error) => {
                self.provider_permissions = None;
                self.set_model_catalog(Err(error));
            }
        }
    }

    pub fn set_model_catalog(
        &mut self,
        options: Result<Vec<bootty_agents::NativeModelOption>, String>,
    ) {
        self.models_loading = false;
        match options {
            Ok(options) if options.is_empty() => {
                self.model_options.clear();
                self.model_error =
                    Some("No models are available for this provider account".to_owned());
            }
            Ok(options) => {
                if let Some(option) = self
                    .draft
                    .model_selection
                    .as_ref()
                    .and_then(|selection| {
                        options.iter().find(|option| option.id == selection.model)
                    })
                    .or_else(|| {
                        self.providers
                            .provider(&self.draft.provider)
                            .and_then(|provider| {
                                options
                                    .iter()
                                    .find(|option| option.id == provider.default_model)
                            })
                    })
                    .or_else(|| options.iter().find(|option| option.is_default))
                    .or_else(|| options.first())
                {
                    let effort = self
                        .draft
                        .model_selection
                        .as_ref()
                        .and_then(|selection| selection.reasoning_effort.as_ref())
                        .or_else(|| {
                            self.providers
                                .provider(&self.draft.provider)
                                .map(|provider| &provider.default_effort)
                        })
                        .filter(|effort| option.reasoning_efforts.contains(effort))
                        .cloned()
                        .or_else(|| option.default_reasoning_effort.clone())
                        .or_else(|| option.reasoning_efforts.first().cloned());
                    self.draft.model_selection = Some(bootty_agents::NativeModelSelection {
                        model: option.id.clone(),
                        reasoning_effort: effort,
                    });
                }
                self.model_options = options;
                self.model_error = None;
            }
            Err(error) => {
                self.model_options.clear();
                self.model_error = Some(error);
            }
        }
    }

    #[must_use]
    pub fn title(&self) -> String {
        if let Some(names) = &self.generated_names {
            return names.title.clone();
        }
        let text = match self.draft.mode {
            NewSessionMode::Agent => &self.draft.prompt,
            NewSessionMode::Terminal => &self.draft.command,
        };
        let title = text
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect::<String>()
            })
            .find(|line| !line.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{} in {}",
                    if self.draft.mode == NewSessionMode::Agent {
                        self.draft.provider.as_str()
                    } else {
                        "Terminal"
                    },
                    directory_name(&self.draft.cwd)
                )
            });
        bounded_title(&title)
    }

    /// # Errors
    /// Reports a missing selected naming account. No provider work starts while editing.
    pub fn naming_invocation(&self) -> Result<Option<CommandInvocation>, String> {
        let prompt = match self.draft.mode {
            NewSessionMode::Agent => &self.draft.prompt,
            NewSessionMode::Terminal => &self.draft.command,
        };
        if self.generated_names.is_some() || prompt.trim().is_empty() {
            return Ok(None);
        }
        let provider = &self.providers.codex;
        if !provider.enabled {
            return Err("Enable Codex to generate session and worktree names".into());
        }
        let selected = self
            .draft
            .profiles
            .get("codex")
            .map_or(provider.selected.as_str(), String::as_str);
        let profile = if selected.is_empty() {
            None
        } else {
            Some(
                provider
                    .profiles
                    .get(selected)
                    .ok_or("The naming account is unavailable")?,
            )
        };
        let arguments = profile.map_or_else(Vec::new, |profile| profile.arguments.clone());
        let mut invocation = CommandInvocation::new(
            "agents.native.names",
            vec![
                "codex".to_owned(),
                self.draft.cwd.clone(),
                provider.program.clone(),
                serde_json::to_string(&arguments).map_err(|error| error.to_string())?,
                String::new(),
                selected.to_owned(),
                prompt.chars().take(8192).collect(),
            ],
            Caller::Internal,
        );
        invocation.target = Some(
            self.destination()
                .ok_or("The selected host is unavailable")?
                .target
                .clone(),
        );
        Ok(Some(invocation))
    }

    /// # Errors
    /// Rejects provider output before it becomes a branch or directory name.
    pub fn set_generated_names(
        &mut self,
        names: bootty_agents::GeneratedSessionNames,
    ) -> Result<(), String> {
        names.validate()?;
        if self.draft.branch.is_empty() {
            self.draft.branch = format!("{}{}", self.branch_prefix(), names.slug);
        }
        if self.draft.folder.is_empty() {
            self.draft.folder = format!("{}-{}", directory_name(&self.draft.cwd), names.slug);
        }
        self.generated_names = Some(names);
        Ok(())
    }

    fn branch_prefix(&self) -> &str {
        self.project_settings()
            .map(|settings| settings.branch_prefix.as_str())
            .filter(|prefix| !prefix.is_empty())
            .unwrap_or("luan/")
    }

    /// The empty-Enter fallback opens a shell without consuming either retained text draft.
    /// # Errors
    /// Returns an unavailable destination or empty project directory.
    pub fn shell_invocation(&self, cwd: &str) -> Result<CommandInvocation, String> {
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
                bounded_title(&format!("Terminal in {}", directory_name(&self.draft.cwd))),
            ],
            Caller::Internal,
        );
        invocation.target = Some(destination.target.clone());
        Ok(invocation)
    }

    fn completion_scope(&self) -> Option<crate::CompletionScope> {
        if self.draft.mode != NewSessionMode::Agent {
            return None;
        }
        let mut catalog = self.invocation(&self.draft.cwd).ok()?;
        "agents.native.catalog-completions".clone_into(&mut catalog.command);
        catalog.arguments.truncate(6);
        let mut files = CommandInvocation::new(
            "files.complete",
            vec![self.draft.cwd.clone()],
            Caller::Internal,
        );
        files.target.clone_from(&catalog.target);
        Some(crate::CompletionScope {
            catalog,
            files,
            applications: true,
            remote: self
                .destination()
                .and_then(|destination| destination.remote.clone()),
        })
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

    #[must_use]
    pub fn spec(&self, busy: bool) -> DialogSpec {
        let command =
            self.draft.mode == NewSessionMode::Terminal && !self.draft.command.trim().is_empty();
        let text = if self.draft.mode == NewSessionMode::Agent {
            &self.draft.prompt
        } else {
            &self.draft.command
        };
        let mut spec = DialogSpec::prompt(
            NEW_SESSION_ID,
            "New session",
            text,
            if self.draft.mode == NewSessionMode::Agent {
                "What should the agent do?"
            } else {
                "Leave empty to open a shell"
            },
            DialogAction::new("start-session"),
        );
        spec.completion = self.completion_scope();
        spec.project_labels = self.project_labels();
        spec.multiline = true;
        spec.hint = None;
        if self.draft.mode == NewSessionMode::Agent {
            spec.model_error.clone_from(&self.model_error);
        }
        spec.text_label = Some(
            if self.draft.mode == NewSessionMode::Agent {
                "Prompt"
            } else if command {
                "Command"
            } else {
                "Terminal"
            }
            .to_owned(),
        );
        spec.busy = busy;
        if self.draft.mode == NewSessionMode::Agent {
            spec.applications.clone_from(&self.draft.applications);
            spec.attachments.clone_from(&self.draft.attachments);
            spec.models.clone_from(&self.model_options);
            spec.models_loading = self.models_loading;
            spec.selected_model = self
                .draft
                .model_selection
                .as_ref()
                .map(|selection| selection.model.clone());
        }
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
        spec.fields.extend(self.provider_fields());
        spec.fields.extend(self.model_fields());
        spec.fields.extend(self.worktree_fields());
        let destination_path = if self.draft.isolated {
            self.worktree_parent.as_ref().map_or_else(
                || self.draft.folder.clone(),
                |parent| format!("{parent}/{}", self.draft.folder),
            )
        } else {
            self.draft.cwd.clone()
        };
        spec.footer = Some(destination_path);
        if let Some(row) = spec.rows.first_mut() {
            (if busy { "Starting…" } else { "Start" }).clone_into(&mut row.label);
            row.detail = self
                .error
                .clone()
                .or_else(|| self.invocation(&self.draft.cwd).err());
            row.enabled = !busy
                && self.invocation(&self.draft.cwd).is_ok()
                && (!self.draft.isolated || self.worktree_available);
        }
        let mut project = DialogRow::action(
            "choose-project",
            format!("{}…", directory_name(&self.draft.cwd)),
            DialogAction::new("choose-project"),
        );
        project.enabled = !busy;
        spec.rows.push(project);
        spec
    }
    fn provider_fields(&self) -> Vec<DialogField> {
        let mut fields = Vec::new();
        if self.draft.mode == NewSessionMode::Agent {
            fields.push(choice(
                "provider",
                "Provider",
                provider_label(&self.draft.provider),
                bootty_agents::AgentKind::ALL
                    .into_iter()
                    .filter(|provider| {
                        bootty_agents::NativeSessionConfig::supports_provider(*provider)
                    })
                    .map(|provider| provider.to_string())
                    .filter(|id| {
                        self.providers
                            .provider(id)
                            .is_some_and(|provider| provider.enabled)
                    })
                    .map(|id| provider_label(&id).to_owned())
                    .collect(),
            ));
            if let Some(provider) = self.providers.provider(&self.draft.provider) {
                let selected = self
                    .draft
                    .profiles
                    .get(&self.draft.provider)
                    .map_or(provider.selected.as_str(), String::as_str);
                let value = provider.profiles.get(selected).map_or_else(
                    || "Default".to_owned(),
                    |profile| profile_label(selected, &profile.name),
                );
                let mut profiles = vec!["Default".to_owned()];
                profiles.extend(
                    provider
                        .profiles
                        .iter()
                        .filter(|(_, profile)| {
                            self.destination()
                                .is_none_or(|destination| destination.remote.is_none())
                                || profile.directory.is_none()
                        })
                        .map(|(id, profile)| profile_label(id, &profile.name)),
                );
                fields.push(choice("profile", "Account / profile", &value, profiles));
            }
        }
        fields
    }

    fn model_fields(&self) -> Vec<DialogField> {
        let mut fields = Vec::new();
        if self.draft.mode == NewSessionMode::Agent && !self.model_options.is_empty() {
            let selected = self
                .draft
                .model_selection
                .as_ref()
                .map(|selection| selection.model.as_str());
            let option = self.model_options.iter().find(|option| {
                selected.map_or(option.is_default, |selected| selected == option.id)
            });
            fields.push(choice(
                "model",
                "Model",
                &option.map_or_else(
                    || "Choose model".to_owned(),
                    |option| model_label(option, &self.model_options),
                ),
                self.model_options
                    .iter()
                    .map(|option| model_label(option, &self.model_options))
                    .collect(),
            ));
            if let Some(option) = option
                && !option.reasoning_efforts.is_empty()
            {
                let effort = self
                    .draft
                    .model_selection
                    .as_ref()
                    .and_then(|selection| selection.reasoning_effort.as_deref())
                    .or(option.default_reasoning_effort.as_deref())
                    .or_else(|| option.reasoning_efforts.first().map(String::as_str))
                    .unwrap_or("Choose effort");
                fields.push(choice(
                    "reasoning",
                    "Reasoning",
                    effort,
                    option.reasoning_efforts.clone(),
                ));
            }
        }
        if self.draft.mode == NewSessionMode::Agent {
            let provider = if self.draft.provider == "pi" {
                bootty_agents::AgentKind::Pi
            } else if self.draft.provider == "claude" {
                bootty_agents::AgentKind::Claude
            } else {
                bootty_agents::AgentKind::Codex
            };
            fields.push(choice(
                "permissions",
                "Permissions",
                self.permission_selection().label(),
                bootty_agents::NativePermissionMode::ALL
                    .into_iter()
                    .filter(|mode| mode.supports(provider))
                    .map(|mode| mode.label().to_owned())
                    .collect(),
            ));
        }
        fields
    }

    fn worktree_fields(&self) -> Vec<DialogField> {
        let mut fields = Vec::new();
        if self.worktree_available {
            fields.push(choice(
                "isolation",
                "Checkout",
                if self.draft.isolated {
                    "New worktree"
                } else {
                    "Current checkout"
                },
                vec!["Current checkout".to_owned(), "New worktree".to_owned()],
            ));
            if self.draft.isolated {
                for (id, label, value, placeholder) in [
                    (
                        "branch",
                        "Branch",
                        &self.draft.branch,
                        "Generated from prompt",
                    ),
                    (
                        "folder",
                        "Folder",
                        &self.draft.folder,
                        "Generated from prompt",
                    ),
                    (
                        "start-ref",
                        "Start from",
                        &self.draft.start_ref,
                        "HEAD, branch, tag or commit",
                    ),
                ] {
                    fields.push(DialogField {
                        id: id.to_owned(),
                        label: label.to_owned(),
                        value: value.clone(),
                        placeholder: placeholder.to_owned(),
                        kind: DialogFieldKind::Text,
                    });
                }
            }
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
fn profile_label(id: &str, name: &str) -> String {
    format!("{name} ({id})")
}

fn model_label(
    option: &bootty_agents::NativeModelOption,
    options: &[bootty_agents::NativeModelOption],
) -> String {
    if options
        .iter()
        .filter(|candidate| candidate.display_name == option.display_name)
        .count()
        > 1
    {
        format!("{} ({})", option.display_name, option.id)
    } else {
        option.display_name.clone()
    }
}

fn provider_label(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "claude" => "Claude",
        "pi" => "Pi",
        id => id,
    }
}
