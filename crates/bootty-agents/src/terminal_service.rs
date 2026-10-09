use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bootty_control::{Caller, CommandTarget, ResourceKind};
use bootty_write::{CommitOutcome, NewFileMode, WriteTarget};
use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, AgentLaunch,
    terminal_observation::{AgentObservation, ObservationSink, TerminalAgentStatus},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TerminalAgentLocation {
    pub task_identity: String,
    pub window_id: String,
    pub pane_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalAgentRecord {
    pub provider: AgentKind,
    pub target: CommandTarget,
    pub binding_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<TerminalAgentLocation>,
    pub launch: AgentLaunch,
    pub observation: AgentObservation,
}

impl TerminalAgentRecord {
    /// Build a provider-native resume from retained reusable settings, never saved prompts.
    /// # Errors
    /// Rejects stopped/ephemeral agents, missing account provenance or unsupported session IDs.
    pub fn recovery_launch(&self) -> Result<AgentLaunch, String> {
        if self.observation.status == TerminalAgentStatus::Stopped {
            return Err("The retained terminal agent was stopped".to_owned());
        }
        if self.launch.account_directory.is_none() {
            return Err("Terminal agent recovery needs its captured account directory".to_owned());
        }
        let session = if self.provider == AgentKind::Pi {
            self.observation
                .session_file
                .as_ref()
                .or(self.observation.session_id.as_ref())
        } else {
            self.observation.session_id.as_ref()
        }
        .ok_or("The terminal agent has no saved native provider session")?;
        let mut launch = self.launch.retained(self.provider);
        launch.arguments = launch.session_arguments(self.provider, session, false)?;
        launch.validate()?;
        Ok(launch)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalAgentActivity {
    pub provider: AgentKind,
    pub target: CommandTarget,
    pub status: TerminalAgentStatus,
    pub session_id: Option<String>,
    pub detail: Option<String>,
    /// Time since this live observation entered `Working`; never restored from the catalog.
    pub working_elapsed: Option<Duration>,
}

/// Exact captured parent configuration; native conversations do not invent terminal observations.
#[derive(Clone)]
pub struct ToolSpawnParent {
    pub binding_id: String,
    pub launch: AgentLaunch,
}

struct ToolAttachment {
    tools: Weak<crate::ToolBridge>,
    parent: Option<ToolSpawnParent>,
}

struct ObservationLease {
    target: Option<CommandTarget>,
    latest: AgentObservation,
    working_since: Option<Instant>,
}

enum TerminalObserver {
    Claude(crate::claude_terminal::ClaudeTerminalObserver),
    Codex(crate::codex_terminal::CodexTerminalObserver),
    Pi(crate::pi_terminal::PiTerminalObserver),
    Unobserved(Vec<String>),
}

impl TerminalObserver {
    fn arguments(&self) -> Vec<String> {
        match self {
            Self::Claude(observer) => observer.arguments(),
            Self::Codex(observer) => observer.arguments(),
            Self::Pi(observer) => observer.arguments(),
            Self::Unobserved(arguments) => arguments.clone(),
        }
    }

    fn stop(&self) {
        match self {
            Self::Claude(observer) => observer.stop(),
            Self::Codex(observer) => observer.stop(),
            Self::Pi(observer) => observer.stop(),
            Self::Unobserved(_) => {}
        }
    }

    fn stop_and_wait(&mut self) -> Result<(), String> {
        match self {
            Self::Claude(observer) => observer.stop_and_wait(),
            Self::Codex(observer) => observer.stop_and_wait(),
            Self::Pi(observer) => observer.stop_and_wait(),
            Self::Unobserved(_) => Ok(()),
        }
    }
}

pub struct PreparedTerminalAgent {
    pub launch: AgentLaunch,
    provider: AgentKind,
    lease: Arc<Mutex<ObservationLease>>,
    observer: TerminalObserver,
    tools: Option<Arc<crate::ToolBridge>>,
}

impl PreparedTerminalAgent {
    /// A revoked pending tool attachment must not be launched again by recovery.
    #[must_use]
    pub fn tools_enabled(&self) -> bool {
        self.tools
            .as_ref()
            .is_none_or(|tools| tools.lease().enabled(None))
    }

    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(directory) = &self.launch.account_directory {
            argv.extend([
                "env".to_owned(),
                format!("{}={directory}", self.provider.account_directory_variable()),
            ]);
        }
        argv.push(self.launch.program.clone());
        argv.extend(self.observer.arguments());
        argv
    }
}

/// Private launch preparation is consumed by its exact cold terminal destination once.
pub struct PreparedTerminalRestore {
    pub prepared: PreparedTerminalAgent,
    pub source: TerminalAgentRecord,
    pub target: CommandTarget,
}

struct LiveObservation {
    target: CommandTarget,
    lease: Arc<Mutex<ObservationLease>>,
    observer: TerminalObserver,
    tools: Option<Arc<crate::ToolBridge>>,
}

impl LiveObservation {
    fn stop_tools(&self) {
        if let Some(tools) = &self.tools {
            tools.stop();
        }
    }
}

struct ProviderInspection {
    request: Arc<()>,
    provider: AgentKind,
    program: String,
    directory: Option<String>,
    pi_selector: Option<crate::PiAccountSelector>,
    status: crate::TerminalProviderStatus,
}

#[derive(Default)]
struct ToolAttachmentPolicy {
    disabled: Vec<AgentKind>,
    spawn_enabled: bool,
    computer_capture_enabled: bool,
}

/// Owns only native observation and retained metadata. Backend panes own the interactive TUIs.
pub struct TerminalAgentService {
    path: PathBuf,
    retiring: Mutex<std::collections::BTreeSet<(String, u64)>>,
    retirement_error: Mutex<Option<String>>,
    runtime_directory: PathBuf,
    mutation: Mutex<()>,
    records: Mutex<Vec<TerminalAgentRecord>>,
    restores: Mutex<std::collections::HashMap<String, PreparedTerminalRestore>>,
    live: Mutex<Vec<LiveObservation>>,
    disabled_tools: Mutex<ToolAttachmentPolicy>,
    tools: Mutex<Vec<ToolAttachment>>,
    revision: AtomicU64,
    provider_status: Mutex<Vec<ProviderInspection>>,
    change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl TerminalAgentService {
    /// # Errors
    /// Returns malformed, oversized or invalid retained catalog errors.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        Self::open_with_clock(path, Arc::new(Instant::now))
    }

    /// Open the native observation owner with an injected monotonic clock.
    /// # Errors
    /// Returns malformed, oversized or invalid retained catalog errors.
    pub fn open_with_clock(
        path: impl Into<PathBuf>,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Result<Self, String> {
        let path = path.into();
        let records = match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > 1024 * 1024 => {
                return Err("Terminal agent catalog exceeds 1 MiB".to_owned());
            }
            Ok(_) => serde_json::from_slice::<Vec<TerminalAgentRecord>>(
                &fs::read(&path).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        if records.len() > 128 {
            return Err("Terminal agent catalog exceeds 128 entries".to_owned());
        }
        let mut restored = Vec::new();
        for mut record in records {
            validate_record(&record)?;
            if record.observation.status != TerminalAgentStatus::Stopped {
                record.observation.status = TerminalAgentStatus::Unavailable;
            }
            restored.push(record);
        }
        let identity = crate::terminal_observation::terminal_session_id()?;
        let runtime_directory = std::env::temp_dir().join(format!(
            "bt-{}",
            identity.chars().take(18).collect::<String>()
        ));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&runtime_directory)
                .map_err(|error| error.to_string())?;
        }
        #[cfg(not(unix))]
        fs::create_dir(&runtime_directory).map_err(|error| error.to_string())?;
        Ok(Self {
            path,
            retiring: Mutex::new(std::collections::BTreeSet::new()),
            retirement_error: Mutex::new(None),
            runtime_directory,
            mutation: Mutex::new(()),
            records: Mutex::new(restored),
            restores: Mutex::new(std::collections::HashMap::new()),
            live: Mutex::new(Vec::new()),
            disabled_tools: Mutex::new(ToolAttachmentPolicy::default()),
            tools: Mutex::new(Vec::new()),
            revision: AtomicU64::new(1),
            provider_status: Mutex::new(Vec::new()),
            change: Mutex::new(None),
            clock,
        })
    }

    #[must_use]
    pub fn provider_status(
        &self,
        provider: AgentKind,
        program: &str,
        directory: Option<&str>,
        model_provider: Option<&str>,
    ) -> Option<crate::TerminalProviderStatus> {
        let selector = model_provider.map(crate::PiAccountSelector::provider);
        self.provider_status_with_pi_selector(provider, program, directory, selector.as_ref())
    }

    #[must_use]
    pub fn provider_status_with_pi_selector(
        &self,
        provider: AgentKind,
        program: &str,
        directory: Option<&str>,
        selector: Option<&crate::PiAccountSelector>,
    ) -> Option<crate::TerminalProviderStatus> {
        self.provider_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|entry| {
                entry.provider == provider
                    && entry.program == program
                    && entry.directory.as_deref() == directory
                    && entry.pi_selector.as_ref() == selector
            })
            .map(|entry| entry.status.clone())
    }

    fn begin_provider_status(&self, entry: ProviderInspection) {
        let mut statuses = self
            .provider_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        statuses.retain(|existing| existing.provider != entry.provider);
        statuses.push(entry);
        drop(statuses);
        self.changed();
    }

    fn publish_provider_status(
        &self,
        provider: AgentKind,
        request: &Arc<()>,
        status: crate::TerminalProviderStatus,
    ) {
        let mut statuses = self
            .provider_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = statuses
            .iter_mut()
            .find(|entry| entry.provider == provider && Arc::ptr_eq(&entry.request, request))
        else {
            // A newer inspection owns this provider's published status.
            return;
        };
        entry.status = status;
        drop(statuses);
        self.changed();
    }

    pub fn inspect_provider(
        &self,
        provider: AgentKind,
        program: &str,
        directory: Option<&str>,
        model_provider: Option<&str>,
    ) -> crate::TerminalProviderStatus {
        let selector = model_provider.map(crate::PiAccountSelector::provider);
        self.inspect_provider_with_pi_selector(provider, program, directory, selector.as_ref())
    }

    pub fn inspect_provider_with_pi_selector(
        &self,
        provider: AgentKind,
        program: &str,
        directory: Option<&str>,
        selector: Option<&crate::PiAccountSelector>,
    ) -> crate::TerminalProviderStatus {
        let request = Arc::new(());
        self.begin_provider_status(ProviderInspection {
            request: Arc::clone(&request),
            provider,
            program: program.to_owned(),
            directory: directory.map(str::to_owned),
            pi_selector: selector.cloned(),
            status: crate::TerminalProviderStatus {
                executable: None,
                version: None,
                installer: None,
                authenticated: None,
                account: None,
                auth_method: None,
                subscription: None,
                message: Some("Checking…".to_owned()),
            },
        });
        let status = crate::terminal_provider_status_with_pi_selector(
            provider, program, directory, selector,
        );
        self.publish_provider_status(provider, &request, status.clone());
        status
    }

    #[must_use]
    pub fn has_live_provider(&self, provider: AgentKind) -> bool {
        let records = self.records();
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|live| {
                records
                    .iter()
                    .any(|record| record.provider == provider && record.target == live.target)
            })
    }

    #[must_use]
    pub fn live_session(
        &self,
        provider: AgentKind,
        session: &str,
        binding: &str,
        directory: Option<&str>,
    ) -> Option<CommandTarget> {
        let records = self.records();
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find_map(|live| {
                records
                    .iter()
                    .find(|record| {
                        record.target == live.target
                            && record.provider == provider
                            && record.binding_id == binding
                            && record.launch.account_directory.as_deref() == directory
                            && (record.observation.session_id.as_deref() == Some(session)
                                || record.observation.session_file.as_deref() == Some(session))
                    })
                    .map(|record| record.target.clone())
            })
    }

    #[must_use]
    pub fn live_records(&self) -> Vec<TerminalAgentRecord> {
        let records = self.records();
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        records
            .into_iter()
            .filter(|record| live.iter().any(|observer| observer.target == record.target))
            .collect()
    }

    pub fn retire_closed(self: &Arc<Self>, target: CommandTarget) {
        self.revoke_terminal_tools(&target);
        let key = (target.handle.clone(), target.generation);
        if !self
            .retiring
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone())
        {
            return;
        }
        let service = Arc::clone(self);
        std::thread::spawn(move || match service.retire(&target) {
            Ok(_) => {
                service
                    .retiring
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&key);
            }
            Err(error) => {
                *service
                    .retirement_error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(format!(
                    "Terminal closed, but observation retirement failed: {error}"
                ));
                service.changed();
            }
        });
    }

    #[must_use]
    pub fn take_retirement_error(&self) -> Option<String> {
        self.retirement_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub fn set_change_handler(&self, handler: Arc<dyn Fn() + Send + Sync>) {
        *self.change.lock().unwrap_or_else(PoisonError::into_inner) = Some(handler);
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn records(&self) -> Vec<TerminalAgentRecord> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn record(&self, target: &CommandTarget) -> Option<TerminalAgentRecord> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|record| record.target == *target)
            .cloned()
    }

    #[must_use]
    pub fn activity(&self, target: &CommandTarget) -> Option<TerminalAgentActivity> {
        self.record(target).map(|record| {
            let working_elapsed = if record.observation.status == TerminalAgentStatus::Working {
                self.live
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .find(|live| live.target == *target)
                    .and_then(|live| {
                        let lease = live.lease.lock().unwrap_or_else(PoisonError::into_inner);
                        lease.working_since
                    })
                    .map(|started| (self.clock)().saturating_duration_since(started))
            } else {
                None
            };
            TerminalAgentActivity {
                provider: record.provider,
                target: record.target,
                status: record.observation.status,
                session_id: record.observation.session_id,
                detail: record.observation.detail,
                working_elapsed,
            }
        })
    }

    /// Spawn authority comes only from the exact live parent's private bridge.
    #[must_use]
    pub fn spawn_parent(
        &self,
        target: &CommandTarget,
    ) -> Option<(TerminalAgentRecord, crate::ToolLease)> {
        let tools = self
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|observer| observer.target == *target)
            .and_then(|observer| observer.tools.as_ref())
            .map(|tools| tools.lease().clone())?;
        Some((self.record(target)?, tools))
    }

    /// Resolve the private bridge's attachment, never another agent sharing its task terminal.
    #[must_use]
    pub fn spawn_parent_for_attachment(
        &self,
        target: &CommandTarget,
        attachment_id: u64,
        caller: Caller,
    ) -> Option<(ToolSpawnParent, crate::ToolLease)> {
        let (tools, parent) = self
            .tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find_map(|attachment| {
                let tools = attachment.tools.upgrade()?;
                (tools.lease().attachment_id() == attachment_id)
                    .then(|| (tools, attachment.parent.clone()))
            })?;
        let lease = tools.lease();
        if lease.caller() != caller
            || lease.terminal_target().as_ref() != Some(target)
            || !lease.spawn_enabled()
        {
            return None;
        }
        if let Some(parent) = parent {
            return Some((parent, lease.clone()));
        }
        let (record, registered) = self.spawn_parent(target)?;
        (registered.attachment_id() == attachment_id).then_some((
            ToolSpawnParent {
                binding_id: record.binding_id,
                launch: record.launch,
            },
            registered,
        ))
    }

    /// Retain native parent provenance after its exact real task terminal has been bound.
    /// # Errors
    /// Rejects unregistered/revoked attachments, duplicate capture, or invalid reusable launch data.
    pub fn retain_native_parent(
        &self,
        tools: &crate::ToolBridge,
        binding_id: &str,
        launch: AgentLaunch,
    ) -> Result<(), String> {
        launch.validate()?;
        if binding_id.is_empty()
            || binding_id.len() > 8192
            || binding_id.chars().any(char::is_control)
            || launch.ephemeral
            || launch.account_directory.is_none()
            || launch.cwd.is_none()
            || tools.lease().terminal_target().is_none()
        {
            return Err(
                "Native spawn parent requires its bound task and captured reusable launch"
                    .to_owned(),
            );
        }
        let mut attachments = self.tools.lock().unwrap_or_else(PoisonError::into_inner);
        let attachment = attachments
            .iter_mut()
            .find(|attachment| {
                attachment.tools.upgrade().is_some_and(|registered| {
                    registered.lease().attachment_id() == tools.lease().attachment_id()
                })
            })
            .ok_or("Native tool attachment is no longer registered")?;
        if attachment.parent.is_some() {
            return Err("Native parent provenance was already captured".to_owned());
        }
        attachment.parent = Some(ToolSpawnParent {
            binding_id: binding_id.to_owned(),
            launch,
        });
        drop(attachments);
        Ok(())
    }

    /// Prepare native observation on a worker before the backend starts the real TUI.
    /// # Errors
    /// Returns unsupported provider selectors, invalid argv or native observation startup errors.
    pub fn prepare(
        self: &Arc<Self>,
        provider: AgentKind,
        launch: AgentLaunch,
    ) -> Result<PreparedTerminalAgent, String> {
        launch.validate()?;
        let lease = Arc::new(Mutex::new(ObservationLease {
            target: None,
            latest: AgentObservation::default(),
            working_since: None,
        }));
        let weak = Arc::downgrade(self);
        let pending = lease.clone();
        let sink: ObservationSink =
            Arc::new(move |observation| publish(&weak, &pending, observation));
        let observer = match provider {
            AgentKind::Claude => TerminalObserver::Claude(
                crate::claude_terminal::ClaudeTerminalObserver::prepare(&launch, sink)?,
            ),
            AgentKind::Codex => TerminalObserver::Codex(
                crate::codex_terminal::CodexTerminalObserver::prepare(
                    &launch,
                    &self.runtime_directory,
                    sink,
                )
                .map_err(|error| error.to_string())?,
            ),
            AgentKind::Pi => TerminalObserver::Pi(
                crate::pi_terminal::PiTerminalObserver::prepare(
                    &launch,
                    &self.runtime_directory,
                    sink,
                )
                .map_err(|error| error.to_string())?,
            ),
        };
        Ok(PreparedTerminalAgent {
            launch,
            provider,
            lease,
            observer,
            tools: None,
        })
    }

    /// Prepare per-launch tools without retaining their private runtime configuration.
    /// # Errors
    /// Returns provider mismatch, oversized merged argv, or observation startup errors.
    pub fn prepare_with_tools(
        self: &Arc<Self>,
        provider: AgentKind,
        launch: AgentLaunch,
        tools: crate::ToolBridge,
        unobserved: Option<String>,
    ) -> Result<PreparedTerminalAgent, String> {
        launch.validate()?;
        let tools = self.retain_tool_attachment(provider, tools)?;
        let mut runtime = launch.clone();
        runtime.arguments = tools.arguments();
        runtime.arguments.extend(launch.arguments.iter().cloned());
        runtime.validate()?;
        let mut prepared = match unobserved {
            Some(detail) => Self::prepare_unobserved(provider, runtime, detail)?,
            None => self.prepare(provider, runtime)?,
        };
        prepared.launch = launch;
        prepared.tools = Some(tools);
        Ok(prepared)
    }

    /// Retain a native or terminal launch under the same provider and capture policy.
    /// Private launch arguments remain owned by the bridge and never enter provider metadata.
    /// # Errors
    /// Rejects another provider or tools disabled before attachment admission.
    pub fn retain_tool_attachment(
        &self,
        provider: AgentKind,
        tools: crate::ToolBridge,
    ) -> Result<Arc<crate::ToolBridge>, String> {
        if tools.lease().scope().provider != provider {
            return Err("Tool attachment belongs to another provider".to_owned());
        }
        let tools = Arc::new(tools);
        {
            let disabled = self
                .disabled_tools
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if disabled.disabled.contains(&provider) {
                return Err("Provider tools were disabled before launch preparation".to_owned());
            }
            tools.lease().restrict(crate::ToolPolicy {
                own_terminal_read: true,
                // Preserve explicit document grants; computer/spawn settings do not own them.
                browser_capture: true,
                computer_capture: disabled.computer_capture_enabled,
                spawn_children: disabled.spawn_enabled,
            });
            let mut attachments = self.tools.lock().unwrap_or_else(PoisonError::into_inner);
            attachments.retain(|attachment| attachment.tools.strong_count() > 0);
            attachments.push(ToolAttachment {
                tools: Arc::downgrade(&tools),
                parent: None,
            });
            drop(attachments);
            drop(disabled);
        }
        Ok(tools)
    }

    /// Disabling revokes current and pending attachments. Re-enabling permits only new leases.
    pub fn set_provider_tools_enabled(&self, provider: AgentKind, enabled: bool) {
        let mut disabled = self
            .disabled_tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        disabled.disabled.retain(|entry| *entry != provider);
        if !enabled {
            disabled.disabled.push(provider);
            let attachments = self.tools.lock().unwrap_or_else(PoisonError::into_inner);
            for tools in attachments
                .iter()
                .filter_map(|attachment| attachment.tools.upgrade())
                .filter(|tools| tools.lease().scope().provider == provider)
            {
                tools.stop();
            }
            drop(attachments);
        }
        drop(disabled);
    }

    /// Disabling narrows pending/live leases; enabling permits only newly issued authority.
    pub fn set_agent_spawning_enabled(&self, enabled: bool) {
        let mut policy = self
            .disabled_tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        policy.spawn_enabled = enabled;
        if !enabled {
            let attachments = self.tools.lock().unwrap_or_else(PoisonError::into_inner);
            for tools in attachments
                .iter()
                .filter_map(|attachment| attachment.tools.upgrade())
            {
                tools.lease().restrict(crate::ToolPolicy {
                    own_terminal_read: true,
                    browser_capture: true,
                    computer_capture: policy.computer_capture_enabled,
                    spawn_children: policy.spawn_enabled,
                });
            }
            drop(attachments);
        }
        drop(policy);
    }

    /// Disabling narrows pending/live leases; enabling permits only newly issued authority.
    pub fn set_computer_capture_enabled(&self, enabled: bool) {
        let mut policy = self
            .disabled_tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        policy.computer_capture_enabled = enabled;
        if !enabled {
            let attachments = self.tools.lock().unwrap_or_else(PoisonError::into_inner);
            for tools in attachments
                .iter()
                .filter_map(|attachment| attachment.tools.upgrade())
            {
                tools.lease().restrict(crate::ToolPolicy {
                    own_terminal_read: true,
                    browser_capture: true,
                    computer_capture: policy.computer_capture_enabled,
                    spawn_children: policy.spawn_enabled,
                });
            }
            drop(attachments);
        }
        drop(policy);
    }

    /// Revoke the closed terminal immediately, independently of its catalog write outcome.
    pub fn revoke_terminal_tools(&self, target: &CommandTarget) {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        for observer in live.iter().filter(|observer| observer.target == *target) {
            observer.stop_tools();
        }
        drop(live);
        let attachments = self.tools.lock().unwrap_or_else(PoisonError::into_inner);
        for tools in attachments
            .iter()
            .filter_map(|attachment| attachment.tools.upgrade())
        {
            if tools.lease().terminal_target().as_ref() == Some(target) {
                tools.stop();
            }
        }
    }

    /// Keep the provider's original TUI argv when its observation cannot share the pane lifetime.
    /// # Errors
    /// Returns an invalid provider launch.
    pub fn prepare_unobserved(
        provider: AgentKind,
        launch: AgentLaunch,
        detail: String,
    ) -> Result<PreparedTerminalAgent, String> {
        launch.validate()?;
        let observer = TerminalObserver::Unobserved(launch.arguments.clone());
        let lease = Arc::new(Mutex::new(ObservationLease {
            target: None,
            working_since: None,
            latest: AgentObservation {
                status: TerminalAgentStatus::Unavailable,
                detail: Some(detail),
                ..Default::default()
            }
            .bounded(),
        }));
        Ok(PreparedTerminalAgent {
            launch,
            provider,
            lease,
            observer,
            tools: None,
        })
    }

    /// Commit identity and retained configuration before exposing a provider in the sidebar.
    /// # Errors
    /// Returns invalid target/catalog errors before publication, or a tool binding failure after
    /// the durable record and observation registered. A binding failure always revokes tools.
    pub fn register(
        &self,
        prepared: PreparedTerminalAgent,
        target: CommandTarget,
        binding_id: String,
    ) -> Result<TerminalAgentRecord, String> {
        self.register_inner(prepared, target, binding_id, None)
    }

    /// Replace one exact old catalog entry after its cold-restored provider has started.
    /// # Errors
    /// Rejects changed provenance and failed commits without removing the retained source.
    pub fn register_restored(
        &self,
        prepared: PreparedTerminalAgent,
        target: CommandTarget,
        source: &TerminalAgentRecord,
    ) -> Result<TerminalAgentRecord, String> {
        self.register_inner(prepared, target, source.binding_id.clone(), Some(source))
    }

    fn register_inner(
        &self,
        prepared: PreparedTerminalAgent,
        target: CommandTarget,
        binding_id: String,
        source: Option<&TerminalAgentRecord>,
    ) -> Result<TerminalAgentRecord, String> {
        let mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let mut lease = prepared
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(source) = source {
            self.restore_observation(&prepared, source, &binding_id, &mut lease.latest)?;
        }
        let record = TerminalAgentRecord {
            provider: prepared.provider,
            target: target.clone(),
            binding_id,
            location: source.and_then(|source| source.location.clone()),
            launch: prepared.launch.retained(prepared.provider),
            observation: lease.latest.clone().bounded(),
        };
        validate_record(&record)?;
        let mut candidate = self.records();
        candidate.retain(|record| {
            record.target.handle != target.handle
                && source.is_none_or(|source| record.target != source.target)
        });
        if candidate.len() >= 128 {
            return Err("Terminal agent catalog is full".to_owned());
        }
        candidate.push(record.clone());
        self.commit(&candidate)?;
        *self.records.lock().unwrap_or_else(PoisonError::into_inner) = candidate;
        lease.target = Some(target.clone());
        drop(lease);
        let disabled = self
            .disabled_tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let tools_result = prepared.tools.as_ref().map_or(Ok(()), |tools| {
            if disabled.disabled.contains(&prepared.provider) {
                tools.stop();
                return Err("Provider was disabled before tool attachment registration".to_owned());
            }
            let binding = &tools.lease().scope().binding;
            tools
                .lease()
                .bind(binding, target.clone())
                .inspect_err(|_| tools.stop())
        });
        let prior = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            let mut prior = Vec::new();
            for retiring in [Some(&target), source.map(|source| &source.target)]
                .into_iter()
                .flatten()
            {
                if let Some(index) = live
                    .iter()
                    .position(|live| live.target.handle == retiring.handle)
                {
                    let observer = live.remove(index);
                    observer
                        .lease
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .target = None;
                    prior.push(observer);
                }
            }
            live.push(LiveObservation {
                target,
                lease: prepared.lease,
                observer: prepared.observer,
                tools: prepared.tools,
            });
            prior
        };
        drop(disabled);
        drop(mutation);
        for prior in prior {
            prior.stop_tools();
            thread_drop(prior);
        }
        self.changed();
        tools_result
            .map(|()| record)
            .map_err(|error| format!("Metadata registered, but tool attachment failed: {error}"))
    }

    fn restore_observation(
        &self,
        prepared: &PreparedTerminalAgent,
        source: &TerminalAgentRecord,
        binding_id: &str,
        observation: &mut AgentObservation,
    ) -> Result<(), String> {
        let current = self
            .record(&source.target)
            .ok_or("Retained agent source changed during recovery")?;
        if current.provider != prepared.provider
            || current.binding_id != binding_id
            || current.launch != source.launch
            || current.location != source.location
            || prepared.launch.retained(prepared.provider)
                != source.launch.retained(source.provider)
            || observation
                .session_id
                .as_ref()
                .zip(source.observation.session_id.as_ref())
                .is_some_and(|(observed, saved)| observed != saved)
            || observation
                .session_file
                .as_ref()
                .zip(source.observation.session_file.as_ref())
                .is_some_and(|(observed, saved)| observed != saved)
            || current.recovery_launch().is_err()
            || current.observation.session_id != source.observation.session_id
            || current.observation.session_file != source.observation.session_file
        {
            return Err("Retained agent provenance changed during recovery".to_owned());
        }
        if observation.session_id.is_none() {
            observation
                .session_id
                .clone_from(&source.observation.session_id);
        }
        if observation.session_file.is_none() {
            observation
                .session_file
                .clone_from(&source.observation.session_file);
        }
        Ok(())
    }

    /// Commit stable saved topology without changing the live or retained terminal target.
    /// # Errors
    /// Rejects stale records, changed associations, unsafe keys and failed durable writes.
    pub fn associate_location(
        &self,
        source: &TerminalAgentRecord,
        location: TerminalAgentLocation,
    ) -> Result<TerminalAgentRecord, String> {
        let mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let mut candidate = self.records();
        let current = candidate
            .iter_mut()
            .find(|record| record.target == source.target)
            .ok_or("Retained terminal changed before topology association")?;
        if current.provider != source.provider
            || current.binding_id != source.binding_id
            || current.launch != source.launch
            || current
                .location
                .as_ref()
                .is_some_and(|prior| prior != &location)
        {
            return Err(
                "Retained terminal provenance changed before topology association".to_owned(),
            );
        }
        if current.location.as_ref() == Some(&location) {
            return Ok(current.clone());
        }
        current.location = Some(location);
        validate_record(current)?;
        let record = current.clone();
        self.commit(&candidate)?;
        *self.records.lock().unwrap_or_else(PoisonError::into_inner) = candidate;
        drop(mutation);
        self.changed();
        Ok(record)
    }

    /// Keep private runtime argv out of recovery command arguments.
    /// # Errors
    /// Rejects oversized pending recovery and a mismatched provider or binding.
    pub fn stage_restore(&self, restore: PreparedTerminalRestore) -> Result<String, String> {
        if restore.prepared.provider != restore.source.provider
            || restore.target.kind != ResourceKind::Terminal
        {
            return Err("Prepared terminal recovery provenance is invalid".to_owned());
        }
        let token = crate::terminal_observation::terminal_session_id()?;
        let mut restores = self.restores.lock().unwrap_or_else(PoisonError::into_inner);
        if restores.len() >= 128 {
            return Err("Pending terminal recovery exceeds 128 entries".to_owned());
        }
        restores.insert(token.clone(), restore);
        drop(restores);
        Ok(token)
    }

    /// Taking a preparation also prevents retries after a partial process replacement.
    #[must_use]
    pub fn take_restore(&self, token: &str) -> Option<PreparedTerminalRestore> {
        self.restores
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(token)
    }

    /// Retire the exact observation after its backend pane has successfully closed.
    /// # Errors
    /// Returns an unregistered target or failed durable commit. Retained metadata stays active;
    /// tool authority is revoked immediately even when that commit fails.
    pub fn retire(&self, target: &CommandTarget) -> Result<TerminalAgentRecord, String> {
        self.revoke_terminal_tools(target);
        let mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let mut candidate = self.records();
        let record = candidate
            .iter_mut()
            .find(|record| record.target == *target)
            .ok_or("The terminal observation target is no longer registered")?;
        record.observation.status = TerminalAgentStatus::Stopped;
        record.observation.detail = None;
        let retired = record.clone();
        self.commit(&candidate)?;
        let observer = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            live.iter()
                .position(|observer| observer.target == *target)
                .map(|index| live.remove(index))
        };
        if let Some(observer) = &observer {
            observer
                .lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .target = None;
        }
        *self.records.lock().unwrap_or_else(PoisonError::into_inner) = candidate;
        drop(mutation);
        if let Some(observer) = observer {
            thread_drop(observer);
        }
        self.changed();
        Ok(retired)
    }

    /// Stop observations and dispose their private files on a worker, preserving the catalog.
    pub fn shutdown(&self) {
        let live = self.take_shutdown_observers();
        let directory = self.runtime_directory.clone();
        std::thread::spawn(move || {
            drop(live);
            let _ = fs::remove_dir(directory);
        });
    }

    /// Worker-only barrier for observations currently owned by this service. Call before retiring
    /// terminals or asynchronous shutdown; detached prior teardown is outside this barrier.
    /// Retained records remain available and backend terminal processes are never stopped here.
    /// # Errors
    /// Returns observer worker panics or private runtime directory cleanup errors after all joins.
    pub fn shutdown_and_wait(&self) -> Result<(), String> {
        let mut live = self.take_shutdown_observers();
        let mut errors = Vec::new();
        for observation in &mut live {
            if let Err(error) = observation.observer.stop_and_wait() {
                errors.push(error);
            }
        }
        drop(live);
        if let Err(error) = fs::remove_dir(&self.runtime_directory)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Stop observations and dispose their private files on a worker, preserving the catalog.
    fn take_shutdown_observers(&self) -> Vec<LiveObservation> {
        {
            let mut disabled = self
                .disabled_tools
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            disabled.disabled = AgentKind::ALL.to_vec();
            disabled.spawn_enabled = false;
            disabled.computer_capture_enabled = false;
            let attachments = self.tools.lock().unwrap_or_else(PoisonError::into_inner);
            for tools in attachments
                .iter()
                .filter_map(|attachment| attachment.tools.upgrade())
            {
                tools.stop();
            }
            drop(attachments);
            drop(disabled);
        }
        let mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let live = std::mem::take(&mut *self.live.lock().unwrap_or_else(PoisonError::into_inner));
        for observer in &live {
            observer.stop_tools();
            observer
                .lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .target = None;
            observer.observer.stop();
        }
        drop(mutation);
        live
    }

    fn commit(&self, records: &[TerminalAgentRecord]) -> Result<(), String> {
        let bytes = serde_json::to_vec(records).map_err(|error| error.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("Terminal agent catalog exceeds 1 MiB".to_owned());
        }
        let outcome = WriteTarget::resolve(&self.path)
            .map_err(|error| error.into_io().to_string())?
            .lock()
            .map_err(|error| error.to_string())?
            .replace(&bytes, NewFileMode::Private)
            .map_err(|error| error.into_io().to_string())?;
        match outcome {
            CommitOutcome::Confirmed => Ok(()),
            CommitOutcome::CommittedWithDurabilityWarning(error) => Err(format!(
                "Terminal catalog durability could not be confirmed: {error}"
            )),
        }
    }

    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        let callback = self
            .change
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(callback) = callback {
            callback();
        }
    }
}

impl Drop for TerminalAgentService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn thread_drop(observer: LiveObservation) {
    std::thread::spawn(move || drop(observer));
}

fn validate_record(record: &TerminalAgentRecord) -> Result<(), String> {
    if record.target.kind != ResourceKind::Terminal
        || record.target.handle.is_empty()
        || record.binding_id.is_empty()
        || record.binding_id.len() > 4096
    {
        return Err(
            "Terminal agent requires an exact terminal target and binding identity".to_owned(),
        );
    }
    if let Some(location) = &record.location {
        for key in [
            &location.task_identity,
            &location.window_id,
            &location.pane_id,
        ] {
            if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
                return Err("Terminal agent requires bounded saved topology keys".to_owned());
            }
        }
    }
    record.launch.validate()
}

fn publish(
    service: &Weak<TerminalAgentService>,
    lease: &Mutex<ObservationLease>,
    observation: AgentObservation,
) {
    let Some(service) = service.upgrade() else {
        return;
    };
    let observation = observation.bounded();
    let target = {
        let mut pending = lease.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.target.is_none() {
            if observation.status != TerminalAgentStatus::Working {
                pending.working_since = None;
            } else if pending.latest.status != TerminalAgentStatus::Working {
                pending.working_since = Some((service.clock)());
            }
        }
        observation.clone_into(&mut pending.latest);
        pending.target.clone()
    };
    let Some(target) = target else {
        return;
    };
    let mutation = service
        .mutation
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if lease
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .target
        .as_ref()
        != Some(&target)
    {
        return;
    }
    let mut candidate = service.records();
    let Some(record) = candidate.iter_mut().find(|record| record.target == target) else {
        return;
    };
    let mut observation = observation;
    if observation.session_id.is_none() {
        observation
            .session_id
            .clone_from(&record.observation.session_id);
    }
    if observation.session_file.is_none() {
        observation
            .session_file
            .clone_from(&record.observation.session_file);
    }
    if record.observation == observation {
        return;
    }
    let identity_changed = record.observation.session_id != observation.session_id
        || record.observation.session_file != observation.session_file;
    let previous_status = record.observation.status;
    let status = observation.status;
    record.observation = observation;
    if identity_changed && service.commit(&candidate).is_err() {
        return;
    }
    {
        let mut pending = lease.lock().unwrap_or_else(PoisonError::into_inner);
        if status != TerminalAgentStatus::Working {
            pending.working_since = None;
        } else if previous_status != TerminalAgentStatus::Working {
            pending.working_since = Some((service.clock)());
        }
    }
    *service
        .records
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = candidate;
    drop(mutation);
    service.changed();
}
