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
};

use bootty_control::{CommandTarget, ResourceKind};
use bootty_write::{NewFileMode, WriteTarget};
use serde::{Deserialize, Serialize};

use crate::{
    NativeAgentSession, NativeChangeHandler, NativeSessionConfig, NativeSessionSnapshot,
    NativeSessionStatus, native_session::lock,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeSessionRecord {
    pub id: String,
    pub binding_id: String,
    pub title: String,
    pub generation: u64,
    pub config: NativeSessionConfig,
    pub snapshot: NativeSessionSnapshot,
}

impl NativeSessionRecord {
    #[must_use]
    pub fn target(&self) -> CommandTarget {
        CommandTarget {
            kind: ResourceKind::Session,
            handle: self.id.clone(),
            generation: self.generation,
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Store {
    next_generation: u64,
    records: Vec<NativeSessionRecord>,
}

/// App-owned native session catalog. All mutations run on workers. Only provider ids/configuration
/// persist; credentials remain with the provider. Restored sessions resume explicitly.
pub struct NativeAgentService {
    path: PathBuf,
    mutation: Arc<Mutex<()>>,
    store: Arc<Mutex<Store>>,
    live: Arc<Mutex<BTreeMap<String, Arc<NativeAgentSession>>>>,
    change_handler: Arc<Mutex<Option<NativeChangeHandler>>>,
    revision: Arc<AtomicU64>,
    publication: mpsc::SyncSender<()>,
    shutdown: Arc<AtomicBool>,
}

impl NativeAgentService {
    /// # Errors
    /// Returns malformed/oversized state or inability to create the private storage directory.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
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
        for record in &mut store.records {
            record.snapshot.status = NativeSessionStatus::Stopped;
            record.snapshot.requests.clear();
            record.snapshot.turn_id = None;
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
            path,
            mutation,
            store,
            live,
            change_handler,
            revision,
            publication,
            shutdown,
        })
    }

    pub fn set_change_handler(&self, handler: NativeChangeHandler) {
        *lock(&self.change_handler) = Some(handler);
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
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

    /// # Errors
    /// Returns invalid titles, stale targets or persistence failures.
    pub fn rename(&self, target: &CommandTarget, title: &str) -> Result<(), String> {
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
        title.clone_into(&mut record.title);
        self.commit(candidate)
    }

    /// Submit through the exact native process owner and save the first prompt's title.
    /// # Errors
    /// Returns stale-target, provider rejection or durable catalog errors.
    pub fn prompt(
        &self,
        target: &CommandTarget,
        message: &str,
    ) -> Result<NativeSessionSnapshot, String> {
        let _mutation = lock(&self.mutation);
        let session = self.resolve(target)?;
        session.send_prompt(message)?;
        let mut candidate = lock(&self.store).clone();
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.target() == *target)
            .ok_or("Native session target is stale")?;
        if !record
            .snapshot
            .transcript
            .iter()
            .any(|item| item.role == "user")
            && let Some(line) = message.lines().find(|line| !line.trim().is_empty())
        {
            record.title = line.trim().chars().take(64).collect();
        }
        record.snapshot = session.snapshot();
        record.config.session_id = record
            .snapshot
            .session_file
            .clone()
            .or_else(|| record.snapshot.session_id.clone());
        let snapshot = record.snapshot.clone();
        self.commit(candidate)?;
        Ok(snapshot)
    }

    /// Reserve identity durably before spawning the child. The returned target is independent
    /// from the provider's thread id and belongs to the captured local binding.
    /// # Errors
    /// Returns invalid inputs, persistence, provider launch or handshake errors.
    pub fn create(
        &self,
        binding_id: &str,
        title: &str,
        mut config: NativeSessionConfig,
    ) -> Result<NativeSessionRecord, String> {
        if binding_id.is_empty() || binding_id.len() > 8192 || title.is_empty() || title.len() > 256
        {
            return Err("Native session needs a binding and title of at most 256 bytes".to_owned());
        }
        let _mutation = lock(&self.mutation);
        if config.provider == crate::AgentKind::Claude && config.session_id.is_none() {
            // The CLI accepts a client-issued UUID, so identity exists before the first prompt.
            let id = uuid::Uuid::new_v4().to_string();
            config.arguments.extend(["--session-id".to_owned(), id]);
        }
        let mut candidate = lock(&self.store).clone();
        if candidate.records.len() >= 128 {
            return Err("Native agent session limit reached; remove a stopped session".to_owned());
        }
        let generation = next_generation(&mut candidate)?;
        let launch = config.clone();
        let retained = crate::AgentLaunch {
            program: config.program.clone(),
            cwd: Some(config.cwd.to_string_lossy().into_owned()),
            arguments: config.arguments.clone(),
            ephemeral: false,
        }
        .retained(config.provider);
        config.arguments = retained.arguments;
        let record = NativeSessionRecord {
            id: format!("native:{}:{}", config.provider, uuid::Uuid::new_v4()),
            binding_id: binding_id.to_owned(),
            title: title.to_owned(),
            generation,
            snapshot: NativeSessionSnapshot::new(config.provider),
            config,
        };
        candidate.records.push(record.clone());
        self.commit(candidate)?;
        self.launch(&record.id, Some(launch))
    }

    fn launch(
        &self,
        id: &str,
        config: Option<NativeSessionConfig>,
    ) -> Result<NativeSessionRecord, String> {
        let record = lock(&self.store)
            .records
            .iter()
            .find(|record| record.id == id)
            .cloned()
            .ok_or("Unknown native agent session")?;
        match NativeAgentSession::spawn(config.unwrap_or(record.config)) {
            Ok(session) => {
                session.restore_recent_history(&record.snapshot);
                let publisher = self.publication.clone();
                session.set_change_handler(Arc::new(move || {
                    let _ = publisher.try_send(());
                }));
                let snapshot = session.snapshot();
                let mut candidate = lock(&self.store).clone();
                let record = candidate
                    .records
                    .iter_mut()
                    .find(|record| record.id == id)
                    .ok_or("Native agent reservation disappeared")?;
                record.config.session_id = snapshot
                    .session_file
                    .clone()
                    .or_else(|| snapshot.session_id.clone());
                if record.config.provider == crate::AgentKind::Claude {
                    // --session-id belongs only to creation; resume uses the provider selector.
                    if let Some(index) = record
                        .config
                        .arguments
                        .iter()
                        .position(|argument| argument == "--session-id")
                    {
                        record.config.arguments.drain(
                            index..(index.saturating_add(2)).min(record.config.arguments.len()),
                        );
                    }
                }
                record.snapshot = snapshot;
                let result = record.clone();
                self.commit(candidate)?;
                lock(&self.live).insert(id.to_owned(), Arc::new(session));
                Ok(result)
            }
            Err(error) => {
                let mut candidate = lock(&self.store).clone();
                if let Some(record) = candidate.records.iter_mut().find(|record| record.id == id) {
                    record.snapshot.error = Some(error.clone());
                    record.snapshot.status = NativeSessionStatus::Error;
                }
                self.commit(candidate)?;
                Err(error)
            }
        }
    }

    /// # Errors
    /// Returns an unknown/live-session error or provider resume failure.
    pub fn resume(&self, id: &str) -> Result<NativeSessionRecord, String> {
        let _mutation = lock(&self.mutation);
        if lock(&self.live).contains_key(id) {
            return Err("Native session already has a live process".to_owned());
        }
        let mut candidate = lock(&self.store).clone();
        let generation = next_generation(&mut candidate)?;
        let record = candidate
            .records
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or("Unknown native agent session")?;
        record.generation = generation;
        record.snapshot.status = NativeSessionStatus::Starting;
        self.commit(candidate)?;
        self.launch(id, None)
    }

    #[must_use]
    pub fn sessions(&self) -> Vec<NativeSessionRecord> {
        let mut records = lock(&self.store).records.clone();
        let live = lock(&self.live);
        for record in &mut records {
            if let Some(session) = live.get(&record.id) {
                record.snapshot = session.snapshot();
            }
        }
        records
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
        let id = record.id.clone();
        drop(store);
        lock(&self.live)
            .get(&id)
            .cloned()
            .ok_or_else(|| "Native session is stopped; resume it explicitly".to_owned())
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
        }
        self.commit(candidate)?;
        lock(&self.live).remove(&target.handle);
        Ok(())
    }

    /// Save recent native transcript views and provider selectors. Full transcript history remains
    /// provider-owned; this checkpoint keeps the application history readable while stopped.
    /// # Errors
    /// Returns write failures without replacing the durable catalog.
    pub fn checkpoint(&self) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        let mut candidate = lock(&self.store).clone();
        candidate.records = self.sessions();
        for record in &mut candidate.records {
            record.config.session_id = record
                .snapshot
                .session_file
                .clone()
                .or_else(|| record.snapshot.session_id.clone());
        }
        self.commit(candidate)
    }

    /// # Errors
    /// Returns unknown/live-session errors or failed persistence.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let _mutation = lock(&self.mutation);
        if lock(&self.live).contains_key(id) {
            return Err("Stop the native session before removing it".to_owned());
        }
        let mut candidate = lock(&self.store).clone();
        let before = candidate.records.len();
        candidate.records.retain(|record| record.id != id);
        if candidate.records.len() == before {
            return Err("Unknown native session".to_owned());
        }
        self.commit(candidate)
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
        self.shutdown.store(true, Ordering::Release);
        let _ = self.publication.try_send(());
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
                let shutdown = state.shutdown.load(Ordering::Acquire);
                {
                    let _mutation = lock(&state.mutation);
                    let mut candidate = lock(&state.store).clone();
                    let mut changed = false;
                    {
                        let live = lock(&state.live);
                        for record in &mut candidate.records {
                            if let Some(session) = live.get(&record.id) {
                                let snapshot = session.snapshot();
                                if (shutdown
                                    || matches!(
                                        snapshot.status,
                                        NativeSessionStatus::Idle
                                            | NativeSessionStatus::Stopped
                                            | NativeSessionStatus::Error
                                    ))
                                    && snapshot.revision != record.snapshot.revision
                                {
                                    record.config.session_id = snapshot
                                        .session_file
                                        .clone()
                                        .or_else(|| snapshot.session_id.clone());
                                    record.snapshot = snapshot;
                                    changed = true;
                                }
                            }
                        }
                    }
                    if changed {
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
                            }
                        }
                    }
                }
                if shutdown {
                    lock(&state.live).clear();
                    return;
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
