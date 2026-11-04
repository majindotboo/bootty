use super::AppState;
use crate::recovery::{ArchiveListing, ArchiveStore, OutputArchive, fingerprint};
use anyhow::Context as _;
use bootty_mux::controller::SpaceId;
use bootty_mux::session_snapshot::SessionPaneCapture;
use bootty_mux::workspace::{PreparedSessionCheckpoint, SavedSessionCheckpoint};
use bootty_terminal::terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope};
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize)]
pub struct RecoveryOverview {
    pub id: String,
    pub title: String,
    pub host: String,
    pub saved_at_ms: u64,
    pub bytes: usize,
    pub omitted_lines: u64,
}
struct PendingArchive {
    archive: Option<OutputArchive>,
    capture: bootty_terminal::terminal_session::PendingWorkerResponse<
        Result<SessionPaneCapture, String>,
    >,
}
struct PendingSessionCheckpoint {
    prepared: Option<PreparedSessionCheckpoint>,
    panes: Vec<PendingArchive>,
}
enum CheckpointEvent {
    Saved(SavedSessionCheckpoint),
    Failed(anyhow::Error),
    ArchiveFailed(anyhow::Error),
    Finished,
}
#[derive(Clone)]
pub struct SessionCheckpointTicket {
    state: Arc<AtomicU8>,
    error: Arc<std::sync::Mutex<Option<String>>>,
}
impl SessionCheckpointTicket {
    pub fn accepted(&self) -> Option<bool> {
        match self.state.load(Ordering::Acquire) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }
    pub fn failure(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn record_failure(&self, error: impl std::fmt::Display) {
        self.error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| error.to_string());
    }
}
struct ArchiveCapture {
    scope: SpaceId,
    scope_s: String,
    session: String,
    identity: Option<String>,
    generation: u64,
    pane: String,
    title: String,
    host: String,
    fingerprint: String,
    backend: String,
}

pub(super) struct RecoveryState {
    store: Arc<ArchiveStore>,
    run: String,
    entries: Arc<Vec<OutputArchive>>,
    warnings: Vec<String>,
    loaded_revision: u64,
    listing: Option<(u64, mpsc::Receiver<anyhow::Result<ArchiveListing>>)>,
    saves: Vec<mpsc::Receiver<CheckpointEvent>>,
    checkpoint_failed: bool,
    checkpoint_finished: bool,
    checkpoint_ticket: Option<SessionCheckpointTicket>,
    restored_bindings: std::collections::HashSet<(SpaceId, u64)>,
    next_save: Instant,
}
impl RecoveryState {
    pub(super) fn new(
        window_key: &str,
        repaint: bootty_mux::RepaintHandle,
    ) -> anyhow::Result<Self> {
        let identity = bootty_config::ApplicationIdentity::current();
        #[cfg(not(windows))]
        let state = bootty_config::unix_daemon_state_path(
            identity,
            None,
            std::env::var_os("XDG_STATE_HOME")
                .as_deref()
                .map(std::path::Path::new),
            std::env::var_os("HOME")
                .as_deref()
                .map(std::path::Path::new),
        );
        #[cfg(windows)]
        let state = bootty_config::windows_daemon_state_path(
            identity,
            None,
            std::env::var_os("LOCALAPPDATA")
                .as_deref()
                .map(std::path::Path::new),
            std::env::var_os("APPDATA")
                .as_deref()
                .map(std::path::Path::new),
        );
        let root = state
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir)
            .join("recovery")
            .join(fingerprint(window_key.as_bytes()));
        let store = Arc::new(ArchiveStore::new(root));
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).context("create recovery run identity")?;
        let run = fingerprint(&random);
        let (sender, listing) = mpsc::channel();
        let loading = Arc::clone(&store);
        std::thread::spawn(move || {
            let _ = sender.send(loading.list());
            repaint();
        });
        let next_save = Instant::now()
            .checked_add(Duration::from_secs(30))
            .context("schedule recovery checkpoint")?;
        Ok(Self {
            store,
            run,
            entries: Arc::new(Vec::new()),
            warnings: Vec::new(),
            loaded_revision: 0,
            listing: Some((0, listing)),
            saves: Vec::new(),
            checkpoint_failed: false,
            checkpoint_finished: false,
            checkpoint_ticket: None,
            restored_bindings: std::collections::HashSet::new(),
            next_save,
        })
    }
}
impl AppState {
    pub fn recovery_overview(&self) -> Vec<RecoveryOverview> {
        self.recovery
            .entries
            .iter()
            .map(|a| RecoveryOverview {
                id: a.id.clone(),
                title: a.title.clone(),
                host: a.host.clone(),
                saved_at_ms: a.saved_at_ms,
                bytes: a.text.len(),
                omitted_lines: a.omitted_lines,
            })
            .collect()
    }
    pub fn recovery_archives(&self) -> Arc<Vec<OutputArchive>> {
        self.recovery.entries.clone()
    }
    pub fn recovery_archive(&self, id: &str) -> Option<OutputArchive> {
        self.recovery.entries.iter().find(|a| a.id == id).cloned()
    }
    pub fn recovery_warnings(&self) -> &[String] {
        &self.recovery.warnings
    }
    pub(super) fn poll_recovery(&mut self, now: Instant) {
        self.restore_selected_session(now);
        if let Some((revision, listing)) = &self.recovery.listing
            && let Ok(result) = listing.try_recv()
        {
            let revision = *revision;
            self.recovery.listing = None;
            match result {
                Ok(list) => {
                    self.recovery.entries = Arc::new(list.entries);
                    self.recovery.warnings = list.warnings;
                    // A save can finish after this list was read but before UI delivery.
                    // Keep its request revision so the newer store still triggers a refresh.
                    self.recovery.loaded_revision = revision;
                }
                Err(e) => self.record_error("Could not load saved sessions", &e),
            }
        }
        self.poll_session_checkpoints();
        let revision = self.recovery.store.revision.load(Ordering::Acquire);
        if self.recovery.listing.is_none() && revision != self.recovery.loaded_revision {
            self.recovery.loaded_revision = revision;
            let (tx, rx) = mpsc::channel();
            let store = Arc::clone(&self.recovery.store);
            let repaint = self.repaint.clone();
            std::thread::spawn(move || {
                let _ = tx.send(store.list());
                repaint();
            });
            self.recovery.listing = Some((revision, rx));
        }
        if now >= self.recovery.next_save
            && let Some(next_save) = now.checked_add(Duration::from_secs(30))
        {
            self.recovery.next_save = next_save;
            self.checkpoint_sessions(crate::clock::ClockSnapshot::now().epoch);
        }
    }

    fn restore_selected_session(&mut self, now: Instant) {
        let binding = &self.workspace.active.binding;
        let scope = binding.scope();
        let generation = binding.mux().binding_generation();
        if !binding.mux().has_session_snapshot() {
            return;
        }
        if self
            .recovery
            .restored_bindings
            .contains(&(scope, generation))
        {
            return;
        }
        // Automatic restore belongs to binding activation, not a later deliberate session close.
        self.recovery.restored_bindings.insert((scope, generation));
        let identities = binding
            .sessions()
            .sessions()
            .iter()
            .filter(|saved| {
                binding.session_attachment(&saved.identity).is_none()
                    && saved.terminal_snapshot.is_some()
            })
            .map(|saved| saved.identity.clone())
            .collect::<Vec<_>>();
        for identity in identities {
            let Some(invocation) = self.saved_session_invocation(
                scope,
                "session.reopen",
                vec![identity, "preserve".into()],
            ) else {
                continue;
            };
            let Some(deadline) = now.checked_add(Duration::from_secs(30)) else {
                return;
            };
            if self
                .app_command_sender(bootty_control::Caller::Internal)
                .submit(
                    invocation,
                    deadline,
                    bootty_control::CommandCancellation::new(),
                )
                .is_err()
            {
                self.recovery.restored_bindings.remove(&(scope, generation));
            }
        }
    }

    pub fn poll_session_checkpoints(&mut self) {
        let mut events = Vec::new();
        self.recovery.saves.retain(|receiver| {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(mpsc::TryRecvError::Empty) => return true,
                    Err(mpsc::TryRecvError::Disconnected) => return false,
                }
            }
        });
        for event in events {
            match event {
                CheckpointEvent::Saved(receipt) => {
                    if let Err(error) = self.workspace.publish_session_checkpoint(receipt) {
                        self.recovery.checkpoint_failed = true;
                        if let Some(ticket) = &self.recovery.checkpoint_ticket {
                            ticket.record_failure(&error);
                        }
                        self.record_error("Could not publish session checkpoint", &error);
                    }
                }
                CheckpointEvent::Failed(error) => {
                    self.recovery.checkpoint_failed = true;
                    if let Some(ticket) = &self.recovery.checkpoint_ticket {
                        ticket.record_failure(&error);
                    }
                    self.record_error("Could not save session checkpoint", &error);
                }
                CheckpointEvent::ArchiveFailed(error) => {
                    self.record_error("Could not save output archive", &error);
                }
                CheckpointEvent::Finished => self.recovery.checkpoint_finished = true,
            }
        }
        if self.recovery.saves.is_empty()
            && let Some(ticket) = self.recovery.checkpoint_ticket.take()
        {
            ticket.state.store(
                if self.recovery.checkpoint_failed || !self.recovery.checkpoint_finished {
                    2
                } else {
                    1
                },
                Ordering::Release,
            );
        }
    }

    fn checkpoint_specs(&self) -> Vec<ArchiveCapture> {
        let mut specs = Vec::new();
        for binding in self.workspace.all_bindings() {
            let scope = binding.scope();
            let scope_s = scope.persistence_value().to_string();
            let remote = binding.multiplexer().remote.as_ref();
            let host = remote.map_or_else(|| "Local".into(), bootty_mux::RemoteTarget::label);
            let fingerprint = remote
                .and_then(|r| serde_json::to_vec(r).ok())
                .map_or_else(|| "local".into(), |v| fingerprint(&v));
            let backend = "native".to_owned();
            for session in binding.member_sessions() {
                for window in &session.windows {
                    for anchor in std::iter::once(&window.anchor).chain(&window.panes) {
                        if let Some(pane) = &anchor.pane_id {
                            specs.push(ArchiveCapture {
                                scope,
                                scope_s: scope_s.clone(),
                                session: session.id.clone(),
                                identity: self.workspace.session_identity(scope, &session.id),
                                generation: binding.mux().binding_generation(),
                                pane: pane.clone(),
                                title: window.name.clone(),
                                host: host.clone(),
                                fingerprint: fingerprint.clone(),
                                backend: backend.clone(),
                            });
                        }
                    }
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        specs.retain(|spec| seen.insert((spec.scope, spec.pane.clone())));
        specs
    }

    /// Capture the current logical sessions without waiting for terminal workers or SQLite.
    /// The mux owner validates the exact attachment before committing each complete session.
    pub fn checkpoint_sessions(&mut self, captured_at: i64) {
        _ = self.checkpoint_sessions_matching(captured_at, None, Vec::new());
    }

    pub(crate) fn checkpoint_session(
        &mut self,
        scope: SpaceId,
        identity: &str,
    ) -> Option<SessionCheckpointTicket> {
        self.checkpoint_sessions_matching(
            crate::clock::ClockSnapshot::now().epoch,
            Some((scope, identity)),
            Vec::new(),
        )
    }

    fn checkpoint_sessions_matching(
        &mut self,
        captured_at: i64,
        only: Option<(SpaceId, &str)>,
        predecessors: Vec<mpsc::Receiver<CheckpointEvent>>,
    ) -> Option<SessionCheckpointTicket> {
        if !self.recovery.saves.is_empty() {
            return None;
        }
        let specs = self.checkpoint_specs();
        let mut sessions = std::collections::BTreeMap::new();
        let archives = self.config().session.output_archives;
        for spec in specs {
            if only.is_some_and(|(scope, identity)| {
                spec.scope != scope || spec.identity.as_deref() != Some(identity)
            }) {
                continue;
            }
            if spec.identity.is_none() && !archives {
                continue;
            }
            sessions
                .entry((
                    spec.scope.persistence_value(),
                    spec.identity.clone(),
                    spec.session.clone(),
                ))
                .or_insert_with(Vec::new)
                .push(spec);
        }
        // Backend-owned startup can outlive topology publication. Command completion waits
        // for its exact attached renderer before admitting the mandatory capture.
        if only.is_some()
            && sessions
                .values()
                .flatten()
                .any(|spec| self.checkpoint_capture_pending(spec))
        {
            return None;
        }
        let ticket = SessionCheckpointTicket {
            state: Arc::new(AtomicU8::new(0)),
            error: Arc::new(std::sync::Mutex::new(None)),
        };
        self.recovery.checkpoint_ticket = Some(ticket.clone());
        self.recovery.checkpoint_failed = false;
        self.recovery.checkpoint_finished = false;
        let mut pending = Vec::new();
        let mut archive_count = 0;
        let saved_at_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        for specs in sessions.into_values() {
            // Other windows can publish process-local topology before this owner has its
            // runtimes. Preserve the whole prior checkpoint until every pane is capturable.
            if specs
                .iter()
                .any(|spec| self.checkpoint_capture_pending(spec))
            {
                continue;
            }
            match self.prepare_checkpoint_capture(
                specs,
                captured_at,
                saved_at_ms,
                &mut archive_count,
            ) {
                Ok(capture) => pending.push(capture),
                Err(error) => {
                    self.recovery.checkpoint_failed = true;
                    ticket.record_failure(&error);
                    self.record_error("Could not save session checkpoints", &error);
                }
            }
        }
        if pending.is_empty() && predecessors.is_empty() {
            if only.is_some() {
                self.recovery.checkpoint_failed = true;
                if ticket.failure().is_none() {
                    ticket.record_failure(
                        "The exact saved session has no available terminal panes to checkpoint",
                    );
                }
            }
            ticket.state.store(
                if self.recovery.checkpoint_failed {
                    2
                } else {
                    1
                },
                Ordering::Release,
            );
            self.recovery.checkpoint_ticket = None;
            return Some(ticket);
        }
        let store = Arc::clone(&self.recovery.store);
        let (tx, rx) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            save_checkpoints(pending, predecessors, &store, &tx);
            let _ = tx.send(CheckpointEvent::Finished);
            drop(tx);
            repaint();
        });
        self.recovery.saves.push(rx);
        Some(ticket)
    }
    fn prepare_checkpoint_capture(
        &self,
        specs: Vec<ArchiveCapture>,
        captured_at: i64,
        saved_at_ms: u64,
        archive_count: &mut usize,
    ) -> anyhow::Result<PendingSessionCheckpoint> {
        let first = specs.first().context("Session checkpoint has no panes")?;
        // Freeze the exact owner and complete topology before any asynchronous history request.
        let prepared = first
            .identity
            .as_deref()
            .map(|identity| {
                self.workspace.prepare_session_checkpoint(
                    first.scope,
                    identity,
                    first.generation,
                    captured_at,
                )
            })
            .transpose()?;
        let mut panes = Vec::new();
        for spec in specs {
            let capture = self.capture_checkpoint_pane(&spec)?;
            let archive = if self.config().session.output_archives
                && *archive_count < crate::recovery::MAX_ARCHIVES
            {
                *archive_count = archive_count.saturating_add(1);
                Some(self.checkpoint_archive(spec, saved_at_ms))
            } else {
                None
            };
            panes.push(PendingArchive { archive, capture });
        }
        // Every capture must queue successfully before this complete session can replace its prior state.
        Ok(PendingSessionCheckpoint { prepared, panes })
    }

    fn checkpoint_capture_pending(&self, spec: &ArchiveCapture) -> bool {
        self.workspace
            .binding(spec.scope)
            .is_some_and(|binding| !binding.mux().has_session_snapshot())
    }

    fn capture_checkpoint_pane(
        &self,
        spec: &ArchiveCapture,
    ) -> anyhow::Result<
        bootty_terminal::terminal_session::PendingWorkerResponse<
            Result<SessionPaneCapture, String>,
        >,
    > {
        let options = CaptureOptions {
            scope: CaptureScope::History,
            format: CaptureFormat::Ansi,
            max_lines: 10_000,
            max_bytes: crate::recovery::MAX_TEXT,
            ..Default::default()
        };
        self.workspace
            .capture_session_pane(spec.scope, &spec.pane, options)
    }

    fn checkpoint_archive(&self, spec: ArchiveCapture, saved_at_ms: u64) -> OutputArchive {
        let id =
            fingerprint(format!("{}:{}:{}", self.recovery.run, spec.scope_s, spec.pane).as_bytes());
        OutputArchive {
            id,
            run: self.recovery.run.clone(),
            scope: spec.scope_s,
            session: spec.session,
            pane: spec.pane,
            title: spec.title,
            host: spec.host,
            host_fingerprint: spec.fingerprint,
            backend: spec.backend,
            saved_at_ms,
            cols: 0,
            rows: 0,
            omitted_lines: 0,
            text: String::new(),
        }
    }
    /// Whether a requested checkpoint still has capture or persistence work outstanding.
    pub const fn session_checkpoint_pending(&self) -> bool {
        !self.recovery.saves.is_empty()
    }
    pub(crate) fn take_session_checkpoint_flush(
        &mut self,
        capture: bool,
    ) -> impl FnOnce() + Send + 'static {
        self.poll_session_checkpoints();
        if capture {
            let predecessors = std::mem::take(&mut self.recovery.saves);
            _ = self.checkpoint_sessions_matching(
                crate::clock::ClockSnapshot::now().epoch,
                None,
                predecessors,
            );
        }
        let saves = std::mem::take(&mut self.recovery.saves);
        move || {
            let deadline = Instant::now()
                .checked_add(gpui_kit::SHUTDOWN_TIMEOUT.saturating_sub(Duration::from_millis(50)));
            for save in saves {
                loop {
                    let remaining = deadline.map_or(Duration::ZERO, |deadline| {
                        deadline.saturating_duration_since(Instant::now())
                    });
                    match save.recv_timeout(remaining) {
                        Ok(CheckpointEvent::Saved(_) | CheckpointEvent::Finished) => {}
                        Ok(
                            CheckpointEvent::Failed(error) | CheckpointEvent::ArchiveFailed(error),
                        ) => {
                            eprintln!("Session checkpoint was not saved before quitting: {error}");
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            eprintln!("Session checkpoint did not finish before quitting");
                            return;
                        }
                    }
                }
            }
        }
    }
    pub(crate) fn recovery_store(&self) -> Arc<ArchiveStore> {
        Arc::clone(&self.recovery.store)
    }
}

impl PendingSessionCheckpoint {
    fn save(
        mut self,
        receipts: &[SavedSessionCheckpoint],
    ) -> anyhow::Result<(Option<SavedSessionCheckpoint>, Vec<OutputArchive>)> {
        if let Some(prepared) = &mut self.prepared {
            for receipt in receipts {
                // Other logical sessions cannot change this checkpoint's expected owner.
                _ = prepared.follow_checkpoint(receipt);
            }
        }
        let mut captures = Vec::new();
        let mut archives = Vec::new();
        for mut item in self.panes {
            let mut capture = item
                .capture
                .receive_for("checkpoint terminal", Duration::from_secs(5))?
                .map_err(anyhow::Error::msg)?;
            capture.text = bootty_terminal::terminal_history::sanitize_history(&capture.text)?;
            if let Some(archive) = &mut item.archive {
                archive.cols = capture.cols;
                archive.rows = capture.rows;
                archive.omitted_lines = capture.omitted_lines;
                archive.text =
                    bootty_terminal::terminal_history::history_plain_text(&capture.text)?;
            }
            captures.push(capture);
            archives.extend(item.archive);
        }
        let saved = self
            .prepared
            .map(|prepared| prepared.save(captures))
            .transpose()?;
        Ok((saved, archives))
    }
}

fn save_checkpoints(
    pending: Vec<PendingSessionCheckpoint>,
    predecessors: Vec<mpsc::Receiver<CheckpointEvent>>,
    store: &ArchiveStore,
    sender: &mpsc::Sender<CheckpointEvent>,
) {
    let mut receipts = Vec::new();
    // Final quit captures follow the immutable predecessor receipts before committing their CAS.
    for predecessor in predecessors {
        while let Ok(event) = predecessor.recv() {
            match event {
                CheckpointEvent::Saved(receipt) => receipts.push(receipt),
                CheckpointEvent::Failed(error) => {
                    if pending.is_empty() {
                        let _ = sender.send(CheckpointEvent::Failed(error));
                    } else {
                        eprintln!("Prior session checkpoint failed before final capture: {error}");
                    }
                }
                CheckpointEvent::ArchiveFailed(error) => {
                    let _ = sender.send(CheckpointEvent::ArchiveFailed(error));
                }
                CheckpointEvent::Finished => {}
            }
        }
    }
    for session in pending {
        match session.save(&receipts) {
            Ok((saved, archives)) => {
                // Publish each committed receipt even if a later session or archive fails.
                if let Some(saved) = saved {
                    let _ = sender.send(CheckpointEvent::Saved(saved));
                }
                for archive in archives {
                    if let Err(error) = store.save(&archive) {
                        let _ = sender.send(CheckpointEvent::ArchiveFailed(error));
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(CheckpointEvent::Failed(error));
            }
        }
    }
}
