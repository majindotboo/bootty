use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use bootty_config::config::MultiplexerConfig;

use crate::{
    RepaintHandle,
    command::MuxCommand,
    config::{MuxBackendKind, build_backend, selected_backend},
    snapshot::{MuxSession, MuxSnapshot, selection_after_refresh},
};

pub const MUX_SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMuxSessionRequest {
    pub session_id: String,
    pub cwd: String,
}

type SessionRefreshSnapshot = std::result::Result<(MuxBackendKind, MuxSnapshot), String>;
type SessionRefreshResult = (u64, SessionRefreshSnapshot);

struct SessionRefreshRequest {
    generation: u64,
    config: MultiplexerConfig,
}

type MuxCommandResult = std::result::Result<Option<String>, String>;

struct MuxCommandJob {
    config: MultiplexerConfig,
    command: MuxCommand,
    selected_session: Option<String>,
}

fn selected_window_after_refresh(
    selected_session: Option<&str>,
    current: Option<String>,
    previous_active: Option<&str>,
    snapshot: &MuxSnapshot,
) -> Option<String> {
    let selected_session = selected_session?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == selected_session || session.name == selected_session)?;
    let active = session.active_window_id.as_deref();
    // Follow an external switch: when tmux's active window moved since the last
    // snapshot, the highlight tracks it (e.g. windows changed from inside tmux).
    // Otherwise keep the current selection, stable across refreshes and during an
    // optimistic local switch that the snapshot hasn't caught up to yet.
    if active.is_some() && active != previous_active {
        return active.map(str::to_owned);
    }
    current
        .filter(|window_id| session.windows.iter().any(|window| &window.id == window_id))
        .or_else(|| session.active_window_id.clone())
}

fn active_window_id_of(sessions: &[MuxSession], selected_session: Option<&str>) -> Option<String> {
    let selected_session = selected_session?;
    sessions
        .iter()
        .find(|session| session.id == selected_session || session.name == selected_session)
        .and_then(|session| session.active_window_id.clone())
}

fn optimistic_window_after_command(
    sessions: &[MuxSession],
    selected_window: Option<&str>,
    command: &MuxCommand,
) -> Option<String> {
    let (session_id, step) = match command {
        MuxCommand::ActivateNextWindow { session_id } => (session_id.as_str(), 1_i32),
        MuxCommand::ActivatePreviousWindow { session_id } => (session_id.as_str(), -1_i32),
        MuxCommand::ActivateWindowIndex { session_id, index } => {
            let session = sessions
                .iter()
                .find(|session| session.id == *session_id || session.name == *session_id)?;
            return session
                .windows
                .iter()
                .find(|window| window.index == *index)
                .map(|window| window.id.clone());
        }
        _ => return None,
    };
    let session = sessions
        .iter()
        .find(|session| session.id == session_id || session.name == session_id)?;
    if session.windows.is_empty() {
        return None;
    }
    let current_id = selected_window.or(session.active_window_id.as_deref());
    let current = current_id
        .and_then(|id| session.windows.iter().position(|window| window.id == id))
        .unwrap_or(0);
    let next = (current as i32 + step).rem_euclid(session.windows.len() as i32) as usize;
    Some(session.windows[next].id.clone())
}
fn command_session_id(command: &MuxCommand) -> &str {
    match command {
        MuxCommand::ActivateWindow { session_id, .. }
        | MuxCommand::NewWindow { session_id, .. }
        | MuxCommand::RenameWindow { session_id, .. }
        | MuxCommand::ActivateNextWindow { session_id }
        | MuxCommand::ActivatePreviousWindow { session_id }
        | MuxCommand::ActivateLastWindow { session_id }
        | MuxCommand::ActivateWindowIndex { session_id, .. }
        | MuxCommand::MoveWindow { session_id, .. }
        | MuxCommand::SplitPane { session_id, .. }
        | MuxCommand::SelectPane { session_id, .. }
        | MuxCommand::SelectNextPane { session_id }
        | MuxCommand::SelectPreviousPane { session_id }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id }
        | MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id } => session_id,
    }
}

fn stable_session_order(
    previous: &[MuxSession],
    mut refreshed: Vec<MuxSession>,
) -> Vec<MuxSession> {
    let mut ordered = Vec::with_capacity(refreshed.len());
    for old in previous {
        if let Some(index) = refreshed
            .iter()
            .position(|session| session.id == old.id || session.name == old.name)
        {
            ordered.push(refreshed.remove(index));
        }
    }
    ordered.extend(refreshed);
    ordered
}

fn order_sessions_by_names(sessions: &[MuxSession], ordered_names: &[String]) -> Vec<MuxSession> {
    let mut remaining = sessions.to_vec();
    let mut ordered = Vec::with_capacity(remaining.len());
    for name in ordered_names {
        if let Some(index) = remaining.iter().position(|session| &session.name == name) {
            ordered.push(remaining.remove(index));
        }
    }
    ordered.extend(remaining);
    ordered
}

#[derive(Default)]
pub struct MuxController {
    sessions: Vec<MuxSession>,
    selected_session: Option<String>,
    previous_selected_session: Option<String>,
    selected_window: Option<String>,
    /// The selected session's active window id from the previous snapshot, used to
    /// detect window switches made outside bootty so the highlight follows them.
    last_active_window: Option<String>,
    current_backend: Option<MuxBackendKind>,
    last_session_refresh: Option<Instant>,
    session_refresh_generation: u64,
    session_refresh_tx: Option<mpsc::Sender<SessionRefreshRequest>>,
    session_refresh_rx: Option<mpsc::Receiver<SessionRefreshResult>>,
    session_refresh_pending: bool,
    mux_command_tx: Option<mpsc::Sender<MuxCommandJob>>,
    mux_command_rx: Option<mpsc::Receiver<MuxCommandResult>>,
}

impl MuxController {
    pub fn new() -> Self {
        Self {
            last_session_refresh: Some(Instant::now() - Duration::from_secs(2)),
            ..Default::default()
        }
    }

    pub fn sessions(&self) -> &[MuxSession] {
        &self.sessions
    }

    pub fn selected_session(&self) -> Option<&str> {
        self.selected_session.as_deref()
    }

    pub fn previous_selected_session(&self) -> Option<&str> {
        let selected = self.previous_selected_session.as_deref()?;
        self.sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.id.as_str())
    }

    pub fn selected_session_anchor(&self) -> Option<&crate::snapshot::MuxPaneAnchor> {
        let selected = self.selected_session.as_deref()?;
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        if let Some(selected_window) = self.selected_window.as_deref()
            && let Some(window) = session
                .windows
                .iter()
                .find(|window| window.id == selected_window)
        {
            return Some(&window.anchor);
        }
        Some(&session.anchor)
    }

    pub fn selected_session_windows(&self) -> &[crate::snapshot::MuxWindow] {
        let Some(selected) = self.selected_session.as_deref() else {
            return &[];
        };
        self.sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.windows.as_slice())
            .unwrap_or_default()
    }

    /// Panes of the selected window (the active window of the selected session unless a specific
    /// window is selected). Native renders these as a split layout; other backends report a single
    /// attach anchor.
    pub fn selected_window_panes(&self) -> &[crate::snapshot::MuxPaneAnchor] {
        let Some(selected) = self.selected_session.as_deref() else {
            return &[];
        };
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
        else {
            return &[];
        };
        let window_id = self
            .selected_window
            .as_deref()
            .or(session.active_window_id.as_deref());
        window_id
            .and_then(|id| session.windows.iter().find(|window| window.id == id))
            .or_else(|| session.windows.first())
            .map(|window| window.panes.as_slice())
            .unwrap_or_default()
    }

    pub fn selected_window_layout(&self) -> Option<&crate::snapshot::MuxPaneLayout> {
        let selected = self.selected_session.as_deref()?;
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        let window_id = self
            .selected_window
            .as_deref()
            .or(session.active_window_id.as_deref());
        window_id
            .and_then(|id| session.windows.iter().find(|window| window.id == id))
            .or_else(|| session.windows.first())
            .and_then(|window| window.layout.as_ref())
    }

    pub fn apply_session_order(&mut self, ordered_names: &[String]) {
        self.sessions = order_sessions_by_names(&self.sessions, ordered_names);
    }

    pub fn selected_window(&self) -> Option<&str> {
        self.selected_window.as_deref()
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) -> Option<String> {
        while let Some((generation, result)) = self.poll_session_refresh() {
            if generation != self.session_refresh_generation {
                continue;
            }
            match result {
                Ok((backend, snapshot)) => self.apply_refreshed_snapshot(backend, snapshot),
                Err(error) => return Some(error),
            }
        }

        if self
            .last_session_refresh
            .is_some_and(|last| last.elapsed() < MUX_SESSION_REFRESH_INTERVAL)
        {
            return None;
        }

        if selected_backend(config) == MuxBackendKind::Native {
            return self.refresh_native_sessions(config);
        }

        if self.session_refresh_pending {
            return None;
        }

        self.ensure_session_refresh_worker(repaint);
        let Some(tx) = &self.session_refresh_tx else {
            return Some("mux session refresh worker did not start".to_owned());
        };
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        let request = SessionRefreshRequest {
            generation: self.session_refresh_generation,
            config: config.clone(),
        };
        match tx.send(request) {
            Ok(()) => {
                self.last_session_refresh = Some(Instant::now());
                self.session_refresh_pending = true;
                None
            }
            Err(_) => {
                self.session_refresh_tx = None;
                self.session_refresh_rx = None;
                self.session_refresh_pending = false;
                Some("mux session refresh worker stopped".to_owned())
            }
        }
    }

    fn refresh_native_sessions(&mut self, config: &MultiplexerConfig) -> Option<String> {
        match build_backend(config).snapshot() {
            Ok(snapshot) => {
                self.apply_refreshed_snapshot(MuxBackendKind::Native, snapshot);
                self.last_session_refresh = Some(Instant::now());
                None
            }
            Err(error) => Some(error.to_string()),
        }
    }

    pub fn poll_command(&mut self) -> Option<Result<(), String>> {
        let mut completed = false;
        let mut first_error = None;
        loop {
            let result = match self.mux_command_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(result)) => result,
                Some(Err(mpsc::TryRecvError::Empty)) => break,
                None => return None,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.mux_command_tx = None;
                    self.mux_command_rx = None;
                    return Some(Err("mux command worker stopped".to_owned()));
                }
            };
            completed = true;
            match result {
                Ok(selected_session) => {
                    if let Some(session) = selected_session {
                        self.activate_session(&session);
                    }
                    self.last_session_refresh = Some(Instant::now() - MUX_SESSION_REFRESH_INTERVAL);
                    self.session_refresh_generation =
                        self.session_refresh_generation.wrapping_add(1);
                    self.session_refresh_pending = false;
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        completed.then(|| first_error.map_or(Ok(()), Err))
    }

    fn set_selected_session(&mut self, session_id: Option<String>) {
        if self.selected_session == session_id {
            return;
        }
        if let Some(current) = self.selected_session.take() {
            self.previous_selected_session = Some(current);
        }
        self.selected_session = session_id;
    }

    pub fn activate_session(&mut self, session_id: &str) {
        self.set_selected_session(Some(session_id.to_owned()));
        self.selected_window = None;
    }

    pub fn activate_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        self.set_selected_session(Some(session_id.to_owned()));
        self.selected_window = Some(window_id.to_owned());
        let command = MuxCommand::ActivateWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
        };
        if self
            .execute_native_command(
                config,
                command.clone(),
                Some(session_id.to_owned()),
                Some(window_id.to_owned()),
            )
            .is_ok()
        {
            repaint();
            return;
        }
        self.enqueue_command(repaint, config, command, Some(session_id.to_owned()));
    }
    pub fn rename_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        name: String,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        let command = MuxCommand::RenameWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
            name,
        };
        if self
            .execute_native_command(
                config,
                command.clone(),
                Some(session_id.to_owned()),
                Some(window_id.to_owned()),
            )
            .is_ok()
        {
            repaint();
            return;
        }
        self.enqueue_command(repaint, config, command, Some(session_id.to_owned()));
    }

    pub fn create_project_session(
        &mut self,
        request: NewMuxSessionRequest,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
    ) {
        let command = MuxCommand::CreateProjectSession {
            session_id: request.session_id.clone(),
            cwd: request.cwd,
        };
        if self
            .execute_native_command(
                config,
                command.clone(),
                Some(request.session_id.clone()),
                None,
            )
            .is_ok()
        {
            repaint();
            return;
        }
        self.activate_session(&request.session_id);
        self.enqueue_command(repaint, config, command, Some(request.session_id));
    }

    fn poll_session_refresh(&mut self) -> Option<SessionRefreshResult> {
        let result = match self.session_refresh_rx.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some((
                self.session_refresh_generation,
                Err("mux session refresh worker stopped".to_owned()),
            )),
        };
        if matches!(result, Some((generation, _)) if generation == self.session_refresh_generation)
        {
            self.session_refresh_pending = false;
        }
        result
    }

    fn ensure_session_refresh_worker(&mut self, repaint: &RepaintHandle) {
        if self.session_refresh_tx.is_some() && self.session_refresh_rx.is_some() {
            return;
        }

        let (request_tx, request_rx) = mpsc::channel::<SessionRefreshRequest>();
        let (result_tx, result_rx) = mpsc::channel::<SessionRefreshResult>();
        let repaint = repaint.clone();
        thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let backend_kind = selected_backend(&request.config);
                let result = build_backend(&request.config)
                    .snapshot()
                    .map(|snapshot| (backend_kind, snapshot))
                    .map_err(|error| error.to_string());
                if result_tx.send((request.generation, result)).is_err() {
                    break;
                }
                repaint();
            }
        });
        self.session_refresh_tx = Some(request_tx);
        self.session_refresh_rx = Some(result_rx);
    }

    pub fn execute_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
        command: MuxCommand,
    ) {
        let selected_session = Some(command_session_id(&command).to_owned());
        if self
            .execute_native_command(config, command.clone(), selected_session.clone(), None)
            .is_ok()
        {
            repaint();
            return;
        }
        self.apply_optimistic_command_selection(&command);
        self.enqueue_command(repaint, config, command, selected_session);
    }

    fn execute_native_command(
        &mut self,
        config: &MultiplexerConfig,
        command: MuxCommand,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> Result<(), String> {
        let backend_kind = selected_backend(config);
        if backend_kind != MuxBackendKind::Native {
            return Err("not synchronous-native".to_owned());
        }
        let mut backend = build_backend(config);
        backend
            .execute(command)
            .and_then(|()| backend.snapshot())
            .map(|snapshot| {
                self.apply_snapshot(backend_kind, snapshot, preferred_session, preferred_window);
                self.last_session_refresh = Some(Instant::now() - MUX_SESSION_REFRESH_INTERVAL);
            })
            .map_err(|error| error.to_string())
    }

    fn apply_refreshed_snapshot(&mut self, backend: MuxBackendKind, snapshot: MuxSnapshot) {
        let same_backend = self.current_backend == Some(backend);
        let current_session = same_backend.then(|| self.selected_session.take()).flatten();
        let current_window = same_backend.then(|| self.selected_window.take()).flatten();
        self.apply_snapshot(backend, snapshot, current_session, current_window);
    }

    fn apply_snapshot(
        &mut self,
        backend: MuxBackendKind,
        mut snapshot: MuxSnapshot,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) {
        let same_backend = self.current_backend == Some(backend);
        if same_backend {
            snapshot.sessions = stable_session_order(&self.sessions, snapshot.sessions);
        }
        self.set_selected_session(selection_after_refresh(preferred_session, &snapshot));
        self.selected_window = selected_window_after_refresh(
            self.selected_session.as_deref(),
            preferred_window,
            self.last_active_window.as_deref(),
            &snapshot,
        );
        self.current_backend = Some(backend);
        self.sessions = snapshot.sessions;
        self.last_active_window =
            active_window_id_of(&self.sessions, self.selected_session.as_deref());
    }

    fn apply_optimistic_command_selection(&mut self, command: &MuxCommand) {
        let session_id = command_session_id(command).to_owned();
        if let Some(window_id) = optimistic_window_after_command(
            &self.sessions,
            self.selected_window.as_deref(),
            command,
        ) {
            self.set_selected_session(Some(session_id));
            self.selected_window = Some(window_id);
        }
    }

    fn ensure_command_worker(&mut self, repaint: &RepaintHandle) {
        if self.mux_command_tx.is_some() && self.mux_command_rx.is_some() {
            return;
        }

        let (request_tx, request_rx) = mpsc::channel::<MuxCommandJob>();
        let (result_tx, result_rx) = mpsc::channel::<MuxCommandResult>();
        let repaint = repaint.clone();
        thread::spawn(move || {
            while let Ok(job) = request_rx.recv() {
                let mut backend = build_backend(&job.config);
                let result = backend
                    .execute(job.command)
                    .map(|()| job.selected_session)
                    .map_err(|error| error.to_string());
                if result_tx.send(result).is_err() {
                    break;
                }
                repaint();
            }
        });
        self.mux_command_tx = Some(request_tx);
        self.mux_command_rx = Some(result_rx);
    }

    fn enqueue_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MultiplexerConfig,
        command: MuxCommand,
        selected_session: Option<String>,
    ) {
        self.ensure_command_worker(repaint);
        let job = MuxCommandJob {
            config: config.clone(),
            command,
            selected_session,
        };
        let Some(tx) = &self.mux_command_tx else {
            return;
        };
        if let Err(error) = tx.send(job) {
            self.mux_command_tx = None;
            self.mux_command_rx = None;
            self.ensure_command_worker(repaint);
            if let Some(tx) = &self.mux_command_tx {
                let _ = tx.send(error.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{MuxPaneAnchor, MuxWindow};

    #[test]
    fn selected_session_anchor_resolves_by_backend_id_or_session_name() {
        let anchor = MuxPaneAnchor {
            session_id: "$7".to_owned(),
            pane_id: Some("%9".to_owned()),
            cwd: None,
            process: None,
        };
        let mut controller = MuxController {
            sessions: vec![MuxSession {
                id: "$7".to_owned(),
                name: "piu".to_owned(),
                active: false,
                anchor: anchor.clone(),
                active_window_id: Some("@2".to_owned()),
                windows: vec![MuxWindow {
                    id: "@2".to_owned(),
                    index: 1,
                    name: "editor".to_owned(),
                    active: true,
                    anchor: MuxPaneAnchor {
                        session_id: "$7".to_owned(),
                        pane_id: Some("%11".to_owned()),
                        cwd: None,
                        process: Some("nvim".to_owned()),
                    },
                    panes: Vec::new(),
                    layout: None,
                }],
            }],
            selected_session: Some("piu".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            controller
                .selected_session_anchor()
                .map(|anchor| anchor.session_id.as_str()),
            Some("$7")
        );

        controller.selected_session = Some("$7".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%9")
        );

        controller.selected_window = Some("@2".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%11")
        );
    }

    #[test]
    fn stable_session_order_preserves_existing_order_and_appends_new_sessions() {
        let previous = vec![
            session("$2", "work"),
            session("$1", "main"),
            session("$4", "old"),
        ];
        let refreshed = vec![
            session("$1", "main"),
            session("$3", "new"),
            session("$2", "work"),
        ];

        let ordered = stable_session_order(&previous, refreshed);

        assert_eq!(
            ordered
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["$2", "$1", "$3"]
        );
    }

    #[test]
    fn apply_session_order_reorders_sessions_by_name_and_appends_new_sessions() {
        let mut controller = MuxController {
            sessions: vec![
                session("$1", "main"),
                session("$2", "work"),
                session("$3", "new"),
            ],
            ..Default::default()
        };

        controller.apply_session_order(&["work".to_owned(), "main".to_owned()]);

        assert_eq!(
            controller
                .sessions()
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec!["work", "main", "new"]
        );
    }

    #[test]
    fn activate_session_tracks_previous_bootty_selection() {
        let mut controller = MuxController {
            sessions: vec![session("$1", "main"), session("$2", "work")],
            ..Default::default()
        };

        controller.activate_session("$1");
        controller.activate_session("$2");
        assert_eq!(controller.selected_session(), Some("$2"));
        assert_eq!(controller.previous_selected_session(), Some("$1"));

        controller.activate_session("$1");
        assert_eq!(controller.selected_session(), Some("$1"));
        assert_eq!(controller.previous_selected_session(), Some("$2"));
    }

    #[test]
    fn optimistic_tab_commands_select_known_external_windows() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 1), window("@2", 2), window("@3", 3)];
        work.active_window_id = Some("@1".to_owned());
        let mut controller = MuxController {
            sessions: vec![work],
            selected_session: Some("$1".to_owned()),
            selected_window: Some("@2".to_owned()),
            ..Default::default()
        };

        controller.apply_optimistic_command_selection(&MuxCommand::ActivateNextWindow {
            session_id: "$1".to_owned(),
        });
        assert_eq!(controller.selected_window(), Some("@3"));

        controller.apply_optimistic_command_selection(&MuxCommand::ActivateNextWindow {
            session_id: "$1".to_owned(),
        });
        assert_eq!(controller.selected_window(), Some("@1"));

        controller.apply_optimistic_command_selection(&MuxCommand::ActivatePreviousWindow {
            session_id: "$1".to_owned(),
        });
        assert_eq!(controller.selected_window(), Some("@3"));

        controller.apply_optimistic_command_selection(&MuxCommand::ActivateWindowIndex {
            session_id: "$1".to_owned(),
            index: 2,
        });
        assert_eq!(controller.selected_window(), Some("@2"));
    }

    #[test]
    fn native_refresh_keeps_empty_startup_snapshot_without_worker() {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let config = MultiplexerConfig {
            backend: bootty_config::config::MultiplexerBackendConfig::Native,
            ..Default::default()
        };
        let mut controller = MuxController::new();

        let error = controller.refresh_sessions(&repaint, &config);

        assert_eq!(error, None);
        assert_eq!(controller.current_backend, Some(MuxBackendKind::Native));
        assert!(controller.sessions.is_empty());
        assert!(controller.session_refresh_tx.is_none());
        assert!(controller.session_refresh_rx.is_none());
        assert!(!controller.session_refresh_pending);
    }

    fn session(id: &str, name: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: None,
                cwd: None,
                process: None,
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    fn window(id: &str, index: u32) -> MuxWindow {
        MuxWindow {
            id: id.to_owned(),
            index,
            name: format!("w{index}"),
            active: false,
            anchor: MuxPaneAnchor::default(),
            panes: Vec::new(),
            layout: None,
        }
    }

    #[test]
    fn selected_window_follows_external_switch_but_keeps_local_selection() {
        let mut work = session("$1", "work");
        work.windows = vec![window("@1", 0), window("@2", 1)];
        work.active_window_id = Some("@2".to_owned());
        let snapshot = MuxSnapshot {
            sessions: vec![work],
            active_session_id: Some("$1".to_owned()),
        };

        // tmux's active window moved (@1 -> @2) since the last snapshot, so the
        // highlight follows it even though the local selection still points at @1.
        assert_eq!(
            selected_window_after_refresh(Some("$1"), Some("@1".to_owned()), Some("@1"), &snapshot),
            Some("@2".to_owned())
        );
        // No external change (@2 unchanged): the optimistic local selection wins,
        // so a just-issued local switch doesn't get reverted by a lagging snapshot.
        assert_eq!(
            selected_window_after_refresh(Some("$1"), Some("@1".to_owned()), Some("@2"), &snapshot),
            Some("@1".to_owned())
        );
    }
}
