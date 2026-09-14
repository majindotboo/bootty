use super::AppState;
use crate::recovery::{ArchiveListing, ArchiveStore, ArchivedAgent, OutputArchive, fingerprint};
use anyhow::Context as _;
use bootty_agents::AgentKind;
use bootty_mux::controller::SpaceId;
use bootty_terminal::terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope};
use serde::Serialize;
use std::{
    sync::{Arc, atomic::Ordering, mpsc},
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
    pub resumable: bool,
}
struct PendingArchive {
    archive: OutputArchive,
    capture: bootty_terminal::terminal_session::PendingWorkerResponse<
        Result<bootty_terminal::terminal_capture::TerminalCapture, String>,
    >,
}
struct ArchiveCapture {
    scope: SpaceId,
    scope_s: String,
    session: String,
    pane: String,
    title: String,
    host: String,
    fingerprint: String,
    backend: String,
    agent: Option<ArchivedAgent>,
}

pub(super) struct RecoveryState {
    store: Arc<ArchiveStore>,
    run: String,
    entries: Arc<Vec<OutputArchive>>,
    warnings: Vec<String>,
    loaded_revision: u64,
    listing: Option<(u64, mpsc::Receiver<anyhow::Result<ArchiveListing>>)>,
    saves: Vec<mpsc::Receiver<anyhow::Result<()>>>,
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
                resumable: a.agent.is_some(),
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
                Err(e) => self.record_error(e),
            }
        }
        let mut errors = Vec::new();
        self.recovery.saves.retain(|rx| match rx.try_recv() {
            Ok(Err(e)) => {
                errors.push(e);
                false
            }
            Ok(Ok(())) | Err(mpsc::TryRecvError::Disconnected) => false,
            Err(mpsc::TryRecvError::Empty) => true,
        });
        for e in errors {
            self.record_error(e);
        }
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
        if self.config().session.output_archives
            && now >= self.recovery.next_save
            && let Some(next_save) = now.checked_add(Duration::from_secs(30))
        {
            self.recovery.next_save = next_save;
            self.checkpoint_terminals();
        }
    }
    fn archived_agent(&self, scope: &str, pane: &str) -> Option<ArchivedAgent> {
        let service = self.agent_service()?;
        for provider in AgentKind::ALL {
            let state = service.snapshot_scoped(provider, Some(scope), Some(pane));
            let Some(launch) = state.launch.map(|launch| launch.retained(provider)) else {
                continue;
            };
            let session = match provider {
                AgentKind::Pi => state.session_file.or(state.session_id),
                AgentKind::Codex => state.thread_id,
                AgentKind::Claude => state.session_id,
            };
            let Some(session) = session else {
                continue;
            };
            if launch.ephemeral || launch.session_arguments(provider, &session, false).is_err() {
                continue;
            }
            return Some(ArchivedAgent {
                provider,
                session,
                launch,
            });
        }
        None
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
            let backend = format!("{:?}", binding.multiplexer().backend).to_lowercase();
            for session in binding.mux().all_sessions() {
                for window in &session.windows {
                    for anchor in std::iter::once(&window.anchor).chain(&window.panes) {
                        if let Some(pane) = &anchor.pane_id {
                            specs.push(ArchiveCapture {
                                scope,
                                scope_s: scope_s.clone(),
                                session: session.id.clone(),
                                pane: pane.clone(),
                                title: window.name.clone(),
                                host: host.clone(),
                                fingerprint: fingerprint.clone(),
                                backend: backend.clone(),
                                agent: self.archived_agent(&scope_s, pane),
                            });
                        }
                    }
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        specs.retain(|spec| seen.insert((spec.scope, spec.pane.clone())));
        specs.truncate(crate::recovery::MAX_ARCHIVES);
        specs
    }

    fn checkpoint_terminals(&mut self) {
        if !self.recovery.saves.is_empty() {
            return;
        }
        let specs = self.checkpoint_specs();
        let mut pending = Vec::new();
        let saved_at_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        for spec in specs {
            let Some(binding) = self.workspace.binding_mut(spec.scope) else {
                continue;
            };
            let Some(runtime) = binding.terminal_mut().focused_terminal_runtime(&spec.pane) else {
                continue;
            };
            let Ok(capture) = runtime.capture(CaptureOptions {
                scope: CaptureScope::History,
                format: CaptureFormat::Plain,
                max_lines: 10_000,
                max_bytes: crate::recovery::MAX_TEXT,
                ..Default::default()
            }) else {
                continue;
            };
            let id = fingerprint(
                format!("{}:{}:{}", self.recovery.run, spec.scope_s, spec.pane).as_bytes(),
            );
            pending.push(PendingArchive {
                archive: OutputArchive {
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
                    agent: spec.agent,
                },
                capture,
            });
        }
        if pending.is_empty() {
            return;
        }
        let store = Arc::clone(&self.recovery.store);
        let (tx, rx) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                for mut item in pending {
                    let capture = item
                        .capture
                        .receive("checkpoint terminal")?
                        .map_err(anyhow::Error::msg)?;
                    item.archive.cols = capture.cols;
                    item.archive.rows = capture.rows;
                    item.archive.omitted_lines = capture.omitted_lines;
                    item.archive.text = capture.text;
                    store.save(&item.archive)?;
                }
                Ok(())
            })();
            let _ = tx.send(result);
            repaint();
        });
        self.recovery.saves.push(rx);
    }
    pub(crate) fn recovery_store(&self) -> Arc<ArchiveStore> {
        Arc::clone(&self.recovery.store)
    }
}
