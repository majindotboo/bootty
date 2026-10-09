#[path = "native_fork.rs"]
pub mod fork;
#[path = "native_tool_activity.rs"]
pub mod tool_activity;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use bootty_control::{CommandInvocation, CommandTarget, ResourceKind};
use bootty_write::{NewFileMode, WriteTarget};
use serde::{Deserialize, Serialize};

use crate::{
    NativeAgentSession, NativeAttachmentReference, NativeChangeHandler, NativePromptAttachments,
    NativeSessionConfig, NativeSessionSnapshot, NativeSessionStatus,
    native_attachments::{
        MAX_NATIVE_PROMPT_ATTACHMENTS, MAX_NATIVE_SESSION_ATTACHMENT_BYTES,
        MAX_NATIVE_SESSION_ATTACHMENTS, NativeAttachmentStore,
    },
    native_protocol::bounded_text,
    native_session::lock,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeSessionRecord {
    pub id: String,
    pub binding_id: String,
    /// Captured Bootty task membership; never reconstructed from the selected mux session.
    #[serde(default)]
    pub task_identity: Option<String>,
    pub title: String,
    /// Accepted first input retained until provider submission succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_initial_message: Option<String>,
    pub generation: u64,
    pub config: NativeSessionConfig,
    /// Live projection: the saved policy differs from the current provider process.
    #[serde(default)]
    pub permissions_pending: bool,
    pub snapshot: NativeSessionSnapshot,
    #[serde(default)]
    pub attachments: Vec<NativeAttachmentReference>,
    #[serde(default)]
    pub side_chat: Option<crate::NativeSideChat>,
    /// Exact spawning conversation; its live host grant is never restored with this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_parent: Option<CommandTarget>,
}

/// Creation can fail after a durable reservation exists. Its exact target remains retryable.
#[derive(Debug)]
pub struct NativeCreationError {
    pub message: String,
    pub target: Option<CommandTarget>,
}

impl From<String> for NativeCreationError {
    fn from(message: String) -> Self {
        Self {
            message,
            target: None,
        }
    }
}

impl std::fmt::Display for NativeCreationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NativeCreationError {}

impl NativeSessionRecord {
    fn restore_stopped(&mut self) {
        self.snapshot.status = NativeSessionStatus::Stopped;
        if self.config.account_directory.is_none() {
            self.snapshot.error = Some("This saved conversation predates captured accounts. Its history is retained; start a new session to continue with your selected account.".into());
        }
        if let Some(fork) = &mut self.side_chat {
            fork.retain_copied_transcript(&self.snapshot.transcript);
        }
        self.snapshot.application_mentions_supported = false;
        self.snapshot.requests.clear();
        self.snapshot.turn_id = None;
        self.snapshot.working_since = None;
    }

    #[must_use]
    pub fn target(&self) -> CommandTarget {
        CommandTarget {
            kind: ResourceKind::Session,
            handle: self.id.clone(),
            generation: self.generation,
        }
    }
}

/// Lightweight accepted conversation facts for the sidebar; transcript data stays in records.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeSessionActivity {
    pub id: String,
    pub title: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_parent: Option<CommandTarget>,
    pub binding_id: String,
    pub task_identity: Option<String>,
    pub provider: crate::AgentKind,
    pub status: NativeSessionStatus,
    pub completed_turn: bool,
    pub first_turn: Option<crate::NativeTurnReceipt>,
    pub approval: bool,
    pub input: bool,
    /// Elapsed time from this live turn's accepted prompt; absent outside `Working`.
    pub working_elapsed: Option<Duration>,
}

/// Captured provider choices without executable, account, project or credential paths.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeProviderInfo {
    pub provider: crate::AgentKind,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub fast_mode: bool,
    pub permissions: crate::NativePermissionMode,
    pub permission_modes: Vec<crate::NativePermissionMode>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Store {
    next_generation: u64,
    records: Vec<NativeSessionRecord>,
    #[serde(default)]
    model_favorites: Vec<ModelFavorite>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_catalogs: BTreeMap<String, crate::NativeProviderCatalog>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_catalog_aliases: BTreeMap<String, String>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct ModelFavorite {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<bootty_config::config::RemoteConfig>,
    provider: crate::AgentKind,
    account_directory: Option<String>,
    #[serde(rename = "model")]
    model_id: String,
}

type NativePlacement<'a> = dyn Fn(&NativeSessionRecord) -> Result<(), String> + 'a;

enum CreationOrigin {
    SideChat(fork::ForkContext),
    Spawn(CommandTarget),
    InitialMessage {
        message: String,
        attachments: Vec<PathBuf>,
    },
}

/// App-owned native sessions and bounded provider model catalogs. Credentials remain with providers.
/// Opening a restored session resumes its existing provider identity.
pub struct NativeAgentService {
    path: PathBuf,
    attachment_store: NativeAttachmentStore,
    mutation: Arc<Mutex<()>>,
    store: Arc<Mutex<Store>>,
    live: Arc<Mutex<BTreeMap<String, Arc<NativeAgentSession>>>>,
    change_handler: Arc<Mutex<Option<NativeChangeHandler>>>,
    revision: Arc<AtomicU64>,
    publication: mpsc::SyncSender<()>,
    shutdown: Arc<AtomicBool>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl NativeAgentService {
    /// # Errors
    /// Returns malformed/oversized state or inability to create the private storage directory.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        Self::open_with_clock(path, Arc::new(Instant::now))
    }

    /// Open the native session owner with an injected monotonic clock.
    /// # Errors
    /// Returns malformed/oversized state or inability to create the private storage directory.
    pub fn open_with_clock(
        path: impl Into<PathBuf>,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Result<Self, String> {
        let path = path.into();
        let mut store = match fs::File::open(&path) {
            Ok(file) => {
                if file.metadata().map_err(|error| error.to_string())?.len() > 16 * 1024 * 1024 {
                    return Err("Native agent catalog exceeds 16 MiB".to_owned());
                }
                serde_json::from_reader::<_, Store>(file).map_err(|error| error.to_string())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(error) => return Err(error.to_string()),
        };
        if store.records.len() > 128 {
            return Err("Native agent catalog exceeds 128 sessions".to_owned());
        }
        if store.model_favorites.len() > 256 {
            return Err("Model favorites exceed 256 choices".to_owned());
        }
        validate_model_catalog_cache(&store)?;
        let mut identities = std::collections::BTreeSet::new();
        for record in &mut store.records {
            record.config.validate_stored()?;
            validate_pending_message(
                record.pending_initial_message.as_deref(),
                !record.attachments.is_empty(),
            )?;
            if record.id.is_empty()
                || !identities.insert(record.id.clone())
                || record.generation > store.next_generation
                || record.attachments.len() > MAX_NATIVE_SESSION_ATTACHMENTS
                || record.spawn_parent.as_ref().is_some_and(|parent| {
                    parent.kind != ResourceKind::Session
                        || parent.handle.is_empty()
                        || parent.handle.len() > 8192
                        || parent.handle.chars().any(char::is_control)
                        || parent.generation == 0
                        || parent.generation > store.next_generation
                })
            {
                return Err("Invalid native agent catalog identity".to_owned());
            }
            let mut attachment_ids = std::collections::BTreeSet::new();
            let _attachment_bytes =
                record
                    .attachments
                    .iter()
                    .try_fold(0_u64, |total, attachment| {
                        attachment.validate()?;
                        if !attachment_ids.insert(attachment.id.as_str()) {
                            return Err("Duplicate native attachment identity".to_owned());
                        }
                        total
                            .checked_add(attachment.size_bytes)
                            .filter(|size| *size <= MAX_NATIVE_SESSION_ATTACHMENT_BYTES)
                            .ok_or_else(|| "Native session attachments exceed 512 MiB".to_owned())
                    })?;
            record.restore_stopped();
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mutation = Arc::new(Mutex::new(()));
        let store = Arc::new(Mutex::new(store));
        let live = Arc::new(Mutex::new(BTreeMap::new()));
        let change_handler = Arc::new(Mutex::new(None));
        let revision = Arc::new(AtomicU64::new(1));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (publication, receiver) = mpsc::sync_channel(1);
        start_publication_worker(
            PublicationState {
                path: path.clone(),
                mutation: Arc::clone(&mutation),
                store: Arc::clone(&store),
                live: Arc::clone(&live),
                change_handler: Arc::clone(&change_handler),
                revision: Arc::clone(&revision),
                shutdown: Arc::clone(&shutdown),
            },
            receiver,
        )?;
        Ok(Self {
            attachment_store: NativeAttachmentStore::new(&path),
            path,
            mutation,
            store,
            live,
            change_handler,
            revision,
            publication,
            shutdown,
            clock,
        })
    }

    pub fn set_change_handler(&self, handler: NativeChangeHandler) {
        *lock(&self.change_handler) = Some(handler);
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Observe accepted-input delivery without copying conversation transcripts.
    #[must_use]
    pub fn initial_message_pending(&self, binding_id: &str, task_identity: &str) -> Option<bool> {
        lock(&self.store)
            .records
            .iter()
            .find(|record| {
                record.binding_id == binding_id
                    && record.task_identity.as_deref() == Some(task_identity)
            })
            .map(|record| record.pending_initial_message.is_some())
    }

    /// Capture the session's Space without cloning its transcript on the UI thread.
    /// # Errors
    /// Returns an unknown or stale host target.
    pub fn binding_for_target(&self, target: &CommandTarget) -> Result<String, String> {
        lock(&self.store)
            .records
            .iter()
            .find(|record| record.target() == *target)
            .map(|record| record.binding_id.clone())
            .ok_or_else(|| "Native session target is unknown or stale".to_owned())
    }

    /// Copy a caller-selected file into this exact session's private attachment store.
    /// The source path is used only for this import and is never persisted or sent to a provider.
    /// # Errors
    /// Returns stale targets, bounded import failures, or durable catalog errors.
    pub fn import_attachment(
        &self,
        target: &CommandTarget,
        source_path: &Path,
    ) -> Result<NativeAttachmentReference, String> {
        if target.kind != ResourceKind::Session {
            return Err("Native attachments require a Session target".to_owned());
        }
        let _mutation = lock(&self.mutation);
        if self.shutdown.load(Ordering::Acquire) {
            return Err("Native agent host is shutting down".to_owned());
        }
        let mut candidate = lock(&self.store).clone();
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is unknown or stale")?;
        if record.attachments.len() >= MAX_NATIVE_SESSION_ATTACHMENTS {
            return Err("Native session attachment limit reached".to_owned());
        }
        let stored = self.attachment_store.import(&record.id, source_path)?;
        let current_bytes = record
            .attachments
            .iter()
            .map(|attachment| attachment.size_bytes)
            .sum::<u64>();
        if current_bytes.saturating_add(stored.reference.size_bytes)
            > MAX_NATIVE_SESSION_ATTACHMENT_BYTES
        {
            NativeAttachmentStore::remove_file(&stored);
            return Err("Native session attachments exceed 512 MiB".to_owned());
        }
        let reference = stored.reference.clone();
        record.attachments.push(reference.clone());
        if let Err(error) = self.commit(candidate) {
            NativeAttachmentStore::remove_file(&stored);
            return Err(error);
        }
        Ok(reference)
    }

    /// Resolve host-issued IDs for one exact session without exposing a path in catalog metadata.
    /// Images become validated provider payloads; other files become host path references.
    /// # Errors
    /// Rejects stale identities, duplicates, missing references or unavailable stored bytes.
    pub fn resolve_prompt_attachments(
        &self,
        target: &CommandTarget,
        ids: &[String],
    ) -> Result<NativePromptAttachments, String> {
        self.resolve_prompt_attachments_with(
            target,
            ids,
            &attachment_runner(Arc::clone(&self.shutdown))?,
        )
    }

    /// Resolve attachments using the invocation's owning-host deadline and cancellation.
    /// # Errors
    /// Rejects stale identities, missing bytes, or failed uploads without submitting input.
    pub fn resolve_prompt_attachments_with(
        &self,
        target: &CommandTarget,
        ids: &[String],
        runner: &impl bootty_host::CommandRunner,
    ) -> Result<NativePromptAttachments, String> {
        if target.kind != ResourceKind::Session || ids.len() > MAX_NATIVE_PROMPT_ATTACHMENTS {
            return Err("Native prompt attachment target or count is invalid".to_owned());
        }
        let mut seen = std::collections::BTreeSet::new();
        let record = lock(&self.store)
            .records
            .iter()
            .find(|record| record.target() == *target)
            .cloned()
            .ok_or("Native session target is unknown or stale")?;
        let mut resolved = Vec::with_capacity(ids.len());
        for id in ids {
            if !seen.insert(id.as_str()) {
                return Err("Duplicate native attachment identity".to_owned());
            }
            let reference = record
                .attachments
                .iter()
                .find(|reference| reference.id == *id)
                .ok_or("Native attachment does not belong to this session")?;
            let mut attachment = self.attachment_store.resolve(&record.id, reference)?;
            if let Some(remote) = &record.config.remote
                && reference.kind == crate::NativeAttachmentKind::File
            {
                let directory = remote_attachment_directory(&record.config)?;
                let host = bootty_host::remote::RemoteHost::new(remote.host.clone());
                let name = attachment
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("Native attachment has no stored filename")?;
                attachment.path = directory
                    .upload_remote(&host, runner, name, &attachment.path)
                    .map_err(|error| format!("Upload attachment: {error:#}"))?;
            }
            resolved.push(attachment);
        }
        // Uploads never redirect a prompt after its captured generation has been replaced.
        self.binding_for_target(target)?;
        NativePromptAttachments::from_resolved(resolved)
    }

    /// Return a small validated PNG preview for an image owned by this session.
    /// # Errors
    /// Rejects stale targets, cross-session IDs, files, or unavailable image bytes.
    pub fn preview_attachment(&self, target: &CommandTarget, id: &str) -> Result<Vec<u8>, String> {
        if target.kind != ResourceKind::Session {
            return Err("Native attachments require a Session target".to_owned());
        }
        let record = lock(&self.store)
            .records
            .iter()
            .find(|record| record.target() == *target)
            .cloned()
            .ok_or("Native session target is unknown or stale")?;
        let reference = record
            .attachments
            .iter()
            .find(|reference| reference.id == id)
            .ok_or("Native attachment does not belong to this session")?;
        self.attachment_store.preview(&record.id, reference)
    }

    /// # Errors
    /// Returns invalid titles, stale targets or persistence failures.
    pub fn rename(&self, target: &CommandTarget, title: &str) -> Result<(), String> {
        self.rename_when(target, None, title).map(|_| ())
    }

    /// Apply a delayed generated title only while its captured title remains current.
    /// # Errors
    /// Returns invalid titles, stale targets or persistence failures.
    pub fn rename_if_unchanged(
        &self,
        target: &CommandTarget,
        expected: &str,
        title: &str,
    ) -> Result<bool, String> {
        self.rename_when(target, Some(expected), title)
    }

    fn rename_when(
        &self,
        target: &CommandTarget,
        expected: Option<&str>,
        title: &str,
    ) -> Result<bool, String> {
        if title.trim().is_empty() || title.len() > 256 || title.chars().any(char::is_control) {
            return Err(
                "A session title must contain 1–256 bytes without control characters".to_owned(),
            );
        }
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is unknown or stale")?;
        if expected.is_some_and(|expected| record.title != expected) {
            return Ok(false);
        }
        title.clone_into(&mut record.title);
        self.commit(candidate).map(|()| true)
    }

    /// Submit through the exact native process owner, preserving its captured title.
    /// # Errors
    /// Returns stale-target, provider rejection or durable catalog errors.
    pub fn prompt(
        &self,
        target: &CommandTarget,
        message: &str,
    ) -> Result<NativeSessionSnapshot, String> {
        self.prompt_input(target, &crate::NativePrompt::text(message)?)
    }

    /// Submit host-admitted images through the same exact process and durable publication owner.
    /// # Errors
    /// Returns stale-target, provider rejection or durable catalog errors.
    pub fn prompt_input(
        &self,
        target: &CommandTarget,
        prompt: &crate::NativePrompt,
    ) -> Result<NativeSessionSnapshot, String> {
        // Provider waits hold no catalog mutation lease: interrupt/approval stay available.
        let session = self.resolve(target)?;
        let record = lock(&self.store)
            .records
            .iter()
            .find(|record| record.target() == *target)
            .cloned()
            .ok_or("Native session target is stale")?;
        let copied = record
            .side_chat
            .as_ref()
            .and_then(|fork| fork.copied_transcript(&record.snapshot.transcript))
            .unwrap_or_default();
        for citation in prompt.citations() {
            if copied.iter().any(|item| item.id == citation.message_id) {
                citation.validate(copied)?;
            } else {
                session.validate_history_citation(citation)?;
            }
        }
        let selection = record
            .config
            .model
            .as_ref()
            .map(|model| crate::NativeModelSelection {
                model: model.clone(),
                reasoning_effort: record.config.reasoning_effort.clone(),
            });
        if session.snapshot().status == NativeSessionStatus::Idle
            && session.config.permissions != record.config.permissions
        {
            return Err(
                "The next-turn permissions are being applied; retry after the conversation resumes"
                    .into(),
            );
        }
        session.grant_applications(target, prompt)?;
        let (prompt_with_history, history_identity) =
            self.side_chat_prompt(target, prompt, &session)?;
        let outcome =
            session.send_prompt_input_with_selection(&prompt_with_history, selection.as_ref());
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is stale")?;
        record.snapshot = session.snapshot();
        if outcome.is_ok() {
            record.pending_initial_message = None;
        }
        if outcome.is_ok()
            && let Some(identity) = history_identity
            && let Some(fork) = &mut record.side_chat
        {
            fork.seeded_identity = Some(identity);
        }
        record.config.capture_session(&record.snapshot);
        let snapshot = record.snapshot.clone();
        self.commit(candidate).map_err(|error| {
            if let Err(provider_error) = &outcome {
                format!("{provider_error}; native state could not be saved: {error}")
            } else {
                error
            }
        })?;
        outcome.map(|()| snapshot)
    }

    /// Discover choices from this conversation's exact live provider account.
    /// # Errors
    /// Returns stale/stopped targets or unsupported/provider discovery errors.
    pub fn models(&self, target: &CommandTarget) -> Result<Vec<crate::NativeModelOption>, String> {
        let session = self.resolve(target)?;
        let mut models = session.models()?;
        self.mark_model_favorites(&session.config, &mut models);
        Ok(models)
    }

    /// Read the model and permission metadata for this live provider account.
    /// # Errors
    /// Returns stale/stopped targets or unsupported/provider discovery errors.
    pub fn model_catalog(
        &self,
        target: &CommandTarget,
    ) -> Result<crate::NativeProviderCatalog, String> {
        let session = self.resolve(target)?;
        let mut catalog = session.catalog()?;
        self.mark_model_favorites(&session.config, &mut catalog.models);
        self.cache_provider_catalog(&session.config, &catalog)?;
        Ok(catalog)
    }

    /// Read advertised commands and skills through the exact live provider owner.
    /// # Errors
    /// Returns stale targets, missing provider capabilities or transport failures.
    pub fn completions(
        &self,
        target: &CommandTarget,
    ) -> Result<crate::NativeCompletionCatalog, String> {
        self.resolve(target)?.completions()
    }

    /// Read history through the exact provider/account owner; pages do not overwrite live state.
    /// # Errors
    /// Returns stale targets, unsupported paging or provider transport failures.
    pub fn read_history(
        &self,
        target: &CommandTarget,
        direction: &str,
    ) -> Result<crate::NativeHistoryPage, String> {
        self.resolve(target)?.read_history(direction)
    }

    /// Read a provider-reported child without acquiring its conversation writer.
    /// # Errors
    /// Returns stale targets, unknown children or provider transport errors.
    pub fn read_subagent(
        &self,
        target: &CommandTarget,
        id: &str,
    ) -> Result<crate::NativeSubagentDetail, String> {
        self.resolve(target)?.read_subagent(id)
    }

    /// Project favorites for this exact provider account into its advertised catalog.
    /// Retain observed catalog data so starring a model does not restart provider discovery.
    /// # Errors
    /// Returns an invalid captured launch configuration.
    pub fn cache_model_catalog(
        &self,
        config: &NativeSessionConfig,
        models: &[crate::NativeModelOption],
    ) -> Result<(), String> {
        let mut catalog =
            self.cached_provider_catalog(config)
                .unwrap_or_else(|| crate::NativeProviderCatalog {
                    models: Vec::new(),
                    permissions: None,
                });
        catalog.models = models.to_vec();
        self.cache_provider_catalog(config, &catalog)
    }

    /// Retain a complete provider catalog under its captured account and project identity.
    /// # Errors
    /// Returns an invalid captured launch identity.
    pub fn cache_provider_catalog(
        &self,
        config: &NativeSessionConfig,
        catalog: &crate::NativeProviderCatalog,
    ) -> Result<(), String> {
        self.cache_catalog(config, catalog, None)
    }

    /// Retain a catalog and its exact launch-invocation lookup alias atomically.
    /// # Errors
    /// Returns an invalid captured launch identity or a durable storage error.
    pub fn cache_provider_catalog_for_invocation(
        &self,
        config: &NativeSessionConfig,
        invocation: &CommandInvocation,
        preferences: &bootty_config::config::AgentProviderConfig,
        remote: Option<&bootty_config::config::RemoteConfig>,
        catalog: &crate::NativeProviderCatalog,
    ) -> Result<(), String> {
        let alias = model_catalog_invocation_key(invocation, preferences, remote)
            .ok_or("Invalid catalog invocation")?;
        self.cache_catalog(config, catalog, Some(alias))
    }

    fn cache_catalog(
        &self,
        config: &NativeSessionConfig,
        catalog: &crate::NativeProviderCatalog,
        alias: Option<String>,
    ) -> Result<(), String> {
        let key = model_catalog_key(config).map_err(|error| error.to_string())?;
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let catalog_matches = candidate.model_catalogs.get(&key).is_some_and(|cached| {
            cached.permissions == catalog.permissions && cached.models == catalog.models
        });
        let alias_matches = alias
            .as_ref()
            .is_none_or(|alias| candidate.model_catalog_aliases.get(alias) == Some(&key));
        if catalog_matches && alias_matches {
            return Ok(());
        }
        // Keep provider catalogs small and bounded beside the existing durable session state.
        if candidate.model_catalogs.len() >= 16
            && !candidate.model_catalogs.contains_key(&key)
            && let Some((evicted, _)) = candidate.model_catalogs.pop_first()
        {
            candidate
                .model_catalog_aliases
                .retain(|_, target| target != &evicted);
        }
        candidate
            .model_catalogs
            .insert(key.clone(), catalog.clone());
        if let Some(alias) = alias {
            if candidate.model_catalog_aliases.len() >= 16
                && !candidate.model_catalog_aliases.contains_key(&alias)
            {
                candidate.model_catalog_aliases.pop_first();
            }
            candidate.model_catalog_aliases.insert(alias, key);
        }
        self.commit(candidate)
    }

    #[must_use]
    pub fn cached_model_catalog(
        &self,
        config: &NativeSessionConfig,
    ) -> Option<Vec<crate::NativeModelOption>> {
        self.cached_provider_catalog(config)
            .map(|catalog| catalog.models)
    }

    #[must_use]
    pub fn cached_provider_catalog(
        &self,
        config: &NativeSessionConfig,
    ) -> Option<crate::NativeProviderCatalog> {
        let key = model_catalog_key(config).ok()?;
        lock(&self.store).model_catalogs.get(&key).cloned()
    }

    #[must_use]
    pub fn cached_provider_catalog_for_invocation(
        &self,
        invocation: &CommandInvocation,
        preferences: &bootty_config::config::AgentProviderConfig,
        remote: Option<&bootty_config::config::RemoteConfig>,
    ) -> Option<crate::NativeProviderCatalog> {
        let alias = model_catalog_invocation_key(invocation, preferences, remote)?;
        let store = lock(&self.store);
        let key = store.model_catalog_aliases.get(&alias)?;
        store.model_catalogs.get(key).cloned()
    }

    pub fn mark_model_favorites(
        &self,
        config: &NativeSessionConfig,
        models: &mut [crate::NativeModelOption],
    ) {
        let store = lock(&self.store);
        for model in models {
            model.is_favorite = store
                .model_favorites
                .iter()
                .filter(|favorite| {
                    favorite.provider == config.provider
                        && favorite.remote.as_ref()
                            == config.remote.as_ref().map(|remote| &remote.host)
                        && favorite.account_directory == config.account_directory
                })
                .any(|favorite| favorite.model_id == model.id);
        }
    }

    /// Toggle a catalog model without changing the selected model or provider process.
    /// # Errors
    /// Returns invalid account/model identities or an atomic persistence failure.
    pub fn toggle_model_favorite(
        &self,
        config: &NativeSessionConfig,
        model: &str,
    ) -> Result<(), String> {
        config.validate()?;
        if model.is_empty() || model.len() > 8192 || model.chars().any(char::is_control) {
            return Err("Invalid favorite model identity".to_owned());
        }
        let favorite = ModelFavorite {
            remote: config.remote.as_ref().map(|remote| remote.host.clone()),
            provider: config.provider,
            account_directory: config.account_directory.clone(),
            model_id: model.to_owned(),
        };
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        if let Some(index) = candidate
            .model_favorites
            .iter()
            .position(|saved| saved == &favorite)
        {
            candidate.model_favorites.remove(index);
        } else {
            if candidate.model_favorites.len() >= 256 {
                return Err("Model favorites exceed 256 choices".to_owned());
            }
            candidate.model_favorites.push(favorite);
        }
        self.commit(candidate)
    }

    /// Toggle a model advertised by this live conversation, then return accepted favorites.
    /// # Errors
    /// Returns stale targets, discovery errors, unavailable models or persistence failures.
    pub fn favorite_model(
        &self,
        target: &CommandTarget,
        model: &str,
    ) -> Result<Vec<crate::NativeModelOption>, String> {
        let session = self.resolve(target)?;
        let mut models = session.models()?;
        if !models.iter().any(|option| option.id == model) {
            return Err("Selected model is not advertised by this provider account".to_owned());
        }
        self.toggle_model_favorite(&session.config, model)?;
        self.mark_model_favorites(&session.config, &mut models);
        Ok(models)
    }

    /// Save advertised next-prompt settings before publishing the new selection.
    /// # Errors
    /// Returns stale targets, unadvertised model/effort choices, or persistence errors.
    pub fn configure(
        &self,
        target: &CommandTarget,
        selection: &crate::NativeModelSelection,
    ) -> Result<NativeSessionConfig, String> {
        let session = self.resolve(target)?;
        let options = session.models()?;
        let option = options
            .iter()
            .find(|option| option.id == selection.model)
            .ok_or("Selected model is not advertised by this provider account")?;
        if selection
            .reasoning_effort
            .as_ref()
            .is_some_and(|effort| !option.reasoning_efforts.contains(effort))
        {
            return Err("Selected reasoning effort is not supported by this model".to_owned());
        }
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is unknown or stale")?;
        record.config.model = Some(selection.model.clone());
        record
            .config
            .reasoning_effort
            .clone_from(&selection.reasoning_effort);
        record.config.validate()?;
        let config = record.config.clone();
        self.commit(candidate)?;
        Ok(config)
    }

    /// Save the next-turn policy without interrupting active work. Idle changes resume with
    /// a fresh scoped tool lease; the captured approval request keeps its current policy.
    /// # Errors
    /// Returns stale targets, unsupported modes, or persistence failures.
    pub fn configure_permissions(
        &self,
        target: &CommandTarget,
        mode: crate::NativePermissionMode,
    ) -> Result<crate::native_permissions::NativePermissionUpdate, String> {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is unknown or stale")?;
        let session = lock(&self.live).get(&target.handle).cloned();
        let mut busy = false;
        if let Some(session) = &session {
            let snapshot = session.snapshot();
            busy = !matches!(
                snapshot.status,
                NativeSessionStatus::Idle
                    | NativeSessionStatus::Stopped
                    | NativeSessionStatus::Error
            );
            record.config.capture_session(&snapshot);
            record.snapshot = snapshot;
        }
        record.config.permissions = mode;
        record.config.validate()?;
        if busy {
            self.commit(candidate)?;
            return Ok(crate::native_permissions::NativePermissionUpdate::Queued);
        }
        record.snapshot.status = NativeSessionStatus::Stopped;
        record.snapshot.requests.clear();
        record.snapshot.working_since = None;
        self.commit(candidate)?;
        if let Some(session) = session {
            session.stop();
            lock(&self.live).remove(&target.handle);
        }
        Ok(crate::native_permissions::NativePermissionUpdate::Stopped)
    }

    /// # Errors
    /// Returns a stale/stopped target, or first-hand provider interrupt errors.
    pub fn interrupt(&self, target: &CommandTarget) -> Result<(), String> {
        self.resolve(target)?.interrupt()
    }

    /// # Errors
    /// Returns stale targets, unknown approval IDs, or provider transport errors.
    pub fn approve(&self, target: &CommandTarget, id: &str, allow: bool) -> Result<(), String> {
        self.resolve(target)?.approve(id, allow)
    }

    /// # Errors
    /// Returns stale targets, unavailable approval scopes, or transport errors.
    pub fn approve_decision(
        &self,
        target: &CommandTarget,
        id: &str,
        decision: crate::NativeApprovalDecision,
    ) -> Result<(), String> {
        self.resolve(target)?.approve_decision(id, decision)
    }

    /// # Errors
    /// Returns stale targets, unknown question IDs, or invalid provider responses.
    pub fn respond(
        &self,
        target: &CommandTarget,
        id: &str,
        response: serde_json::Value,
    ) -> Result<(), String> {
        self.resolve(target)?.respond(id, response)
    }

    /// Reserve identity durably before spawning. A failed launch remains retryable in place.
    /// # Errors
    /// Returns invalid captured inputs, persistence failures, or provider launch errors.
    pub fn create(
        &self,
        binding_id: &str,
        title: &str,
        config: NativeSessionConfig,
    ) -> Result<NativeSessionRecord, String> {
        self.create_captured(binding_id, None, title, config, None, None, None)
            .map_err(|error| error.message)
    }

    /// Attach to the exact persisted Bootty task captured by the invocation owner.
    /// # Errors
    /// Returns invalid membership, launch validation, persistence, or provider errors.
    pub fn create_for_task(
        &self,
        binding_id: &str,
        task_identity: &str,
        title: &str,
        config: NativeSessionConfig,
    ) -> Result<NativeSessionRecord, NativeCreationError> {
        self.create_captured(
            binding_id,
            Some(task_identity),
            title,
            config,
            None,
            None,
            None,
        )
    }

    /// Launch with an ephemeral bridge bound to the exact task terminal by the host.
    /// # Errors
    /// Returns invalid captured authority, persistence failures, or provider launch errors.
    pub fn create_for_task_with_tools(
        &self,
        binding_id: &str,
        task_identity: &str,
        title: &str,
        config: NativeSessionConfig,
        tools: Arc<crate::ToolBridge>,
    ) -> Result<NativeSessionRecord, NativeCreationError> {
        self.create_captured(
            binding_id,
            Some(task_identity),
            title,
            config,
            Some(tools),
            None,
            None,
        )
    }

    /// Create a native child using only the persisted parent's provider, account and project.
    /// # Errors
    /// Rejects stale parents or failed durable creation; tools require a live parent grant.
    pub fn create_spawned_for_task(
        &self,
        parent: &CommandTarget,
        task_identity: &str,
        title: &str,
        tools: Arc<crate::ToolBridge>,
    ) -> Result<NativeSessionRecord, NativeCreationError> {
        self.create_spawned_for_task_placed(parent, task_identity, title, tools, |_| Ok(()))
    }

    /// Reserve and publish the captured child's native pane before provider initialization.
    /// # Errors
    /// Rejects invalid parent authority, failed reservation, placement or provider startup.
    pub fn create_spawned_for_task_placed(
        &self,
        parent: &CommandTarget,
        task_identity: &str,
        title: &str,
        tools: Arc<crate::ToolBridge>,
        place: impl Fn(&NativeSessionRecord) -> Result<(), String>,
    ) -> Result<NativeSessionRecord, NativeCreationError> {
        if tools.lease().native_parent_target().as_ref() != Some(parent) {
            return Err("Native children require their exact live parent tool grant"
                .to_owned()
                .into());
        }
        let record = lock(&self.store)
            .records
            .iter()
            .find(|record| record.target() == *parent)
            .cloned()
            .ok_or_else(|| NativeCreationError::from("Spawning conversation changed".to_owned()))?;
        let mut config = record.config;
        config.session_id = None;
        config.session_file = None;
        config.fresh_session_id = None;
        self.create_captured(
            &record.binding_id,
            Some(task_identity),
            title,
            config,
            Some(tools),
            Some(CreationOrigin::Spawn(parent.clone())),
            Some(&place),
        )
    }

    /// Publish the real mux placement after reservation and before provider initialization.
    /// # Errors
    /// Returns reservation, placement or provider errors while retaining accepted identity.
    #[expect(
        clippy::too_many_arguments,
        reason = "Accepted input and placement share the creation boundary"
    )]
    pub fn create_for_task_placed(
        &self,
        binding_id: &str,
        task_identity: &str,
        title: &str,
        config: NativeSessionConfig,
        tools: Option<Arc<crate::ToolBridge>>,
        initial_message: &str,
        initial_attachments: &[PathBuf],
        place: impl Fn(&NativeSessionRecord) -> Result<(), String>,
    ) -> Result<NativeSessionRecord, NativeCreationError> {
        self.create_captured(
            binding_id,
            Some(task_identity),
            title,
            config,
            tools,
            if initial_message.trim().is_empty() && initial_attachments.is_empty() {
                None
            } else {
                if initial_message.len() > crate::MAX_NATIVE_PROMPT_TEXT_BYTES
                    || initial_message.contains('\0')
                    || initial_attachments.len() > MAX_NATIVE_PROMPT_ATTACHMENTS
                    || initial_attachments.iter().any(|path| !path.is_absolute())
                {
                    return Err("Invalid accepted initial input".to_owned().into());
                }
                if !initial_message.trim().is_empty() {
                    crate::NativePrompt::text(initial_message)?;
                }
                Some(CreationOrigin::InitialMessage {
                    message: initial_message.to_owned(),
                    attachments: initial_attachments.to_vec(),
                })
            },
            Some(&place),
        )
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "Captured creation keeps placement inside its durable acceptance boundary"
    )]
    fn create_captured(
        &self,
        binding_id: &str,
        task_identity: Option<&str>,
        title: &str,
        mut config: NativeSessionConfig,
        tools: Option<Arc<crate::ToolBridge>>,
        origin: Option<CreationOrigin>,
        place: Option<&NativePlacement<'_>>,
    ) -> Result<NativeSessionRecord, NativeCreationError> {
        config.prepare_fresh_identity()?;
        config.validate()?;
        validate_tools(config.provider, tools.as_deref())?;
        if task_identity.is_some_and(|identity| {
            identity.is_empty() || identity.len() > 8192 || identity.chars().any(char::is_control)
        }) {
            return Err(
                "Native conversation requires a captured task membership identity"
                    .to_owned()
                    .into(),
            );
        }
        if binding_id.is_empty()
            || binding_id.len() > 8192
            || binding_id.chars().any(char::is_control)
            || title.trim().is_empty()
            || title.len() > 256
            || title.chars().any(char::is_control)
        {
            return Err("Native session requires a captured binding and valid title"
                .to_owned()
                .into());
        }
        let _mutation = lock(&self.mutation);
        if self.shutdown.load(Ordering::Acquire) {
            return Err("Native agent host is shutting down".to_owned().into());
        }
        let mut candidate = lock(&self.store).clone();
        if candidate.records.len() >= 128 {
            return Err(
                "Native agent session limit reached; remove a stopped session"
                    .to_owned()
                    .into(),
            );
        }
        let generation = next_generation(&mut candidate)?;
        let mut record = NativeSessionRecord {
            id: format!("native:{}:{generation}", config.provider.default_program()),
            binding_id: binding_id.to_owned(),
            task_identity: task_identity.map(str::to_owned),
            title: title.to_owned(),
            pending_initial_message: match &origin {
                Some(CreationOrigin::InitialMessage { message, .. }) => Some(message.clone()),
                _ => None,
            },
            generation,
            snapshot: NativeSessionSnapshot::new(config.provider),
            config,
            permissions_pending: false,
            attachments: Vec::new(),
            side_chat: None,
            spawn_parent: None,
        };
        if let Some(CreationOrigin::InitialMessage { attachments, .. }) = &origin {
            for path in attachments {
                match self.attachment_store.import(&record.id, path) {
                    Ok(stored) => {
                        let bytes = record
                            .attachments
                            .iter()
                            .map(|reference| reference.size_bytes)
                            .sum::<u64>();
                        if bytes.saturating_add(stored.reference.size_bytes)
                            > MAX_NATIVE_SESSION_ATTACHMENT_BYTES
                        {
                            _ = self.attachment_store.remove_session(&record.id);
                            return Err("Native session attachments exceed 512 MiB"
                                .to_owned()
                                .into());
                        }
                        record.attachments.push(stored.reference);
                    }
                    Err(error) => {
                        _ = self.attachment_store.remove_session(&record.id);
                        return Err(error.into());
                    }
                }
            }
        }
        if let Some(CreationOrigin::Spawn(parent)) = &origin {
            if !candidate
                .records
                .iter()
                .any(|source| source.target() == *parent && source.binding_id == binding_id)
            {
                return Err("Spawning conversation changed during creation"
                    .to_owned()
                    .into());
            }
            record.spawn_parent = Some(parent.clone());
        }
        if let Some(CreationOrigin::SideChat(fork)) = origin {
            if !candidate
                .records
                .iter()
                .any(|source| source.target() == fork.source)
            {
                return Err(
                    "The source conversation changed while creating the side chat"
                        .to_owned()
                        .into(),
                );
            }
            self.attachment_store.copy_session(
                &fork.origin.source_id,
                &record.id,
                &fork.attachments,
            )?;
            record
                .snapshot
                .transcript
                .clone_from(&fork.origin.transcript);
            record.side_chat = Some(fork.origin);
            record.attachments = fork.attachments;
        }
        candidate.records.push(record.clone());
        if let Err(error) = self.commit(candidate) {
            if record.side_chat.is_some() || !record.attachments.is_empty() {
                _ = self.attachment_store.remove_session(&record.id);
            }
            return Err(error.into());
        }
        if let Some(place) = place
            && let Err(message) = place(&record)
        {
            let mut candidate = lock(&self.store).clone();
            if let Some(saved) = candidate
                .records
                .iter_mut()
                .find(|saved| saved.id == record.id)
            {
                saved.snapshot.status = NativeSessionStatus::Error;
                saved.snapshot.error = Some(bounded_text(message.clone()));
            }
            self.commit(candidate)?;
            return Err(NativeCreationError {
                message,
                target: Some(record.target()),
            });
        }
        self.launch(&record.id, tools)
            .map_err(|message| NativeCreationError {
                message,
                target: Some(record.target()),
            })
    }

    fn launch(
        &self,
        id: &str,
        tools: Option<Arc<crate::ToolBridge>>,
    ) -> Result<NativeSessionRecord, String> {
        let record = lock(&self.store)
            .records
            .iter()
            .find(|record| record.id == id)
            .cloned()
            .ok_or("Unknown native agent session")?;
        if let Some(tools) = &tools {
            tools.lease().bind_native_session(record.target())?;
        }
        let attachment_directory = if record.config.provider != crate::AgentKind::Claude {
            None
        } else if let Some(remote) = &record.config.remote {
            let directory = remote_attachment_directory(&record.config)?;
            Some(
                directory
                    .prepare_remote(
                        &bootty_host::remote::RemoteHost::new(remote.host.clone()),
                        &attachment_runner(Arc::clone(&self.shutdown))?,
                    )
                    .map_err(|error| format!("Prepare remote attachments: {error:#}"))?,
            )
        } else {
            Some(self.attachment_store.prepare_session(&record.id)?)
        };
        let started = NativeAgentSession::start_with_tools(
            record.config.clone(),
            Arc::clone(&self.clock),
            tools,
            attachment_directory.as_deref(),
        )
        .map(Arc::new)
        .and_then(|session| {
            let publisher = self.publication.clone();
            session.set_change_handler(Arc::new(move || {
                let _ = publisher.try_send(());
            }));
            {
                let mut live = lock(&self.live);
                if self.shutdown.load(Ordering::Acquire) {
                    return Err("Native agent host is shutting down".to_owned());
                }
                live.insert(id.to_owned(), Arc::clone(&session));
            }
            let empty_reservation = !record.snapshot.completed_turn
                && record.snapshot.first_turn.is_none()
                && record.snapshot.pending_user_item.is_none()
                && record
                    .snapshot
                    .transcript
                    .iter()
                    .all(|item| item.id.starts_with("fork:"))
                && record
                    .side_chat
                    .as_ref()
                    .is_none_or(|fork| fork.seeded_identity.is_none());
            session.initialize_with_empty_restore(empty_reservation)?;
            Ok(session)
        });
        match started {
            Ok(session) => {
                if let Some(fork) = &record.side_chat {
                    session.restore_fork_history(&record.snapshot, fork);
                } else {
                    session.restore_recent_history(&record.snapshot);
                }
                let snapshot = session.snapshot();
                let mut candidate = lock(&self.store).clone();
                let record = candidate
                    .records
                    .iter_mut()
                    .find(|record| record.id == id)
                    .ok_or("Native agent reservation disappeared")?;
                record.config.capture_session(&snapshot);
                record.snapshot = snapshot;
                let result = record.clone();
                if let Err(error) = self.commit(candidate) {
                    session.stop();
                    lock(&self.live).remove(id);
                    return Err(error);
                }
                Ok(result)
            }
            Err(error) => {
                lock(&self.live).remove(id);
                let mut candidate = lock(&self.store).clone();
                if let Some(record) = candidate.records.iter_mut().find(|record| record.id == id) {
                    record.snapshot.error = Some(bounded_text(error.clone()));
                    record.snapshot.status = NativeSessionStatus::Error;
                    record.snapshot.working_since = None;
                }
                self.commit(candidate)?;
                Err(error)
            }
        }
    }

    /// # Errors
    /// Returns an unknown/live-session error or provider resume failure.
    pub fn resume(&self, target: &CommandTarget) -> Result<NativeSessionRecord, String> {
        self.resume_captured(target, None)
    }

    /// Resume using a fresh host-issued attachment; stopped runtime configuration is never reused.
    /// # Errors
    /// Returns stale targets, invalid tool authority, persistence failures, or resume errors.
    pub fn resume_with_tools(
        &self,
        target: &CommandTarget,
        tools: Arc<crate::ToolBridge>,
    ) -> Result<NativeSessionRecord, String> {
        self.resume_captured(target, Some(tools))
    }

    fn resume_captured(
        &self,
        target: &CommandTarget,
        tools: Option<Arc<crate::ToolBridge>>,
    ) -> Result<NativeSessionRecord, String> {
        let _mutation = lock(&self.mutation);
        if self.shutdown.load(Ordering::Acquire) {
            return Err("Native agent host is shutting down".to_owned());
        }
        let provider = lock(&self.store)
            .records
            .iter()
            .find(|record| record.target() == *target)
            .map(|record| record.config.provider)
            .ok_or("Native session target is unknown or stale")?;
        validate_tools(provider, tools.as_deref())?;
        {
            let mut live = lock(&self.live);
            if let Some(session) = live.get(&target.handle) {
                if !matches!(
                    session.snapshot().status,
                    NativeSessionStatus::Stopped | NativeSessionStatus::Error
                ) {
                    return Err("Native session already has a live process".to_owned());
                }
                session.stop();
                live.remove(&target.handle);
            }
        }
        let mut candidate = lock(&self.store).clone();
        let generation = next_generation(&mut candidate)?;
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.target() == *target)
            .ok_or("Unknown native agent session")?;
        record.config.validate()?;
        record.generation = generation;
        // Revisions belong to one process generation, while retained history belongs to the task.
        record.snapshot.revision = 0;
        record.snapshot.status = NativeSessionStatus::Starting;
        record.snapshot.working_since = None;
        record.snapshot.turn_id = None;
        record.snapshot.requests.clear();
        record.snapshot.error = None;
        record.snapshot.pending_user_item = None;
        record.snapshot.first_turn = None;
        self.commit(candidate)?;
        self.launch(&target.handle, tools)
    }

    #[must_use]
    pub fn sessions(&self) -> Vec<NativeSessionRecord> {
        // The publication worker commits before replacing this immutable UI projection.
        let mut records = lock(&self.store).records.clone();
        let live = lock(&self.live);
        for record in &mut records {
            // Grants are live projections, never restored from the durable catalog.
            record.permissions_pending = live
                .get(&record.id)
                .is_some_and(|session| session.config.permissions != record.config.permissions);
            record.snapshot.browser_access = live
                .get(&record.id)
                .map_or(crate::NativeBrowserAccess::Unavailable, |session| {
                    session.browser_access()
                });
        }
        records
    }

    #[must_use]
    pub fn activities(&self) -> Vec<NativeSessionActivity> {
        let now = (self.clock)();
        lock(&self.store)
            .records
            .iter()
            .map(|record| NativeSessionActivity {
                id: record.id.clone(),
                title: record.title.clone(),
                generation: record.generation,
                spawn_parent: record.spawn_parent.clone(),
                binding_id: record.binding_id.clone(),
                task_identity: record.task_identity.clone(),
                provider: record.config.provider,
                status: record.snapshot.status,
                completed_turn: record.snapshot.completed_turn,
                first_turn: record.snapshot.first_turn.clone(),
                approval: record.snapshot.requests.iter().any(|request| {
                    request.is_mcp_approval()
                        || (request.method == "claude.permission"
                            && crate::native_protocol::field(&request.parameters, "tool_name")
                                != "AskUserQuestion")
                        || matches!(
                            request.method.as_str(),
                            "item/commandExecution/requestApproval"
                                | "item/fileChange/requestApproval"
                                | "pi.confirm"
                        )
                }),
                input: record.snapshot.requests.iter().any(|request| {
                    (request.method == "mcpServer/elicitation/request"
                        && !request.is_mcp_approval())
                        || (request.method == "claude.permission"
                            && crate::native_protocol::field(&request.parameters, "tool_name")
                                == "AskUserQuestion")
                        || matches!(
                            request.method.as_str(),
                            "item/tool/requestUserInput" | "pi.select" | "pi.input" | "pi.editor"
                        )
                }),
                working_elapsed: record.snapshot.working_elapsed(now),
            })
            .collect()
    }

    /// Read compact accepted status for the exact captured conversation, without its transcript.
    /// # Errors
    /// Rejects unknown or stale conversation identities.
    pub fn activity(&self, target: &CommandTarget) -> Result<NativeSessionActivity, String> {
        if target.kind != ResourceKind::Session {
            return Err("Agent status requires an exact Session target".into());
        }
        self.activities()
            .into_iter()
            .find(|activity| {
                (&activity.id, activity.generation) == (&target.handle, target.generation)
            })
            .ok_or_else(|| "Native session target is unknown or stale".into())
    }

    /// Read the exact conversation's persisted provider selection, including while stopped.
    /// # Errors
    /// Rejects unknown, stale or non-session targets.
    pub fn provider_info(&self, target: &CommandTarget) -> Result<NativeProviderInfo, String> {
        let store = lock(&self.store);
        let record = store
            .records
            .iter()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is unknown or stale")?;
        let config = &record.config;
        let info = NativeProviderInfo {
            provider: config.provider,
            profile: config.profile.clone(),
            model: config.model.clone(),
            reasoning_effort: config.reasoning_effort.clone(),
            fast_mode: config.fast_mode,
            // Legacy provider-default intent remains unknown until explicitly configured.
            permissions: config.permissions,
            permission_modes: crate::NativePermissionMode::ALL
                .into_iter()
                .filter(|mode| {
                    *mode != crate::NativePermissionMode::ProviderDefault
                        && mode.supports(config.provider)
                })
                .collect(),
        };
        drop(store);
        Ok(info)
    }

    #[must_use]
    pub fn activities_for_binding(&self, binding_id: &str) -> Vec<NativeSessionActivity> {
        self.activities()
            .into_iter()
            .filter(|record| record.binding_id == binding_id)
            .collect()
    }

    /// Resolve the exact host-issued identity and generation, never the selected session.
    /// # Errors
    /// Returns stale, mismatched, stopped or unknown target errors.
    pub fn resolve(&self, target: &CommandTarget) -> Result<Arc<NativeAgentSession>, String> {
        if target.kind != ResourceKind::Session {
            return Err("Native agents require a Session target".to_owned());
        }
        let store = lock(&self.store);
        let record = store
            .records
            .iter()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is unknown or stale")?;
        // Keep generation validation and process lookup under the same catalog lease.
        let session = lock(&self.live)
            .get(&record.id)
            .cloned()
            .ok_or_else(|| "Native session is stopped; resume it explicitly".to_owned());
        drop(store);
        session
    }

    /// # Errors
    /// Returns stale targets or persistence failures. The stopped transcript remains in history.
    pub fn stop(&self, target: &CommandTarget) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        let session = self.resolve(target)?;
        session.stop();
        let mut candidate = lock(&self.store).clone();
        if let Some(record) = candidate
            .records
            .iter_mut()
            .find(|record| record.id == target.handle)
        {
            record.snapshot = session.snapshot();
            record.config.capture_session(&record.snapshot);
        }
        self.commit(candidate)?;
        lock(&self.live).remove(&target.handle);
        Ok(())
    }

    /// Supervise only a child created under this exact parent generation.
    /// # Errors
    /// Rejects changed identity, foreign provenance and cancelled authority before acting.
    pub fn control_spawned_child(
        &self,
        parent: &CommandTarget,
        request: &crate::ToolChildControlRequest,
        cancellation: &bootty_control::CommandCancellation,
    ) -> Result<NativeSessionActivity, String> {
        request.validate()?;
        let child = request.target();
        let store = lock(&self.store);
        let parent = store
            .records
            .iter()
            .find(|record| record.target() == *parent)
            .ok_or("Parent conversation changed")?;
        let record = store
            .records
            .iter()
            .find(|record| record.target() == child)
            .ok_or("Child conversation changed")?;
        if record.spawn_parent.as_ref() != Some(&parent.target())
            || record.binding_id != parent.binding_id
        {
            return Err(
                "Supervision is limited to children created under this parent grant".into(),
            );
        }
        let stopped = record.snapshot.status == NativeSessionStatus::Stopped;
        drop(store);
        // Withdrawal cancels queued operations; an accepted shared command finishes.
        if cancellation.is_cancelled() {
            return Err("Parent child-operation authority was cancelled".into());
        }
        match request.operation {
            crate::ToolChildOperation::Interrupt => self.interrupt(&child),
            crate::ToolChildOperation::Stop if stopped => Ok(()),
            crate::ToolChildOperation::Stop => self.stop(&child),
        }?;
        self.activity(&child)
    }

    /// Save recent native transcript views and provider selectors. Full transcript history remains
    /// provider-owned; this checkpoint keeps the application history readable while stopped.
    /// # Errors
    /// Returns write failures without replacing the durable catalog.
    pub fn checkpoint(&self) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        let live = lock(&self.live);
        for record in &mut candidate.records {
            if let Some(session) = live.get(&record.id) {
                record.snapshot = session.snapshot();
                record.config.capture_session(&record.snapshot);
            }
        }
        drop(live);
        self.commit(candidate)
    }

    /// # Errors
    /// Returns unknown/live-session errors or failed persistence.
    pub fn remove(&self, target: &CommandTarget) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        if self.shutdown.load(Ordering::Acquire) {
            return Err("Native agent host is shutting down".to_owned());
        }
        if lock(&self.live).contains_key(&target.handle) {
            return Err("Stop the native session before removing it".to_owned());
        }
        let mut candidate = lock(&self.store).clone();
        let removed_id = candidate
            .records
            .iter()
            .find(|record| record.target() == *target)
            .map(|record| record.id.clone())
            .ok_or("Native session target is unknown or stale")?;
        let before = candidate.records.len();
        candidate
            .records
            .retain(|record| record.target() != *target);
        if candidate.records.len() == before {
            return Err("Native session target is unknown or stale".to_owned());
        }
        self.commit(candidate)?;
        self.attachment_store.remove_session(&removed_id)
    }

    /// Stop owned processes before saving their final state. Call on a shutdown worker and await
    /// completion before releasing the window or exiting the host.
    /// # Errors
    /// Returns final catalog persistence failures after every owned process has been stopped.
    pub fn shutdown(&self) -> Result<(), String> {
        self.shutdown.store(true, Ordering::Release);
        let sessions = std::mem::take(&mut *lock(&self.live));
        for session in sessions.values() {
            session.stop();
        }
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        for record in &mut candidate.records {
            if let Some(session) = sessions.get(&record.id) {
                record.snapshot = session.snapshot();
                record.config.capture_session(&record.snapshot);
            }
        }
        self.commit(candidate)?;
        let _ = self.publication.try_send(());
        Ok(())
    }

    fn commit(&self, candidate: Store) -> Result<(), String> {
        let bytes = serde_json::to_vec(&candidate).map_err(|error| error.to_string())?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("Native agent catalog exceeds 16 MiB".to_owned());
        }
        write_private(&self.path, &bytes)?;
        *lock(&self.store) = candidate;
        publish(&self.revision, &self.change_handler);
        Ok(())
    }
}

fn remote_attachment_directory(
    config: &NativeSessionConfig,
) -> Result<bootty_host::private_files::PrivateFileDirectory, String> {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let identity = config
        .session_id
        .as_deref()
        .or(config.fresh_session_id.as_deref())
        .ok_or("Remote file attachments need the provider's durable conversation identity")?;
    let mut owner = String::with_capacity(64);
    for byte in Sha256::digest(format!("{}:{identity}", config.provider.module())) {
        write!(&mut owner, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    Ok(bootty_host::private_files::PrivateFileDirectory {
        root: config
            .account_directory
            .clone()
            .ok_or("Missing captured account directory")?,
        owner,
    })
}

fn attachment_runner(
    shutdown: Arc<AtomicBool>,
) -> Result<bootty_host::CancellableCommandRunner, String> {
    Ok(
        bootty_host::CancellableCommandRunner::with_deadline_and_cancellation_check(
            bootty_host::CommandCancellation::default(),
            Instant::now()
                .checked_add(Duration::from_secs(120))
                .ok_or("Attachment deadline exceeds the clock range")?,
            move || shutdown.load(Ordering::Acquire),
        ),
    )
}

fn next_generation(store: &mut Store) -> Result<u64, String> {
    store.next_generation = store
        .next_generation
        .checked_add(1)
        .ok_or("Native session generation exhausted")?;
    Ok(store.next_generation)
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

impl Drop for NativeAgentService {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("Native agent shutdown state was not saved: {error}");
        }
    }
}

struct PublicationState {
    path: PathBuf,
    mutation: Arc<Mutex<()>>,
    store: Arc<Mutex<Store>>,
    live: Arc<Mutex<BTreeMap<String, Arc<NativeAgentSession>>>>,
    change_handler: Arc<Mutex<Option<NativeChangeHandler>>>,
    revision: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
}

fn start_publication_worker(
    state: PublicationState,
    receiver: mpsc::Receiver<()>,
) -> Result<(), String> {
    thread::Builder::new()
        .name("native-agent-history".to_owned())
        .spawn(move || {
            while receiver.recv().is_ok() {
                if state.shutdown.load(Ordering::Acquire) {
                    return;
                }
                {
                    let _mutation = lock(&state.mutation);
                    let mut candidate = lock(&state.store).clone();
                    let mut changed = false;
                    {
                        let live = lock(&state.live);
                        for record in &mut candidate.records {
                            if let Some(session) = live.get(&record.id) {
                                let snapshot = session.snapshot();
                                if snapshot.revision != record.snapshot.revision {
                                    record.config.capture_session(&snapshot);
                                    record.snapshot = snapshot;
                                    changed = true;
                                }
                            }
                        }
                    }
                    if !changed {
                        continue;
                    }
                    {
                        let result = serde_json::to_vec(&candidate)
                            .map_err(|error| error.to_string())
                            .and_then(|bytes| {
                                if bytes.len() > 16 * 1024 * 1024 {
                                    return Err("Native agent catalog exceeds 16 MiB".to_owned());
                                }
                                write_private(&state.path, &bytes)
                            });
                        match result {
                            Ok(()) => *lock(&state.store) = candidate,
                            Err(error) => {
                                eprintln!("native agent history could not be saved: {error}");
                                continue;
                            }
                        }
                    }
                }
                publish(&state.revision, &state.change_handler);
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn publish(revision: &AtomicU64, handler: &Mutex<Option<NativeChangeHandler>>) {
    revision.fetch_add(1, Ordering::Release);
    let callback = lock(handler).clone();
    if let Some(callback) = callback {
        callback();
    }
}

fn validate_tools(
    provider: crate::AgentKind,
    tools: Option<&crate::ToolBridge>,
) -> Result<(), String> {
    if let Some(tools) = tools
        && (tools.lease().scope().provider != provider || tools.lease().terminal_target().is_none())
    {
        tools.stop();
        return Err(
            "Native tool attachment requires its provider and a bound live task terminal"
                .to_owned(),
        );
    }
    Ok(())
}

impl NativeAgentService {
    /// # Errors
    /// Rejects stopped, stale, inherited or disabled conversation grants.
    pub fn attach_browser(
        &self,
        target: &CommandTarget,
        attachment: Option<crate::NativeBrowserAttachment>,
    ) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        self.resolve(target)?.attach_browser(target, attachment)?;
        publish(&self.revision, &self.change_handler);
        Ok(())
    }
    /// # Errors
    /// Rejects stale sessions, unattached tools or an unmentioned application.
    pub fn application_access(
        &self,
        target: &CommandTarget,
        reference: &str,
        caller: bootty_control::Caller,
    ) -> Result<crate::NativeApplicationAccess, String> {
        self.resolve(target)?
            .application_access(target, reference, caller)
    }
}

fn validate_model_catalog_cache(store: &Store) -> Result<(), String> {
    if store.model_catalogs.len() > 16
        || store
            .model_catalogs
            .keys()
            .any(|key| key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || store.model_catalog_aliases.len() > 16
        || store.model_catalog_aliases.iter().any(|(alias, key)| {
            alias.len() != 64
                || !alias.bytes().all(|byte| byte.is_ascii_hexdigit())
                || key.len() != 64
                || !key.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !store.model_catalogs.contains_key(key)
        })
    {
        return Err("Invalid native model catalog cache".to_owned());
    }
    Ok(())
}

fn model_catalog_key(config: &NativeSessionConfig) -> Result<String, serde_json::Error> {
    let mut account = config.clone();
    account.session_id = None;
    account.session_file = None;
    account.fresh_session_id = None;
    account.model = None;
    account.reasoning_effort = None;
    account.fast_mode = false;
    let encoded = serde_json::to_vec(&account)?;
    Ok(catalog_cache_digest(&encoded))
}

fn model_catalog_invocation_key(
    invocation: &CommandInvocation,
    preferences: &bootty_config::config::AgentProviderConfig,
    remote: Option<&bootty_config::config::RemoteConfig>,
) -> Option<String> {
    if invocation.command != "agents.native.catalog-info" {
        return None;
    }
    let mut identity = invocation.clone();
    if identity
        .target
        .as_ref()
        .is_some_and(|target| target.kind == ResourceKind::Binding)
    {
        // Cached metadata follows provider/account/project, not process-local control targets.
        identity.target = None;
    }
    if identity
        .arguments
        .get(6)
        .is_some_and(|argument| argument == "refresh")
    {
        identity.arguments.truncate(6);
    }
    serde_json::to_vec(&(identity, preferences, remote))
        .ok()
        .map(|encoded| catalog_cache_digest(&encoded))
}

fn catalog_cache_digest(encoded: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    use std::fmt::Write as _;
    let mut key = String::with_capacity(64);
    for byte in Sha256::digest(encoded) {
        _ = write!(key, "{byte:02x}");
    }
    key
}

fn validate_pending_message(message: Option<&str>, has_attachments: bool) -> Result<(), String> {
    let Some(message) = message else {
        return Ok(());
    };
    if message.len() > crate::MAX_NATIVE_PROMPT_TEXT_BYTES || message.contains('\0') {
        return Err("Invalid saved initial input".into());
    }
    if !message.trim().is_empty() {
        crate::NativePrompt::text(message)?;
    } else if !has_attachments {
        return Err("Pending input has no message or attachments".into());
    }
    Ok(())
}
