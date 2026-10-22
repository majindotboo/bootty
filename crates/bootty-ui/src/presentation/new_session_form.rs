//! The retained new-session draft. Selecting a destination never starts work.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use bootty_config::config::{AgentProviderConfig, AgentProvidersConfig, RemoteConfig};
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
    pub preview: Option<crate::gpui_image_viewer::ImagePreview>,
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
    pub projectless: bool,
    pub prompt: String,
    pub attachments: Vec<NewSessionAttachment>,
    pub applications: Vec<bootty_agents::NativeApplicationMention>,
    pub provider_mentions: Vec<bootty_agents::NativeProviderMention>,
    pub goal: String,
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
    /// More models the prompt goes to, each starting its own session in its own worktree.
    pub fan_out: Vec<FanOutModel>,
}

/// A model a new session's prompt also goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FanOutModel {
    pub provider: String,
    pub model: String,
    /// What the model is called, for its chip.
    pub name: String,
}

impl FanOutModel {
    /// The `fan-out` field's wire form: provider, model and name, tab separated.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}\t{}\t{}", self.provider, self.model, self.name)
    }

    #[must_use]
    pub fn decode(value: &str) -> Option<Self> {
        let mut parts = value.splitn(3, '\t');
        let (provider, model) = (parts.next()?, parts.next()?);
        (!provider.is_empty() && !model.is_empty()).then(|| Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
            name: parts
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(model)
                .to_owned(),
        })
    }
}

pub struct NewSessionForm {
    pub draft: NewSessionDraft,
    destinations: Vec<SessionDestination>,
    providers: AgentProvidersConfig,
    pub worktree_available: bool,
    worktree_repository: Option<String>,
    worktree_directory: Option<String>,
    base_branches: Vec<String>,
    base_branch_error: Option<String>,
    /// The configured `session.branch-prefix`, for projects without their own.
    branch_prefix: String,
    pub error: Option<String>,
    pub model_options: Vec<bootty_agents::NativeModelOption>,
    pub model_error: Option<String>,
    pub models_loading: bool,
    pub provider_permissions: Option<bootty_agents::NativePermissionMode>,
    provider_catalogs: HashMap<String, Result<bootty_agents::NativeProviderCatalog, String>>,
    unavailable_providers: std::collections::HashSet<String>,
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
            worktree_repository: None,
            worktree_directory: None,
            base_branches: Vec::new(),
            base_branch_error: None,
            branch_prefix: bootty_config::config::DEFAULT_BRANCH_PREFIX.to_owned(),
            error: None,
            model_options: Vec::new(),
            model_error: None,
            models_loading: false,
            provider_permissions: None,
            provider_catalogs: HashMap::new(),
            unavailable_providers: std::collections::HashSet::new(),
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
        if form.draft.provider == "pi" {
            form.draft.permissions = bootty_agents::NativePermissionMode::ProviderDefault;
        }
        form.hydrate_model_catalog();
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
        self.hydrate_model_catalog();
        self.set_worktree_available(
            self.destination()
                .is_some_and(|destination| destination.worktrees)
                && worktrees.iter().any(|entry| entry.is_new),
        );
    }

    pub const fn set_worktree_available(&mut self, available: bool) {
        self.worktree_available = available && !self.draft.projectless;
        if !available {
            self.draft.isolated = false;
        }
    }

    #[must_use]
    pub fn base_branch_invocation(&self) -> Option<CommandInvocation> {
        if !self.worktree_available || !self.draft.isolated {
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

    pub fn set_base_branches_for(
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
                if matches!(self.draft.start_ref.as_str(), "" | "HEAD")
                    && let Some(current) = overview.branches.iter().find(|branch| branch.current)
                {
                    self.draft.start_ref.clone_from(&current.name);
                }
                self.base_branches = overview
                    .branches
                    .into_iter()
                    .map(|branch| branch.name)
                    .collect();
                self.base_branch_error = None;
            }
            Err(error) => self.base_branch_error = Some(error),
        }
    }

    pub fn set_directory(&mut self, cwd: String) {
        self.draft.projectless = false;
        self.update_directory(cwd);
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
        self.set_directory(cwd);
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
        self.base_branches.clear();
        self.base_branch_error = None;
        self.worktree_available = false;
        self.worktree_repository = None;
        self.error = None;
        self.apply_project_defaults();
        self.hydrate_model_catalog();
    }

    pub fn set_project_defaults(
        &mut self,
        projects: Vec<bootty_mux::repository::RegisteredProject>,
    ) {
        self.project_defaults = projects;
        self.apply_project_defaults();
        self.hydrate_model_catalog();
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
                self.worktree_repository = None;
                self.base_branches.clear();
                self.base_branch_error = None;
                if !destination.worktrees {
                    self.draft.isolated = false;
                }
                self.hydrate_model_catalog();
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
            "model" | "reasoning" | "fast-mode" | "plan-mode" => {
                self.change_model_field(field, value);
            }
            "goal" => value.trim().clone_into(&mut self.draft.goal),
            "fan-out" => self.toggle_fan_out(value),
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
        if matches!(field, "provider" | "profile") {
            self.hydrate_model_catalog();
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
        if !self.provider_unavailable(provider)
            && bootty_agents::AgentKind::ALL.into_iter().any(|kind| {
                kind.to_string() == provider
                    && bootty_agents::NativeSessionConfig::supports_provider(kind)
            })
            && self
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

    /// Sends the prompt to one more model, or stops sending it there.
    fn toggle_fan_out(&mut self, value: &str) {
        let Some(model) = FanOutModel::decode(value) else {
            return;
        };
        if let Some(at) = self
            .draft
            .fan_out
            .iter()
            .position(|chosen| chosen.provider == model.provider && chosen.model == model.model)
        {
            self.draft.fan_out.remove(at);
        } else {
            self.draft.fan_out.push(model);
        }
    }

    fn change_plan_mode(&mut self, value: &str, fast: Option<bool>) {
        if let Ok(plan) = value.parse::<bool>() {
            if let Some(selection) = &mut self.draft.model_selection {
                selection.plan_mode = Some(plan);
            } else if let Some(model) = self.model_options.iter().find(|model| model.is_default) {
                self.draft.model_selection = Some(bootty_agents::NativeModelSelection {
                    plan_mode: Some(plan),
                    fast_mode: fast,
                    model: model.id.clone(),
                    reasoning_effort: model.default_reasoning_effort.clone(),
                });
            }
        }
    }

    fn change_model_field(&mut self, field: &str, value: &str) {
        let available = self
            .providers
            .provider(&self.draft.provider)
            .map_or(&[][..], |provider| provider.available_efforts.as_slice());
        let fast = self
            .draft
            .model_selection
            .as_ref()
            .and_then(|selection| selection.fast_mode);
        let plan = self
            .draft
            .model_selection
            .as_ref()
            .and_then(|selection| selection.plan_mode);
        match field {
            "plan-mode" => self.change_plan_mode(value, fast),
            "fast-mode" => {
                if self.draft.provider != "pi"
                    && let Ok(fast) = value.parse::<bool>()
                    && (!fast
                        || self
                            .model_options
                            .iter()
                            .find(|model| {
                                self.draft
                                    .model_selection
                                    .as_ref()
                                    .map_or(model.is_default, |selection| {
                                        selection.model == model.id
                                    })
                            })
                            .is_some_and(|model| model.supports_fast_mode))
                {
                    if self.draft.model_selection.is_none()
                        && let Some(model) =
                            self.model_options.iter().find(|model| model.is_default)
                    {
                        self.draft.model_selection = Some(bootty_agents::NativeModelSelection {
                            plan_mode: plan,
                            model: model.id.clone(),
                            reasoning_effort: model.default_reasoning_effort.clone(),
                            fast_mode: Some(fast),
                        });
                    } else if let Some(selection) = &mut self.draft.model_selection {
                        selection.fast_mode = Some(fast);
                    }
                }
            }
            "model" => {
                if let Some(option) = self.model_options.iter().find(|option| {
                    option.id == value || model_label(option, &self.model_options) == value
                }) {
                    self.draft.model_selection = Some(bootty_agents::NativeModelSelection {
                        plan_mode: plan,
                        fast_mode: fast.map(|fast| fast && option.supports_fast_mode),
                        model: option.id.clone(),
                        reasoning_effort: option
                            .default_reasoning_effort
                            .clone()
                            .filter(|effort| {
                                option.reasoning_efforts.contains(effort)
                                    && (available.is_empty() || available.contains(effort))
                            })
                            .or_else(|| {
                                option
                                    .reasoning_efforts
                                    .iter()
                                    .find(|effort| {
                                        available.is_empty() || available.contains(effort)
                                    })
                                    .cloned()
                            }),
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
                        plan_mode: plan,
                        fast_mode: fast,
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
            if self.draft.folder == names.slug {
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
            directory: None,
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
                } else {
                    shell_argv(&self.draft.command, destination.remote.is_some())
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
                if self.provider_unavailable(&self.draft.provider) {
                    return Err("Install a provider or choose an installed provider".to_owned());
                }
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
        if self.draft.projectless {
            let index: usize = if self.draft.mode == NewSessionMode::Agent {
                15
            } else {
                5
            };
            invocation.arguments.resize(
                invocation.arguments.len().max(index.saturating_add(1)),
                String::new(),
            );
            if let Some(projectless) = invocation.arguments.get_mut(index) {
                "true".clone_into(projectless);
            }
        }
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
            && self.draft.provider != "pi"
            && self.draft.permissions != bootty_agents::NativePermissionMode::ProviderDefault
        {
            invocation.arguments.resize(14, String::new());
            invocation
                .arguments
                .push(self.draft.permissions.id().to_owned());
        }
        if self.draft.mode == NewSessionMode::Agent && !self.draft.provider_mentions.is_empty() {
            invocation.arguments.resize(17, String::new());
            invocation.arguments.push(
                serde_json::to_string(&self.draft.provider_mentions)
                    .map_err(|error| error.to_string())?,
            );
        }
        if self.draft.mode == NewSessionMode::Agent
            && self.draft.provider == "codex"
            && !self.draft.goal.is_empty()
        {
            invocation.arguments.resize(18, String::new());
            invocation.arguments.push(self.draft.goal.clone());
        }

        Ok(())
    }

    #[must_use]
    pub fn model_catalog_invocation(&self) -> Option<CommandInvocation> {
        self.model_catalog_invocation_for(&self.draft.provider)
    }

    #[must_use]
    pub fn model_catalog_invocation_for(&self, provider_id: &str) -> Option<CommandInvocation> {
        if self.draft.mode != NewSessionMode::Agent || self.draft.cwd.is_empty() {
            return None;
        }
        let destination = self.destination()?;
        let provider = self
            .providers
            .provider(provider_id)
            .filter(|provider| provider.enabled)?;
        let selected = self
            .draft
            .profiles
            .get(provider_id)
            .map_or(provider.selected.as_str(), String::as_str);
        let profile = if selected.is_empty() {
            None
        } else {
            Some(provider.profiles.get(selected)?)
        };
        let arguments = profile.map_or_else(Vec::new, |profile| profile.arguments.clone());
        let mut invocation = CommandInvocation::new(
            "agents.native.catalog-info",
            vec![
                provider_id.to_owned(),
                self.draft.cwd.clone(),
                provider.program.clone(),
                serde_json::to_string(&arguments).ok()?,
                String::new(),
                selected.to_owned(),
            ],
            Caller::Internal,
        );
        invocation.target = Some(destination.target.clone());
        Some(invocation)
    }

    /// Current provider first, followed by every enabled native provider for this exact target.
    #[must_use]
    pub fn model_catalog_invocations(&self) -> Vec<CommandInvocation> {
        let mut providers = bootty_agents::AgentKind::ALL
            .into_iter()
            .filter(|kind| {
                bootty_agents::NativeSessionConfig::supports_provider(*kind)
                    && self
                        .providers
                        .provider(&kind.to_string())
                        .is_some_and(|provider| provider.enabled)
            })
            .map(|kind| kind.to_string())
            .collect::<Vec<_>>();
        providers.sort_by_key(|provider| provider != &self.draft.provider);
        providers
            .iter()
            .filter_map(|provider| self.model_catalog_invocation_for(provider))
            .collect()
    }

    #[must_use]
    pub fn catalog_provider_preferences(&self, provider_id: &str) -> Option<&AgentProviderConfig> {
        self.providers.provider(provider_id)
    }

    #[must_use]
    pub fn cached_provider_catalog(
        &self,
        invocation: &CommandInvocation,
    ) -> Option<Result<bootty_agents::NativeProviderCatalog, String>> {
        self.provider_catalogs
            .get(&catalog_invocation_key(invocation)?)
            .cloned()
    }

    pub fn set_provider_catalog_for(
        &mut self,
        invocation: &CommandInvocation,
        catalog: Result<bootty_agents::NativeProviderCatalog, String>,
    ) {
        let key = catalog_invocation_key(invocation);
        if let Some(key) = &key {
            self.unavailable_providers.remove(key);
        }
        if let Err(error) = &catalog
            && matches!(self.cached_provider_catalog(invocation), Some(Ok(_)))
        {
            if self
                .model_catalog_invocation()
                .as_ref()
                .and_then(catalog_invocation_key)
                == key
            {
                self.model_error = Some(error.clone());
            }
            return;
        }
        if let Some(key) = &key {
            self.provider_catalogs.insert(key.clone(), catalog.clone());
        }
        let current_key = self
            .model_catalog_invocation()
            .as_ref()
            .and_then(catalog_invocation_key);
        if current_key == key {
            self.apply_provider_catalog(catalog);
        }
    }

    /// A missing executable is different from an installed provider needing sign-in.
    pub fn set_provider_unavailable_for(
        &mut self,
        invocation: &CommandInvocation,
        message: String,
    ) {
        let Some(key) = catalog_invocation_key(invocation) else {
            return;
        };
        self.provider_catalogs.remove(&key);
        self.set_provider_catalog_for(invocation, Err(message));
        self.unavailable_providers.insert(key);
        if self.provider_unavailable(&self.draft.provider) {
            let replacement = self
                .model_catalog_invocations()
                .into_iter()
                .find_map(|candidate| {
                    let provider = candidate.arguments.first()?;
                    (!self.provider_unavailable(provider)).then(|| provider.clone())
                });
            if let Some(provider) = replacement {
                self.change_provider(&provider);
                self.hydrate_model_catalog();
            }
        }
    }

    fn provider_unavailable(&self, provider: &str) -> bool {
        self.model_catalog_invocation_for(provider)
            .and_then(|invocation| catalog_invocation_key(&invocation))
            .is_some_and(|key| self.unavailable_providers.contains(&key))
    }

    /// Open the selected account's own login in the captured Space, preserving the draft.
    #[must_use]
    pub fn sign_in_invocation(&self) -> Option<CommandInvocation> {
        if !self
            .model_error
            .as_deref()
            .into_iter()
            .chain(self.error.as_deref())
            .any(crate::error_catalog::needs_sign_in)
        {
            return None;
        }
        let catalog = self.model_catalog_invocation()?;
        let provider = catalog.arguments.first()?;
        let mut invocation = CommandInvocation::new(
            format!("agents.{provider}.account.login"),
            vec![
                catalog.arguments.get(1)?.clone(),
                catalog.arguments.get(2)?.clone(),
                catalog.arguments.get(5)?.clone(),
            ],
            Caller::Internal,
        );
        invocation.target = catalog.target;
        Some(invocation)
    }

    pub fn set_favorite_models_for(
        &mut self,
        invocation: &CommandInvocation,
        models: Vec<bootty_agents::NativeModelOption>,
    ) {
        let Some(key) = catalog_invocation_key(invocation) else {
            return;
        };
        if let Some(Ok(catalog)) = self.provider_catalogs.get_mut(&key) {
            catalog.models = models;
            let catalog = catalog.clone();
            let current_key = self
                .model_catalog_invocation()
                .as_ref()
                .and_then(catalog_invocation_key);
            if current_key.as_deref() == Some(key.as_str()) {
                self.apply_provider_catalog(Ok(catalog));
            }
        }
    }

    pub fn hydrate_model_catalog(&mut self) {
        let Some(invocation) = self.model_catalog_invocation() else {
            self.model_options.clear();
            self.provider_permissions = None;
            self.model_error = None;
            self.models_loading = false;
            return;
        };
        if let Some(catalog) = self.cached_provider_catalog(&invocation) {
            self.apply_provider_catalog(catalog);
        } else {
            self.model_options.clear();
            self.provider_permissions = None;
            self.model_error = None;
            self.models_loading = true;
        }
    }

    const fn reset_provider_permissions(&mut self) {
        self.draft.permissions = bootty_agents::NativePermissionMode::ProviderDefault;
        self.provider_permissions = None;
    }

    /// Display the selected account policy while leaving inherited launch values untouched.
    #[must_use]
    pub fn permission_selection(&self) -> bootty_agents::NativePermissionMode {
        if self.draft.provider == "pi" {
            bootty_agents::NativePermissionMode::ProviderDefault
        } else if self.draft.permissions == bootty_agents::NativePermissionMode::ProviderDefault {
            self.provider_permissions.unwrap_or(self.draft.permissions)
        } else {
            self.draft.permissions
        }
    }

    pub fn set_provider_catalog(
        &mut self,
        catalog: Result<bootty_agents::NativeProviderCatalog, String>,
    ) {
        if let Some(invocation) = self.model_catalog_invocation() {
            self.set_provider_catalog_for(&invocation, catalog);
        } else {
            self.apply_provider_catalog(catalog);
        }
    }

    fn apply_provider_catalog(
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
                    let available = self
                        .providers
                        .provider(&self.draft.provider)
                        .map_or(&[][..], |provider| provider.available_efforts.as_slice());
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
                        .filter(|effort| {
                            option.reasoning_efforts.contains(effort)
                                && (available.is_empty() || available.contains(effort))
                        })
                        .cloned()
                        .or_else(|| {
                            option
                                .default_reasoning_effort
                                .clone()
                                .filter(|effort| available.is_empty() || available.contains(effort))
                        })
                        .or_else(|| {
                            option
                                .reasoning_efforts
                                .iter()
                                .find(|effort| available.is_empty() || available.contains(effort))
                                .cloned()
                        });
                    self.draft.model_selection = Some(bootty_agents::NativeModelSelection {
                        plan_mode: self
                            .draft
                            .model_selection
                            .as_ref()
                            .and_then(|selection| selection.plan_mode),
                        fast_mode: self
                            .draft
                            .model_selection
                            .as_ref()
                            .and_then(|selection| selection.fast_mode)
                            .or_else(|| {
                                self.providers
                                    .provider(&self.draft.provider)
                                    .map(|provider| provider.fast_mode)
                            })
                            .map(|fast| fast && option.supports_fast_mode),
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
        if self.generated_names.is_some()
            || prompt.trim().is_empty()
            || (self.draft.isolated && !self.draft.branch.trim().is_empty())
        {
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
            self.draft.folder.clone_from(&names.slug);
        }
        self.generated_names = Some(names);
        Ok(())
    }

    fn branch_prefix(&self) -> &str {
        self.project_settings()
            .map(|settings| settings.branch_prefix.as_str())
            .filter(|prefix| !prefix.is_empty())
            .unwrap_or(&self.branch_prefix)
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
                self.draft.projectless.to_string(),
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
            commands: false,
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

    fn catalog_error(&self) -> Option<String> {
        self.error
            .as_ref()
            .filter(|error| crate::error_catalog::needs_sign_in(error))
            .cloned()
            .or_else(|| self.model_error.clone())
    }

    fn submission_error(&self) -> Option<String> {
        self.error
            .as_ref()
            .filter(|error| {
                self.draft.mode != NewSessionMode::Agent
                    || !crate::error_catalog::needs_sign_in(error)
            })
            .cloned()
            .or_else(|| self.invocation(&self.draft.cwd).err())
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
        spec.selected_project = Some(if self.draft.projectless {
            String::new()
        } else {
            self.project_path().to_owned()
        });
        spec.multiline = true;
        spec.hint = self.worktree_hint();
        if self.draft.mode == NewSessionMode::Agent {
            spec.model_error = self.catalog_error();
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
            spec.provider_mentions
                .clone_from(&self.draft.provider_mentions);
            spec.attachments.clone_from(&self.draft.attachments);
            spec.models.clone_from(&self.model_options);
            spec.available_efforts = self
                .providers
                .provider(&self.draft.provider)
                .map_or_else(Vec::new, |provider| provider.available_efforts.clone());
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
        if !self.draft.isolated {
            return None;
        }
        Some(self.base_branch_error.as_ref().map_or_else(
            || format!("Source checkout: {}", self.draft.cwd),
            |error| {
                format!(
                    "Could not load base branches: {error}. Search for a ref to use it directly."
                )
            },
        ))
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
        if self.draft.isolated {
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
            let folder = if self.draft.folder.is_empty() {
                if self.draft.branch.is_empty() {
                    "<generated-name>".to_owned()
                } else {
                    self.draft.branch.replace('/', "-")
                }
            } else {
                self.draft.folder.clone()
            };
            format!("{directory}/{repository}/{folder}")
        } else {
            self.draft.cwd.clone()
        }
    }
    fn provider_fields(&self) -> Vec<DialogField> {
        let mut fields = Vec::new();
        if self.draft.mode == NewSessionMode::Agent {
            fields.push(choice(
                "provider",
                "Provider",
                if self.provider_unavailable(&self.draft.provider) {
                    "Choose provider"
                } else {
                    provider_label(&self.draft.provider)
                },
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
                    .filter(|id| !self.provider_unavailable(id))
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

    /// The other models the prompt goes to, one per line.
    fn fan_out_field(&self) -> DialogField {
        DialogField {
            id: "fan-out".into(),
            label: "Also send to".into(),
            value: self
                .draft
                .fan_out
                .iter()
                .map(FanOutModel::encode)
                .collect::<Vec<_>>()
                .join("\n"),
            placeholder: String::new(),
            kind: DialogFieldKind::Text,
        }
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
        if self.draft.mode == NewSessionMode::Agent && self.draft.provider == "codex" {
            fields.push(DialogField {
                id: "goal".into(),
                label: "Goal".into(),
                value: self.draft.goal.clone(),
                placeholder: String::new(),
                kind: DialogFieldKind::Text,
            });
        }
        if self.draft.mode == NewSessionMode::Agent {
            fields.push(choice(
                "plan-mode",
                "Plan mode",
                &self
                    .draft
                    .model_selection
                    .as_ref()
                    .and_then(|selection| selection.plan_mode)
                    .unwrap_or(false)
                    .to_string(),
                vec!["false".into(), "true".into()],
            ));
            fields.push(self.fan_out_field());
        }
        if self.draft.mode == NewSessionMode::Agent && self.draft.provider != "pi" {
            let fast = self
                .draft
                .model_selection
                .as_ref()
                .and_then(|selection| selection.fast_mode)
                .unwrap_or_else(|| {
                    self.providers
                        .provider(&self.draft.provider)
                        .is_some_and(|provider| provider.fast_mode)
                });
            fields.push(choice(
                "fast-mode",
                "Fast mode",
                &fast.to_string(),
                vec!["false".into(), "true".into()],
            ));
            let provider = if self.draft.provider == "claude" {
                bootty_agents::AgentKind::Claude
            } else {
                bootty_agents::AgentKind::Codex
            };
            fields.push(choice(
                "permissions",
                "Permissions",
                self.permission_selection().label(),
                bootty_agents::NativePermissionMode::choices(provider)
                    .iter()
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
                let mut branches = self.base_branches.clone();
                if !self.draft.start_ref.is_empty()
                    && self.draft.start_ref != "HEAD"
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

fn catalog_invocation_key(invocation: &CommandInvocation) -> Option<String> {
    let mut identity = invocation.clone();
    if identity.command == "agents.native.catalog-info"
        && identity
            .arguments
            .get(6)
            .is_some_and(|argument| argument == "refresh")
    {
        identity.arguments.truncate(6);
    }
    serde_json::to_string(&identity).ok()
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

/// The argv that runs a shell `command` on a host: PowerShell on this Windows computer, a login
/// shell elsewhere.
#[must_use]
pub fn shell_argv(command: &str, remote: bool) -> Vec<String> {
    if cfg!(windows) && !remote {
        vec![
            "powershell.exe".to_owned(),
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            command.to_owned(),
        ]
    } else {
        vec!["/bin/sh".to_owned(), "-lc".to_owned(), command.to_owned()]
    }
}
