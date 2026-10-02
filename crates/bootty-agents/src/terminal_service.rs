use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use bootty_control::{CommandTarget, ResourceKind};
use bootty_write::{CommitOutcome, NewFileMode, WriteTarget};
use serde::{Deserialize, Serialize};

use crate::{
    AgentKind, AgentLaunch,
    terminal_observation::{AgentObservation, ObservationSink, TerminalAgentStatus},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalAgentRecord {
    pub provider: AgentKind,
    pub target: CommandTarget,
    pub binding_id: String,
    pub launch: AgentLaunch,
    pub observation: AgentObservation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalAgentActivity {
    pub provider: AgentKind,
    pub target: CommandTarget,
    pub status: TerminalAgentStatus,
    pub session_id: Option<String>,
    pub detail: Option<String>,
}

struct ObservationLease {
    target: Option<CommandTarget>,
    latest: AgentObservation,
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
}

pub struct PreparedTerminalAgent {
    pub launch: AgentLaunch,
    provider: AgentKind,
    lease: Arc<Mutex<ObservationLease>>,
    observer: TerminalObserver,
}

impl PreparedTerminalAgent {
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(self.launch.program.clone())
            .chain(self.observer.arguments())
            .collect()
    }
}

struct LiveObservation {
    target: CommandTarget,
    lease: Arc<Mutex<ObservationLease>>,
    _observer: TerminalObserver,
}

/// Owns only native observation and retained metadata. Backend panes own the interactive TUIs.
pub struct TerminalAgentService {
    path: PathBuf,
    runtime_directory: PathBuf,
    mutation: Mutex<()>,
    records: Mutex<Vec<TerminalAgentRecord>>,
    live: Mutex<Vec<LiveObservation>>,
    revision: AtomicU64,
    change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl TerminalAgentService {
    /// # Errors
    /// Returns malformed, oversized or invalid retained catalog errors.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
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
            runtime_directory,
            mutation: Mutex::new(()),
            records: Mutex::new(restored),
            live: Mutex::new(Vec::new()),
            revision: AtomicU64::new(1),
            change: Mutex::new(None),
        })
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
        self.record(target).map(|record| TerminalAgentActivity {
            provider: record.provider,
            target: record.target,
            status: record.observation.status,
            session_id: record.observation.session_id,
            detail: record.observation.detail,
        })
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
        })
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
        })
    }

    /// Commit identity and retained configuration before exposing a provider in the sidebar.
    /// # Errors
    /// Returns an invalid host-issued target, full catalog or failed durable commit.
    pub fn register(
        &self,
        prepared: PreparedTerminalAgent,
        target: CommandTarget,
        binding_id: String,
    ) -> Result<TerminalAgentRecord, String> {
        let mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let mut lease = prepared
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let record = TerminalAgentRecord {
            provider: prepared.provider,
            target: target.clone(),
            binding_id,
            launch: prepared.launch.retained(prepared.provider),
            observation: lease.latest.clone().bounded(),
        };
        validate_record(&record)?;
        let mut candidate = self.records();
        candidate.retain(|record| record.target.handle != target.handle);
        if candidate.len() >= 128 {
            return Err("Terminal agent catalog is full".to_owned());
        }
        candidate.push(record.clone());
        self.commit(&candidate)?;
        *self.records.lock().unwrap_or_else(PoisonError::into_inner) = candidate;
        lease.target = Some(target.clone());
        drop(lease);
        let prior = {
            let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
            let prior = live
                .iter()
                .position(|prior| prior.target.handle == target.handle)
                .map(|index| live.remove(index));
            live.push(LiveObservation {
                target,
                lease: prepared.lease,
                _observer: prepared.observer,
            });
            prior
        };
        drop(mutation);
        if let Some(prior) = prior {
            thread_drop(prior);
        }
        self.changed();
        Ok(record)
    }

    /// Retire the exact observation after its backend pane has successfully closed.
    /// # Errors
    /// Returns an unregistered target or a failed durable commit, leaving prior state active.
    pub fn retire(&self, target: &CommandTarget) -> Result<TerminalAgentRecord, String> {
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
        let mutation = self.mutation.lock().unwrap_or_else(PoisonError::into_inner);
        let live = std::mem::take(&mut *self.live.lock().unwrap_or_else(PoisonError::into_inner));
        for observer in &live {
            observer
                .lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .target = None;
        }
        drop(mutation);
        let directory = self.runtime_directory.clone();
        std::thread::spawn(move || {
            drop(live);
            let _ = fs::remove_dir(directory);
        });
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
    record.launch.validate()
}

fn publish(
    service: &Weak<TerminalAgentService>,
    lease: &Mutex<ObservationLease>,
    observation: AgentObservation,
) {
    let observation = observation.bounded();
    let target = {
        let mut pending = lease.lock().unwrap_or_else(PoisonError::into_inner);
        observation.clone_into(&mut pending.latest);
        pending.target.clone()
    };
    let (Some(service), Some(target)) = (service.upgrade(), target) else {
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
    record.observation = observation;
    if identity_changed && service.commit(&candidate).is_err() {
        return;
    }
    *service
        .records
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = candidate;
    drop(mutation);
    service.changed();
}
