//! Agent state that outlives one Bootty process.
//!
//! Hooks only report changes, so an agent that stays idle across a restart would vanish until it
//! acts again. The service keeps the last reported facts in a private file and restores them,
//! marked `restored`, until the pane reports again.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    AgentLaunch,
    provider::{AgentAttention, AgentKind, AgentPaneKey, AgentSource, AgentState, AgentStatus},
};

const VERSION: u32 = 1;
/// A burst of hook events costs one write: the writer waits this long for the burst to settle.
const WRITE_DELAY: Duration = Duration::from_secs(1);
/// The longest a failing write waits before trying again.
const RETRY_LIMIT: Duration = Duration::from_secs(60);

/// Pane states as a service last held them.
pub struct Snapshot {
    pub attention_sequence: u64,
    pub panes: Vec<(AgentPaneKey, AgentState)>,
}

/// One writer per state file for the whole process, shared by every service that persists there
/// and alive after they drop. Snapshots therefore reach disk in the order they were taken: a
/// retiring window can never overwrite its replacement's newer state, and a failed write keeps
/// retrying instead of dying with its window.
fn writers() -> &'static Mutex<HashMap<PathBuf, StateWriter>> {
    static WRITERS: OnceLock<Mutex<HashMap<PathBuf, StateWriter>>> = OnceLock::new();
    WRITERS.get_or_init(Mutex::default)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    attention_sequence: u64,
    panes: Vec<PaneRecord>,
}

#[derive(Serialize, Deserialize)]
struct PaneRecord {
    provider: AgentKind,
    scope: String,
    pane: String,
    status: String,
    attention: Option<AgentAttention>,
    attention_sequence: u64,
    acknowledged_sequence: u64,
    session_id: Option<String>,
    session_file: Option<String>,
    session_name: Option<String>,
    thread_id: Option<String>,
    turn_id: Option<String>,
    last_event: Option<String>,
    last_message: Option<String>,
    turn_ended_at: Option<u64>,
    error: Option<String>,
    cwd: Option<String>,
    launch: Option<AgentLaunch>,
    server: Option<String>,
}

/// Pane states read back from a previous process.
pub struct RestoredState {
    pub attention_sequence: u64,
    pub panes: Vec<(AgentKind, AgentPaneKey, AgentState)>,
}

/// Read the saved state: this process's newest snapshot for `path`, else the file. A missing,
/// unreadable, or foreign-version file restores nothing: the next hook from each pane rebuilds
/// its state. Panes a previous service never placed in a Space are dropped; their private scope
/// died with it.
pub fn load(path: &Path) -> Option<RestoredState> {
    let in_process = lock(writers())
        .get(path)
        .and_then(|writer| lock(&writer.shared.slot).latest.clone());
    let (attention_sequence, panes) = match in_process {
        Some(snapshot) => (snapshot.attention_sequence, snapshot.panes.clone()),
        None => decode(&fs::read(path).ok()?)?,
    };
    let panes = panes
        .into_iter()
        .filter(|(key, _)| !key.scope.starts_with("service:"))
        .map(|(key, mut state)| {
            state.source = AgentSource::Restored;
            (state.provider, key, state)
        })
        .collect();
    Some(RestoredState {
        attention_sequence,
        panes,
    })
}

fn decode(bytes: &[u8]) -> Option<(u64, Vec<(AgentPaneKey, AgentState)>)> {
    let file: StateFile = serde_json::from_slice(bytes).ok()?;
    if file.version != VERSION {
        return None;
    }
    let panes = file
        .panes
        .into_iter()
        .filter_map(|record| {
            let status = AgentStatus::from_label(&record.status)?;
            let key = AgentPaneKey::new(record.scope, record.pane);
            let state = AgentState {
                provider: record.provider,
                attention: record.attention,
                attention_sequence: record.attention_sequence,
                acknowledged_sequence: record.acknowledged_sequence,
                source: AgentSource::Existing,
                status,
                session_id: record.session_id,
                session_file: record.session_file,
                session_name: record.session_name,
                thread_id: record.thread_id,
                turn_id: record.turn_id,
                last_event: record.last_event,
                last_message: record.last_message.map(Arc::from),
                turn_ended_at: record.turn_ended_at,
                error: record.error,
                cwd: record.cwd,
                launch: record.launch,
                server: record.server,
            };
            Some((key, state))
        })
        .collect();
    Some((file.attention_sequence, panes))
}

/// The saved form of a snapshot.
fn encode(snapshot: &Snapshot) -> Vec<u8> {
    let file = StateFile {
        version: VERSION,
        attention_sequence: snapshot.attention_sequence,
        panes: snapshot
            .panes
            .iter()
            .map(|(key, state)| PaneRecord {
                provider: state.provider,
                scope: key.scope.clone(),
                pane: key.pane.clone(),
                status: state.status.as_str(),
                attention: state.attention,
                attention_sequence: state.attention_sequence,
                acknowledged_sequence: state.acknowledged_sequence,
                session_id: state.session_id.clone(),
                session_file: state.session_file.clone(),
                session_name: state.session_name.clone(),
                thread_id: state.thread_id.clone(),
                turn_id: state.turn_id.clone(),
                last_event: state.last_event.clone(),
                last_message: state.last_message.as_deref().map(str::to_owned),
                turn_ended_at: state.turn_ended_at,
                error: state.error.clone(),
                cwd: state.cwd.clone(),
                launch: state.launch.clone(),
                server: state.server.clone(),
            })
            .collect(),
    };
    serde_json::to_vec(&file).unwrap_or_default()
}

/// The process's writer for one state file. Encoding and disk writes happen on its own thread,
/// coalescing bursts and retrying failures, so no caller ever waits on the disk.
#[derive(Clone)]
pub struct StateWriter {
    shared: Arc<Shared>,
}

struct Shared {
    path: PathBuf,
    slot: Mutex<Slot>,
    wake: Condvar,
}

#[derive(Default)]
struct Slot {
    /// The newest snapshot, kept after it is written so a replacing service restores it.
    latest: Option<Arc<Snapshot>>,
    /// Revision of `latest`; each submission counts one.
    submitted: u64,
    /// Newest revision a write was tried for, and newest on disk.
    attempted: u64,
    saved: u64,
    /// Skip the settle delay until the newest revision has been tried. A shutdown flush waits
    /// for the writer to clear it, so it sees a fresh attempt, not one made before it asked.
    flush: bool,
    error: Option<String>,
}

impl StateWriter {
    /// The process's writer for `path`, started on first use.
    pub fn for_path(path: &Path) -> Self {
        lock(writers())
            .entry(path.to_owned())
            .or_insert_with(|| {
                let shared = Arc::new(Shared {
                    path: path.to_owned(),
                    slot: Mutex::default(),
                    wake: Condvar::new(),
                });
                let worker = Arc::clone(&shared);
                thread::spawn(move || write_forever(&worker));
                Self { shared }
            })
            .clone()
    }

    /// Queue `snapshot` as the newest state, replacing anything not yet written.
    pub fn submit(&self, snapshot: Snapshot) {
        let mut slot = lock(&self.shared.slot);
        slot.latest = Some(Arc::new(snapshot));
        slot.submitted = slot.submitted.saturating_add(1);
        drop(slot);
        self.shared.wake.notify_all();
    }
}

/// Write every file's newest state now, waiting at most `limit` in total, for a clean shutdown.
///
/// # Errors
/// Names each state file whose newest state is not on disk, with the last write failure.
pub fn flush_all(limit: Duration) -> Result<(), String> {
    let deadline = Instant::now()
        .checked_add(limit)
        .unwrap_or_else(Instant::now);
    let pending = lock(writers()).values().cloned().collect::<Vec<_>>();
    let failures = pending
        .iter()
        .filter_map(|writer| flush_one(&writer.shared, deadline))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Ask `shared` to write its newest state at once and wait for the attempt until `deadline`.
/// Returns why the newest state is not on disk, if it is not.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the guard sets the flush request and must stay held into the condvar wait"
)]
fn flush_one(shared: &Shared, deadline: Instant) -> Option<String> {
    let mut slot = lock(&shared.slot);
    if slot.saved >= slot.submitted {
        return None;
    }
    slot.flush = true;
    shared.wake.notify_all();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let (slot, _) = shared
        .wake
        .wait_timeout_while(slot, remaining, |slot| slot.flush)
        .unwrap_or_else(PoisonError::into_inner);
    (slot.saved < slot.submitted).then(|| {
        slot.error
            .clone()
            .unwrap_or_else(|| format!("{}: not written before shutdown", shared.path.display()))
    })
}

fn write_forever(shared: &Shared) {
    let mut failures = 0_u32;
    loop {
        let (snapshot, revision) = next_snapshot(shared, retry_delay(failures));
        let written = write_private(&shared.path, &encode(&snapshot));
        let mut slot = lock(&shared.slot);
        slot.attempted = slot.attempted.max(revision);
        match written {
            Ok(()) => {
                slot.saved = slot.saved.max(revision);
                slot.error = None;
                failures = 0;
            }
            Err(error) => {
                // The snapshot stays newest until something replaces it; the next pass retries.
                slot.error = Some(format!("{}: {error}", shared.path.display()));
                failures = failures.saturating_add(1);
            }
        }
        if slot.attempted >= slot.submitted {
            slot.flush = false;
        }
        drop(slot);
        shared.wake.notify_all();
    }
}

/// The settle delay, doubled per consecutive failure up to [`RETRY_LIMIT`].
fn retry_delay(failures: u32) -> Duration {
    WRITE_DELAY
        .saturating_mul(1_u32 << failures.min(6))
        .min(RETRY_LIMIT)
}

/// Wait until a snapshot newer than disk exists, let a burst settle for `delay` unless a flush
/// is requested, then take the newest.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the guard must stay held across both condvar waits; it drops right after the take"
)]
fn next_snapshot(shared: &Shared, delay: Duration) -> (Arc<Snapshot>, u64) {
    loop {
        let unsaved = shared
            .wake
            .wait_while(lock(&shared.slot), |slot| {
                slot.saved >= slot.submitted || slot.latest.is_none()
            })
            .unwrap_or_else(PoisonError::into_inner);
        let (settled, _) = shared
            .wake
            .wait_timeout_while(unsaved, delay, |slot| !slot.flush)
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(snapshot) = settled.latest.clone() {
            return (snapshot, settled.submitted);
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Replacement keeps an existing file's mode, so tighten one that others can read first.
    #[cfg(unix)]
    if let Ok(metadata) = fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
    }
    bootty_write::WriteTarget::resolve(path)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?
        .lock()?
        .replace(bytes, bootty_write::NewFileMode::Private)
        .map(|_| ())
        .map_err(bootty_write::CommitError::into_io)
}
