use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crate::MuxBindingConfig;
use anyhow::Context as _;
use bootty_control::CommandCancellation;
use serde::{Deserialize, Serialize};

use crate::{
    command::MuxCommand,
    native::{NativeBackend, protocol::TopologySnapshot},
    snapshot::{MuxSession, MuxSnapshot, selection_after_refresh, session_matches},
};

pub type RepaintHandle = Arc<dyn Fn() + Send + Sync + 'static>;

/// How often a focused window reads the shared native topology.
/// Other Bootty windows can change the shared native state between commands.
pub const MUX_SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
/// A lower refresh cadence for unfocused windows.
pub const MUX_SESSION_REFRESH_INTERVAL_UNFOCUSED: Duration = Duration::from_secs(2);
static NEXT_BINDING_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_binding_generation() -> u64 {
    NEXT_BINDING_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// The session-poll cadence a window with this focus state should use.
#[must_use]
pub const fn mux_session_refresh_interval(focused: bool) -> Duration {
    if focused {
        MUX_SESSION_REFRESH_INTERVAL
    } else {
        MUX_SESSION_REFRESH_INTERVAL_UNFOCUSED
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuxSessionRefreshOutcome {
    pub applied: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuxCommandError {
    Cancelled,
    DeadlineExceeded,
    Unavailable,
    Stale,
    Failed(String),
    LaunchFailed(String),
}

impl std::fmt::Display for MuxCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("command was cancelled"),
            Self::DeadlineExceeded => formatter.write_str("command deadline expired"),
            Self::Unavailable => formatter.write_str("mux operation is unavailable"),
            Self::Stale => formatter.write_str("mux target is stale"),
            Self::Failed(message) | Self::LaunchFailed(message) => formatter.write_str(message),
        }
    }
}

fn mux_failure(error: &anyhow::Error) -> MuxCommandError {
    if let Some(error) = error.downcast_ref::<MuxCommandError>() {
        return error.clone();
    }
    if error.is::<crate::native::ServerChanged>() {
        return MuxCommandError::Stale;
    }
    if error.is::<crate::native::RequestExpired>() {
        return MuxCommandError::DeadlineExceeded;
    }
    if error.is::<crate::native::RequestCanceled>() {
        return MuxCommandError::Cancelled;
    }
    if error.is::<crate::native::protocol::LaunchFailure>() {
        MuxCommandError::LaunchFailed(format!("{error:#}"))
    } else {
        MuxCommandError::Failed(format!("{error:#}"))
    }
}

impl std::error::Error for MuxCommandError {}

pub type MuxCommandResult = std::result::Result<MuxCommandCompletion, MuxCommandError>;

/// Which selection an authoritative command publishes when its result lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSelection {
    /// Select the command's session, and the window it moved to.
    Follow,
    /// Keep whatever the binding has selected when the result lands. Work done on a caller's
    /// behalf in the background, such as creating or closing another session, must not move the
    /// user's focus. A selected session that no longer exists still falls back as usual.
    Preserve,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MuxCommandCompletion {
    /// The selection the command asked for. Both are `None` for a command that preserves it.
    pub selected_session: Option<String>,
    pub selected_window: Option<String>,
    preserve_selection: bool,
    snapshot: Option<(MuxBindingConfig, TopologySnapshot)>,
}

impl MuxCommandCompletion {
    #[must_use]
    pub const fn preserves_selection(&self) -> bool {
        self.preserve_selection
    }

    #[must_use]
    pub fn matches_config(&self, config: &MuxBindingConfig) -> bool {
        self.snapshot
            .as_ref()
            .is_none_or(|(completed_config, _)| completed_config == config)
    }

    const fn requested(selected_session: Option<String>, selected_window: Option<String>) -> Self {
        Self {
            selected_session,
            selected_window,
            preserve_selection: false,
            snapshot: None,
        }
    }

    const fn preserving_selection() -> Self {
        Self {
            selected_session: None,
            selected_window: None,
            preserve_selection: true,
            snapshot: None,
        }
    }

    fn with_snapshot(self, config: MuxBindingConfig, snapshot: TopologySnapshot) -> Self {
        if self.preserve_selection {
            return Self {
                snapshot: Some((config, snapshot)),
                ..self
            };
        }
        // Detached creation need not change the backend's active session.
        let requested = self.selected_session.filter(|selected| {
            snapshot
                .snapshot
                .sessions
                .iter()
                .any(|session| session_matches(session, selected))
        });
        let selected_session = selection_after_refresh(
            requested.or_else(|| snapshot.snapshot.active_session_id.clone()),
            &snapshot.snapshot.sessions,
        );
        let selected_window = selected_window_after_refresh(
            selected_session.as_deref(),
            self.selected_window,
            &snapshot.snapshot,
        );
        Self {
            selected_session,
            selected_window,
            preserve_selection: false,
            snapshot: Some((config, snapshot)),
        }
    }
}

fn selected_window_after_refresh(
    selected_session: Option<&str>,
    current: Option<String>,
    snapshot: &MuxSnapshot,
) -> Option<String> {
    let selected_session = selected_session?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == selected_session || session.name == selected_session)?;
    current
        .filter(|window_id| session.windows.iter().any(|window| &window.id == window_id))
        .or_else(|| session.active_window_id.clone())
}

fn optimistic_window_after_command(
    sessions: &[MuxSession],
    selected_window: Option<&str>,
    command: &MuxCommand,
) -> Option<String> {
    let (session_id, step) = match command {
        MuxCommand::ActivateWindow { window_id, .. }
        | MuxCommand::ActivatePane { window_id, .. } => return Some(window_id.clone()),
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
        MuxCommand::MoveWindow {
            session_id,
            window_id,
            ..
        } => {
            let session = sessions
                .iter()
                .find(|session| session.id == *session_id || session.name == *session_id)?;
            let current_id = window_id
                .as_deref()
                .or(selected_window)
                .or(session.active_window_id.as_deref())?;
            return session
                .windows
                .iter()
                .any(|window| window.id == current_id)
                .then(|| current_id.to_owned());
        }
        MuxCommand::MoveWindowPreservingSelection {
            session_id,
            selected_window_id,
            ..
        } => {
            let session = sessions
                .iter()
                .find(|session| session.id == *session_id || session.name == *session_id)?;
            return session
                .windows
                .iter()
                .any(|window| window.id == *selected_window_id)
                .then(|| selected_window_id.clone());
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
    let next = crate::snapshot::wrap_index(current, step, session.windows.len())?;
    session.windows.get(next).map(|window| window.id.clone())
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
    ordered
}

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
pub struct SpaceId(i64);

impl SpaceId {
    #[must_use]
    pub const fn from_persistence(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn persistence_value(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MuxResourceKey {
    Session(String),
    Window(String, String),
    Pane(String, String, String),
}

#[derive(Clone, Debug)]
enum BindingAvailabilityError {
    Configured(String),
    Runtime(String),
}

impl BindingAvailabilityError {
    fn message(&self) -> &str {
        match self {
            Self::Configured(message) | Self::Runtime(message) => message,
        }
    }
}

impl MuxResourceKey {
    fn generation_in(
        &self,
        generations: &BTreeMap<Self, u64>,
        observed: &BTreeMap<Self, String>,
    ) -> Option<u64> {
        observed
            .contains_key(self)
            .then(|| generations.get(self).copied())
            .flatten()
    }
}

pub(crate) struct CommandFence(Arc<AtomicBool>);
impl CommandFence {
    pub(crate) fn claim(&self) -> Result<(), MuxCommandError> {
        self.0
            .load(Ordering::Acquire)
            .then_some(())
            .ok_or(MuxCommandError::Stale)
    }
}

pub struct MuxController {
    last_error: Option<String>,
    availability_error: Option<BindingAvailabilityError>,
    binding_generation: u64,
    server_generation: Option<u64>,
    server_revision: Option<u64>,
    resource_generations: BTreeMap<MuxResourceKey, u64>,
    observed_resources: BTreeMap<MuxResourceKey, String>,
    sessions: Vec<MuxSession>,
    all_sessions: Vec<MuxSession>,
    backend_session_names: Vec<String>,
    selected_session: Option<String>,
    previous_selected_session: Option<String>,
    selected_window: Option<String>,
    alive: Arc<AtomicBool>,
    backend: NativeBackend,
    pending_inline: Vec<mpsc::Receiver<MuxCommandResult>>,
    last_refresh: Option<Instant>,
    has_snapshot: bool,
}

impl Drop for MuxController {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}
impl MuxController {
    pub(crate) fn command_fence(&self, _config: &MuxBindingConfig) -> CommandFence {
        CommandFence(Arc::clone(&self.alive))
    }

    #[must_use]
    pub fn new(workspace: Option<&Path>) -> Self {
        Self::with_backend(workspace.map_or_else(NativeBackend::new, NativeBackend::for_workspace))
    }

    pub(crate) fn with_backend(backend: NativeBackend) -> Self {
        Self {
            alive: Arc::new(AtomicBool::new(true)),
            backend,
            pending_inline: Vec::new(),
            last_refresh: None,
            has_snapshot: false,
            last_error: None,
            availability_error: None,
            binding_generation: next_binding_generation(),
            server_generation: None,
            server_revision: None,
            resource_generations: BTreeMap::new(),
            observed_resources: BTreeMap::new(),
            sessions: Vec::new(),
            all_sessions: Vec::new(),
            backend_session_names: Vec::new(),
            selected_session: None,
            previous_selected_session: None,
            selected_window: None,
        }
    }

    #[must_use]
    pub fn native_backend(&self) -> NativeBackend {
        self.backend.clone()
    }

    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn set_error(&mut self, error: Option<String>) {
        self.last_error = error;
    }

    pub fn set_availability_error(&mut self, error: Option<String>) {
        self.set_availability(error.map(BindingAvailabilityError::Runtime));
    }

    pub fn set_configured_availability_error(&mut self, error: Option<String>) {
        self.set_availability(error.map(BindingAvailabilityError::Configured));
    }

    fn set_availability(&mut self, error: Option<BindingAvailabilityError>) {
        self.last_error = error.as_ref().map(|error| error.message().to_owned());
        self.availability_error = error;
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.availability_error
            .as_ref()
            .map(BindingAvailabilityError::message)
    }

    #[must_use]
    pub const fn binding_generation(&self) -> u64 {
        self.binding_generation
    }

    #[must_use]
    pub fn session_generation(&self, session_id: &str) -> Option<u64> {
        MuxResourceKey::Session(session_id.to_owned())
            .generation_in(&self.resource_generations, &self.observed_resources)
    }

    #[must_use]
    pub fn window_generation(&self, session_id: &str, window_id: &str) -> Option<u64> {
        MuxResourceKey::Window(session_id.to_owned(), window_id.to_owned())
            .generation_in(&self.resource_generations, &self.observed_resources)
    }

    #[must_use]
    pub fn pane_generation(&self, session_id: &str, window_id: &str, pane_id: &str) -> Option<u64> {
        MuxResourceKey::Pane(
            session_id.to_owned(),
            window_id.to_owned(),
            pane_id.to_owned(),
        )
        .generation_in(&self.resource_generations, &self.observed_resources)
    }

    #[must_use]
    pub fn terminal_generation(
        &self,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> Option<u64> {
        self.pane_generation(session_id, window_id, pane_id)
    }

    fn record_resource_snapshot(&mut self) {
        let mut current = BTreeMap::new();
        for session in self.sessions() {
            current.insert(MuxResourceKey::Session(session.id.clone()), String::new());
            for window in &session.windows {
                current.insert(
                    MuxResourceKey::Window(session.id.clone(), window.id.clone()),
                    String::new(),
                );
                for pane in std::iter::once(&window.anchor).chain(&window.panes) {
                    let Some(pane_id) = &pane.pane_id else {
                        continue;
                    };
                    current.insert(
                        MuxResourceKey::Pane(
                            session.id.clone(),
                            window.id.clone(),
                            pane_id.clone(),
                        ),
                        pane.pane_pid.map_or_else(
                            || format!("process:{:?}", pane.process),
                            |pid| format!("pid:{pid}"),
                        ),
                    );
                }
            }
        }
        for (key, fingerprint) in &current {
            let reappeared = !self.observed_resources.contains_key(key);
            let occupant_changed = self
                .observed_resources
                .get(key)
                .is_some_and(|previous| previous != fingerprint);
            match self.resource_generations.get_mut(key) {
                Some(generation) if reappeared || occupant_changed => {
                    *generation = generation.saturating_add(1);
                }
                Some(_) => {}
                None => {
                    self.resource_generations.insert(key.clone(), 1);
                }
            }
        }
        self.observed_resources = current;
    }

    pub const fn refresh_on_next_frame(&mut self) {
        self.last_refresh = None;
    }

    #[must_use]
    pub const fn has_session_snapshot(&self) -> bool {
        self.has_snapshot && self.availability_error.is_none()
    }

    #[must_use]
    pub fn sessions(&self) -> &[MuxSession] {
        &self.sessions
    }

    #[must_use]
    pub fn all_sessions(&self) -> &[MuxSession] {
        if self.all_sessions.is_empty() {
            &self.sessions
        } else {
            &self.all_sessions
        }
    }

    #[must_use]
    pub fn session_by_id_or_name(&self, key: &str) -> Option<&MuxSession> {
        self.sessions()
            .iter()
            .find(|session| session_matches(session, key))
    }

    #[must_use]
    pub fn backend_session_by_id_or_name(&self, key: &str) -> Option<&MuxSession> {
        self.all_sessions()
            .iter()
            .find(|session| session_matches(session, key))
    }

    #[must_use]
    pub fn backend_session_names(&self) -> &[String] {
        &self.backend_session_names
    }

    #[must_use]
    pub fn selected_session(&self) -> Option<&str> {
        self.selected_session.as_deref()
    }

    pub fn restore_selection(&mut self, session_id: String, window_id: Option<String>) {
        self.selected_session = Some(session_id);
        self.selected_window = window_id;
    }

    #[must_use]
    pub fn previous_selected_session(&self) -> Option<&str> {
        let selected = self.previous_selected_session.as_deref()?;
        self.sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.id.as_str())
    }

    fn selected_session_snapshot(&self) -> Option<&MuxSession> {
        let selected = self.selected_session.as_deref()?;
        self.sessions
            .iter()
            .find(|session| session_matches(session, selected))
    }

    fn selected_window_snapshot(&self) -> Option<&crate::snapshot::MuxWindow> {
        let session = self.selected_session_snapshot()?;
        self.selected_window
            .as_deref()
            .and_then(|id| session.windows.iter().find(|window| window.id == id))
            .or_else(|| {
                session
                    .active_window_id
                    .as_deref()
                    .and_then(|id| session.windows.iter().find(|window| window.id == id))
            })
            .or_else(|| session.windows.first())
    }

    #[must_use]
    pub fn selected_session_anchor(&self) -> Option<&crate::snapshot::MuxPaneAnchor> {
        self.selected_window_snapshot()
            .map(|window| &window.anchor)
            .or_else(|| {
                self.selected_session_snapshot()
                    .map(|session| &session.anchor)
            })
    }

    #[must_use]
    pub fn selected_session_windows(&self) -> &[crate::snapshot::MuxWindow] {
        self.selected_session_snapshot()
            .map_or_else(Default::default, |session| session.windows.as_slice())
    }

    /// Panes of the selected window (the active window of the selected session unless a specific
    /// window is selected). Native renders these as a split layout; other backends report a single
    /// attach anchor.
    #[must_use]
    pub fn selected_window_panes(&self) -> &[crate::snapshot::MuxPaneAnchor] {
        self.selected_window_snapshot()
            .map_or_else(Default::default, |window| window.panes.as_slice())
    }

    #[must_use]
    pub fn selected_window_layout(&self) -> Option<&crate::snapshot::MuxPaneLayout> {
        self.selected_window_snapshot()
            .and_then(|window| window.layout.as_ref())
    }

    pub fn apply_session_order(&mut self, ordered_names: &[String]) {
        self.sessions = order_sessions_by_names(self.all_sessions(), ordered_names);
        if self.sessions.is_empty() {
            return;
        }
        // A session this binding is still waiting on keeps the selection: it belongs to the order
        // already, it is just missing from the backend list the order was applied to.
        if self.selected_session.as_deref().is_none_or(|selected| {
            !self
                .sessions
                .iter()
                .any(|session| session_matches(session, selected))
        }) {
            self.set_selected_session(self.sessions.first().map(|session| session.id.clone()));
            self.selected_window = None;
        }
    }

    #[must_use]
    pub fn selected_window(&self) -> Option<&str> {
        self.selected_window.as_deref().or_else(|| {
            self.selected_window_snapshot()
                .map(|window| window.id.as_str())
        })
    }

    pub fn refresh_sessions(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        _interval: Duration,
    ) -> MuxSessionRefreshOutcome {
        if let Some(BindingAvailabilityError::Configured(error)) = &self.availability_error {
            return MuxSessionRefreshOutcome {
                applied: false,
                error: Some(error.clone()),
            };
        }
        let mut ready = Vec::new();
        self.pending_inline.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                ready.push(result);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                ready.push(Err(MuxCommandError::Failed(
                    "native mux command worker stopped".into(),
                )));
                false
            }
        });
        for result in ready {
            if let Err(error) = self.complete_authoritative_command(result, config) {
                self.last_error = Some(error.to_string());
            }
        }
        match self.backend.latest_snapshot(repaint) {
            Ok(None) => MuxSessionRefreshOutcome::default(),
            Ok(Some(snapshot)) => {
                let applied = self.apply_refreshed_snapshot(snapshot);
                self.last_refresh = Some(Instant::now());
                self.set_availability_error(None);
                self.record_resource_snapshot();
                MuxSessionRefreshOutcome {
                    applied,
                    error: None,
                }
            }
            Err(error) => {
                let error = error.to_string();
                self.set_availability_error(Some(error.clone()));
                MuxSessionRefreshOutcome {
                    applied: false,
                    error: Some(error),
                }
            }
        }
    }

    /// # Errors
    /// Returns the command failure without publishing a new backend snapshot.
    pub fn complete_authoritative_command(
        &mut self,
        result: MuxCommandResult,
        config: &MuxBindingConfig,
    ) -> MuxCommandResult {
        let result = self.complete_authoritative_command_inner(result, Some(config));
        // A failure goes back to whoever asked, and only the host knows whether that is the person
        // at the window or a socket client, so it is not this binding's error to show. Success
        // still clears one this binding reported.
        if result.is_ok() {
            self.last_error = None;
            self.record_resource_snapshot();
        }
        result
    }

    fn complete_authoritative_command_inner(
        &mut self,
        result: MuxCommandResult,
        active_config: Option<&MuxBindingConfig>,
    ) -> MuxCommandResult {
        match result {
            Ok(completion) => {
                if let Some((config, snapshot)) = &completion.snapshot {
                    if active_config.is_some_and(|active| active != config) {
                        return Err(MuxCommandError::Stale);
                    }
                    // A preserving command reads the selection now, not when it was submitted, so
                    // a switch made while it ran is kept.
                    let (session, window) = if completion.preserve_selection {
                        (self.selected_session.clone(), self.selected_window.clone())
                    } else {
                        (
                            completion.selected_session.clone(),
                            completion.selected_window.clone(),
                        )
                    };
                    self.apply_server_snapshot(snapshot.clone(), session, window);
                } else if !completion.preserve_selection {
                    match (&completion.selected_session, &completion.selected_window) {
                        (Some(session), Some(window)) => {
                            self.set_selected_session(Some(session.clone()));
                            self.selected_window = Some(window.clone());
                        }
                        (Some(session), None) => self.activate_session(session),
                        (None, Some(window)) => self.selected_window = Some(window.clone()),
                        (None, None) => {}
                    }
                }
                self.last_refresh = None;
                Ok(completion)
            }
            Err(error) => Err(error),
        }
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

    /// The backend id behind the current selection. Selection resolves by name or id, and only the
    /// id survives a rename, so commands that rename carry the id.
    #[must_use]
    pub fn selected_session_id(&self) -> Option<String> {
        let selected = self.selected_session.as_deref()?;
        Some(
            self.sessions
                .iter()
                .chain(self.all_sessions.iter())
                .find(|session| session_matches(session, selected))
                .map_or_else(|| selected.to_owned(), |session| session.id.clone()),
        )
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
        config: &MuxBindingConfig,
    ) {
        self.execute_and_apply_inline_command(
            repaint,
            config,
            MuxCommand::ActivateWindow {
                session_id: session_id.to_owned(),
                window_id: window_id.to_owned(),
            },
            Some(session_id.to_owned()),
            Some(window_id.to_owned()),
        );
        repaint();
    }
    pub fn rename_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        name: String,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        let command = MuxCommand::RenameWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
            name,
        };
        self.execute_preserving_selection(repaint, config, command);
    }

    pub fn rename_session(
        &mut self,
        session_id: &str,
        name: String,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        // Names change here; ids do not. Pin the selection to its id first so it still resolves once
        // the session answers to the new name, whichever backend applies the rename.
        self.selected_session = self.selected_session_id();
        let command = MuxCommand::RenameSession {
            session_id: session_id.to_owned(),
            name,
        };
        self.execute_preserving_selection(repaint, config, command);
    }

    pub fn close_pane(
        &mut self,
        session_id: &str,
        pane_id: Option<&str>,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) {
        self.execute_preserving_selection(
            repaint,
            config,
            MuxCommand::ClosePane {
                session_id: session_id.to_owned(),
                pane_id: pane_id.map(str::to_owned),
            },
        );
    }

    fn execute_preserving_selection(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
    ) {
        self.execute_and_apply_inline_command(
            repaint,
            config,
            command,
            self.selected_session.clone(),
            self.selected_window.clone(),
        );
        repaint();
    }

    pub fn execute_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
    ) {
        let (session, window) = self.command_completion(&command);
        self.execute_and_apply_inline_command(repaint, config, command, session, window);
        repaint();
        self.record_resource_snapshot();
    }

    pub fn execute_command_authoritatively(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
        selection: CommandSelection,
        execution: (Instant, &CommandCancellation),
        persist: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) -> mpsc::Receiver<MuxCommandResult> {
        let (deadline, cancellation) = execution;
        let (response_tx, response_rx) = mpsc::channel();
        if self.availability_error.is_some() {
            let _ = response_tx.send(Err(MuxCommandError::Unavailable));
            return response_rx;
        }
        let completion = match selection {
            CommandSelection::Follow => {
                let (selected_session, selected_window) = self.command_completion(&command);
                MuxCommandCompletion::requested(selected_session, selected_window)
            }
            CommandSelection::Preserve => MuxCommandCompletion::preserving_selection(),
        };
        if cancellation.is_cancelled() {
            let _ = response_tx.send(Err(MuxCommandError::Cancelled));
            return response_rx;
        }
        if Instant::now() >= deadline {
            let _ = cancellation.cancel();
            let _ = response_tx.send(Err(MuxCommandError::DeadlineExceeded));
            return response_rx;
        }
        // A window already present in the server snapshot can be presented immediately.
        // Its focus mutation still completes only after the server acknowledges the command.
        if selection == CommandSelection::Follow
            && matches!(
                command,
                MuxCommand::ActivateWindow { .. }
                    | MuxCommand::ActivateWindowIndex { .. }
                    | MuxCommand::ActivateNextWindow { .. }
                    | MuxCommand::ActivatePreviousWindow { .. }
                    | MuxCommand::ActivateLastWindow { .. }
            )
        {
            if let Some(session) = &completion.selected_session {
                self.set_selected_session(Some(session.clone()));
            }
            if let Some(window) = &completion.selected_window {
                self.selected_window = Some(window.clone());
            }
        }
        let fence = self.command_fence(config);
        let pending = self.backend.submit_persisted(
            command,
            move || {
                fence.claim()?;
                persist()
            },
            deadline,
            cancellation.clone(),
            self.server_generation
                .or_else(|| self.backend.peer_generation()),
        );
        let repaint = Arc::clone(repaint);
        let config = config.clone();
        std::thread::spawn(move || {
            let result = pending
                .recv()
                .context("native mux client worker stopped")
                .and_then(|result| result)
                .map_err(|error| mux_failure(&error))
                .map(|snapshot| completion.with_snapshot(config, snapshot));
            let _ = response_tx.send(result);
            repaint();
        });
        response_rx
    }

    fn command_completion(&self, command: &MuxCommand) -> (Option<String>, Option<String>) {
        (
            Some(command.session_id().to_owned()),
            optimistic_window_after_command(
                &self.sessions,
                self.selected_window.as_deref(),
                command,
            ),
        )
    }

    fn execute_and_apply_inline_command(
        &mut self,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
        command: MuxCommand,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) {
        if let Some(session) = preferred_session.as_deref()
            && self.sessions.iter().any(|s| session_matches(s, session))
        {
            self.set_selected_session(Some(session.to_owned()));
            if preferred_window.as_ref().is_some_and(|window| {
                self.sessions.iter().any(|s| {
                    session_matches(s, session) && s.windows.iter().any(|w| &w.id == window)
                })
            }) {
                self.selected_window = preferred_window;
            }
        }
        drop(preferred_session);
        let completion = MuxCommandCompletion::preserving_selection();
        let pending = self.backend.submit_request_for_generation(
            crate::native::protocol::Request::Execute(command),
            self.server_generation
                .or_else(|| self.backend.peer_generation()),
        );
        let config = config.clone();
        let repaint = Arc::clone(repaint);
        let (tx, rx) = mpsc::channel();
        self.pending_inline.push(rx);
        std::thread::spawn(move || {
            let result = pending
                .recv()
                .context("native mux client worker stopped")
                .and_then(|result| result)
                .and_then(|response| match response {
                    crate::native::protocol::Response::Snapshot(snapshot) => Ok(snapshot),
                    _ => anyhow::bail!("native mux command returned no topology"),
                })
                .map(|snapshot| completion.with_snapshot(config, snapshot))
                .map_err(|error| mux_failure(&error));
            let _ = tx.send(result);
            repaint();
        });
    }

    pub(crate) const fn server_generation(&self) -> Option<u64> {
        self.server_generation
    }
    fn observe_server_generation(&mut self, generation: u64) {
        if self
            .server_generation
            .is_some_and(|previous| previous != generation)
        {
            self.binding_generation = next_binding_generation();
            self.observed_resources.clear();
            self.pending_inline.clear();
        }
        if self.server_generation != Some(generation) {
            self.server_revision = None;
        }
        self.server_generation = Some(generation);
    }

    pub(crate) fn accept_server_snapshot(&mut self, snapshot: TopologySnapshot) {
        if self.apply_refreshed_snapshot(snapshot) {
            self.record_resource_snapshot();
        }
    }

    fn apply_refreshed_snapshot(&mut self, snapshot: TopologySnapshot) -> bool {
        self.apply_server_snapshot(
            snapshot,
            self.selected_session.clone(),
            self.selected_window.clone(),
        )
    }

    fn apply_server_snapshot(
        &mut self,
        snapshot: TopologySnapshot,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> bool {
        if self
            .backend
            .peer_generation()
            .or(self.server_generation)
            .is_some_and(|generation| generation != snapshot.generation)
            || self.server_generation == Some(snapshot.generation)
                && self
                    .server_revision
                    .is_some_and(|revision| revision > snapshot.revision)
        {
            return false;
        }
        // Command completions and pushed events can arrive in a different order.
        // A delayed completion must never retire resources from a newer snapshot.
        self.observe_server_generation(snapshot.generation);
        self.server_revision = Some(snapshot.revision);
        self.apply_snapshot(snapshot.snapshot, preferred_session, preferred_window)
    }

    fn apply_snapshot(
        &mut self,
        mut snapshot: MuxSnapshot,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> bool {
        self.has_snapshot = true;
        self.backend_session_names = snapshot
            .sessions
            .iter()
            .map(|session| session.name.clone())
            .collect();
        snapshot.sessions = stable_session_order(&self.sessions, snapshot.sessions);
        self.set_selected_session(selection_after_refresh(
            preferred_session,
            &snapshot.sessions,
        ));
        self.selected_window = selected_window_after_refresh(
            self.selected_session.as_deref(),
            preferred_window,
            &snapshot,
        );
        self.all_sessions = snapshot.sessions;
        self.sessions = self.all_sessions.clone();
        true
    }
}
