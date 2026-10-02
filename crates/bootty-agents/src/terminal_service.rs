use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use bootty_control::{CommandTarget, ResourceKind};
use bootty_write::{NewFileMode, WriteTarget};
use serde::{Deserialize, Serialize};

use crate::{
    AgentCommandExecutor, AgentKind, AgentLaunch, TerminalToolRequest,
    terminal_tools::{TerminalSpawnScope, TerminalTools},
};

/// Launch identity for a backend-owned terminal. This owner never starts a provider process.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TerminalAgentRecord {
    pub provider: AgentKind,
    pub target: CommandTarget,
    pub binding_id: String,
    pub launch: AgentLaunch,
    pub session_id: Option<String>,
}

pub struct TerminalAgentService {
    path: PathBuf,
    records: Mutex<Vec<TerminalAgentRecord>>,
    revision: AtomicU64,
    tools: TerminalTools,
}

impl TerminalAgentService {
    #[must_use]
    pub fn new_session_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// # Errors
    /// Returns bounded catalog decoding or filesystem errors.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let records: Vec<TerminalAgentRecord> = match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > 1024 * 1024 => {
                return Err("Terminal agent catalog exceeds 1 MiB".to_owned());
            }
            Ok(_) => serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        if records.len() > 128 {
            return Err("Terminal agent catalog exceeds 128 entries".to_owned());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        Ok(Self {
            path,
            records: Mutex::new(records),
            revision: AtomicU64::new(1),
            tools: TerminalTools::default(),
        })
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

    /// Reserve only own-terminal read tools for a native provider launch. No permission or
    /// credential is persisted, and provider hooks are unrelated to this policy.
    /// # Errors
    /// Returns a missing binding or attachment limit error.
    pub fn reserve_terminal_tools(
        &self,
        provider: AgentKind,
        binding_id: &str,
    ) -> Result<String, String> {
        self.tools.reserve(provider, binding_id, None)
    }

    /// Reserve own-terminal read and a detached ordinary shell in this exact Space and checkout.
    /// The child shell receives no agent tool authority. Nothing is persisted.
    /// # Errors
    /// Returns invalid binding/checkout or attachment limit errors.
    pub fn reserve_terminal_session_tools(
        &self,
        provider: AgentKind,
        binding_id: &str,
        binding_target: CommandTarget,
        cwd: String,
    ) -> Result<String, String> {
        self.tools.reserve(
            provider,
            binding_id,
            Some(TerminalSpawnScope {
                binding_target,
                cwd,
            }),
        )
    }

    /// Publish an exact target only after its launch metadata has committed.
    /// # Errors
    /// Returns a missing record or revoked attachment error.
    pub fn complete_terminal_tools(&self, id: &str, target: &CommandTarget) -> Result<(), String> {
        let record = self
            .record(target)
            .ok_or_else(|| "Terminal agent metadata has not committed".to_owned())?;
        self.tools.complete(
            id,
            record.provider,
            &record.binding_id,
            target,
            record.launch.cwd.as_deref(),
        )
    }

    pub fn revoke_terminal_tools(&self, id: &str) {
        self.tools.revoke(id);
    }

    pub fn invoke_terminal_tool(
        &self,
        request: &TerminalToolRequest,
        executor: &dyn AgentCommandExecutor,
        deadline: std::time::Instant,
        cancellation: bootty_control::CommandCancellation,
    ) -> bootty_control::CommandOutcome {
        self.tools
            .invoke(self, request, executor, deadline, cancellation)
    }

    /// Commit retained launch metadata before publishing it. Credential argv is never retained.
    /// # Errors
    /// Returns invalid terminal identities, catalog limits or persistence errors.
    pub fn register(&self, mut record: TerminalAgentRecord) -> Result<(), String> {
        if record.target.kind != ResourceKind::Terminal || record.binding_id.is_empty() {
            return Err("Terminal agent launch needs an exact terminal and binding".to_owned());
        }
        record.launch.validate()?;
        record.launch = record.launch.retained(record.provider);
        let mut current = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let mut candidate = current.clone();
        candidate.retain(|prior| prior.target != record.target);
        if candidate.len() >= 128 {
            return Err("Terminal agent catalog is full".to_owned());
        }
        candidate.push(record);
        let bytes = serde_json::to_vec(&candidate).map_err(|error| error.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("Terminal agent catalog exceeds 1 MiB".to_owned());
        }
        write_private(&self.path, &bytes)?;
        *current = candidate;
        drop(current);
        self.revision.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    WriteTarget::resolve(path)
        .map_err(|error| error.into_io().to_string())?
        .lock()
        .map_err(|error| error.to_string())?
        .replace(bytes, NewFileMode::Private)
        .map_err(|error| error.into_io().to_string())?;
    Ok(())
}
