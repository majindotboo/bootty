use crate::pane_layout::PaneLayout;
use crate::terminal_config::terminal_session_config_with_side_effects;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::{
    RepaintHandle,
    command::MuxCommand,
    controller::{
        MuxCommandError, MuxCommandResult, MuxController, SpaceId, mux_session_refresh_interval,
    },
    snapshot::{MuxSession, MuxSessionTag},
    terminal::ActiveTerminal,
};
use anyhow::Result;
use bootty_config::config::{AppearanceVariant, BoottyConfig, RestoreOnStartup};
use bootty_terminal::terminal_engine::{TerminalLiveConfig, TerminalSideEffectEvent};
use bootty_terminal::terminal_session::DrainStats;

mod binding_panes;
mod binding_session_names;
mod binding_terminal_facts;
mod binding_windows;
mod mux_config;
mod pane_io;
pub use mux_config::RemoteSpaceUnavailable;
mod session_navigation;
mod session_requests;
mod session_snapshots;
pub use session_snapshots::{PreparedSessionCheckpoint, SavedSessionCheckpoint};
mod space_summary;
mod workspace_sessions;

use self::{binding_terminal_facts::BindingTerminalFacts, mux_config::realize_binding};

pub use binding_panes::mux_split_direction;
pub use binding_terminal_facts::{TerminalProgress, TerminalProgressState};
pub use binding_windows::terminal_cwd_for_mux_command;
pub use session_navigation::{BindingSessionGroup, ScopedSessionTarget};
pub use session_requests::{
    PreparedSessionRequest, SESSION_ARGV_MAX_ELEMENTS, SESSION_NAME_MAX_BYTES,
    SavedSessionMetadata, SessionRequestError, StartingSession,
};
pub use space_summary::{NewSpace, SpaceSummary};

use crate::repository::{
    BindingMembershipMutation, SpaceMuxOverride, SpaceRemoteOverride, WorkspaceBinding,
    WorkspacePersistenceError, WorkspaceRepository, WorkspaceSpace,
};
use crate::session_membership::{SessionMembership, SessionState, WorkspaceSession};

/// The only terminal data that the host needs to interpret after a workspace frame.
///
/// The workspace drains every live terminal. It returns only the active drain statistics and
/// active terminal side effects. Bell and shell lifecycle events survive Space switches; other
/// inactive side effects are discarded because no host surface owns them.
pub struct WorkspaceDrainResult {
    pub active_drain: DrainStats,
    pub active_terminal_side_effects: Vec<TerminalSideEffectEvent>,
    pub terminal_notifications: Vec<(SpaceId, u64, TerminalSideEffectEvent)>,
}

const fn is_terminal_notification(
    effect: &bootty_terminal::terminal_side_effect::TerminalSideEffect,
) -> bool {
    use bootty_terminal::terminal_side_effect::TerminalSideEffect;
    matches!(
        effect,
        TerminalSideEffect::Bell
            | TerminalSideEffect::ShellLifecycle(_)
            | TerminalSideEffect::ClipboardPacket(_)
            | TerminalSideEffect::ClipboardReset
    )
}

pub struct WorkspaceFrameOutcome {
    pub next_wake: Option<Duration>,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceUpdateOutcome {
    pub changed: bool,
    pub active_placement_changed: bool,
}

pub enum SpaceCloseOutcome {
    Closed { active_changed: bool },
    LastSpace,
    Unchanged,
}

#[derive(Clone, Debug)]
pub struct PendingGeneratedName {
    /// The name asked of the backend, unique across the whole server.
    name: String,
    /// What bootty calls it, which drops any uniqueness suffix `name` had to carry.
    display_name: String,
    /// Whether the user chose the name instead of Bootty generating it.
    explicit: bool,
    initial_state: crate::session_membership::SessionState,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ScopedWindowId {
    scope: SpaceId,
    session_id: String,
    window_id: String,
}

impl ScopedWindowId {
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    #[must_use]
    pub fn window_id(&self) -> &str {
        &self.window_id
    }

    #[must_use]
    pub const fn new(scope: SpaceId, session_id: String, window_id: String) -> Self {
        Self {
            scope,
            session_id,
            window_id,
        }
    }
}

/// Identity for per-pane terminal facts. Pane ids are unique within a binding, so the enclosing
/// window is not part of the key: a pane that moves between windows keeps its recorded facts, and
/// neither a read nor a write has to search the topology for its window.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct ScopedPaneId {
    pub(super) scope: SpaceId,
    pub(super) pane_id: String,
}

pub(super) struct NativeTerminalOwner {
    pub(super) terminal: Box<ActiveTerminal>,
    pub(super) terminal_side_effect_tx: mpsc::Sender<TerminalSideEffectEvent>,
    pub(super) terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffectEvent>,
}

impl NativeTerminalOwner {
    pub(super) fn new(
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<Self> {
        let (terminal_side_effect_tx, terminal_side_effect_rx) = mpsc::channel();
        let session_config =
            terminal_session_config_with_side_effects(config, variant, &terminal_side_effect_tx);
        Ok(Self {
            terminal: Box::new(ActiveTerminal::new(
                bootty_terminal::geometry::TerminalSurface::for_logical_size(
                    1000.0,
                    672.0,
                    bootty_terminal::geometry::CellMetrics::default(),
                    bootty_terminal::geometry::TerminalPadding::default(),
                )
                .geometry(),
                session_config,
                repaint,
            )?),
            terminal_side_effect_tx,
            terminal_side_effect_rx,
        })
    }

    fn publish_config(
        &mut self,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        live_config: Option<&TerminalLiveConfig>,
    ) -> Result<()> {
        self.terminal
            .native_backend()
            .configure_vt(crate::terminal_config::terminal_vt_policy(config, variant));
        let session = terminal_session_config_with_side_effects(
            config,
            variant,
            &self.terminal_side_effect_tx,
        );
        self.terminal
            .native_backend()
            .configure_launch(session.launch.clone());
        self.terminal.set_terminal_config(session);
        if let Some(config) = live_config {
            self.terminal.apply_live_config(config.clone())?;
        }
        Ok(())
    }
}

pub struct BindingRuntime {
    scope: SpaceId,
    label: String,
    placement: SpaceMuxOverride,
    multiplexer: bootty_config::config::MultiplexerConfig,
    /// The Space id stamped onto every session this binding creates. A remote binding uses the
    /// id the far side knows its Space by, since that is what its daemon filters on.
    space_tag: String,
    terminal_owner: NativeTerminalOwner,
    mux: MuxController,
    sessions: SessionMembership,
    pub(super) pending_generated_names: HashMap<String, PendingGeneratedName>,
    /// Session roots already resolved, keyed by the raw directory the backend reported.
    ///
    /// Resolving one forks `git` to find the checkout root, and the frame path asks for every
    /// session's directory on every frame. A directory's checkout root only changes when its
    /// repository layout does, so each one is resolved once per run; restart bootty if a path's
    /// layout changes underneath it.
    session_roots: RefCell<HashMap<String, String>>,
    pub(super) pane_layouts: HashMap<ScopedWindowId, PaneLayout>,
    pending_pane_focus: Option<binding_panes::PendingPaneFocus>,
    pub(super) saved_selected_session_identity: Option<String>,
    restored_sessions: HashMap<String, session_snapshots::RestoredSessionMapping>,
    terminal_facts: BindingTerminalFacts,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BindingStateCandidate {
    pub(super) scope: SpaceId,
    pub(super) sessions: SessionMembership,
}

impl BindingRuntime {
    /// The Space this binding serves.
    pub const fn scope(&self) -> SpaceId {
        self.scope
    }

    /// The realized backend configuration for this binding.
    pub const fn multiplexer(&self) -> &bootty_config::config::MultiplexerConfig {
        &self.multiplexer
    }

    /// Read-only access to the live multiplexer controller.
    pub const fn mux(&self) -> &MuxController {
        &self.mux
    }

    /// Mutable access to the live multiplexer controller for a domain operation.
    pub const fn mux_mut(&mut self) -> &mut MuxController {
        &mut self.mux
    }

    /// Read-only access to the active terminal owned by this binding.
    pub fn terminal(&self) -> &ActiveTerminal {
        &self.terminal_owner.terminal
    }

    /// Mutable access to the active terminal owned by this binding.
    pub fn terminal_mut(&mut self) -> &mut ActiveTerminal {
        &mut self.terminal_owner.terminal
    }

    /// The persisted session membership owned by this binding.
    pub const fn sessions(&self) -> &SessionMembership {
        &self.sessions
    }

    fn new_with_binding_config(
        state: BindingStateCandidate,
        config: &BoottyConfig,
        placement: SpaceMuxOverride,
        realized: mux_config::RealizedMuxBinding,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<Self> {
        let BindingStateCandidate { scope, sessions } = state;
        let remote_error = realized.availability_error.clone();
        let mut binding_config = config.clone();
        binding_config.multiplexer = realized.config.clone();
        let mut terminal_owner = NativeTerminalOwner::new(&binding_config, variant, repaint)?;
        // A config path names one server workspace across all of its windows and Spaces.
        let workspace = config.config_path.as_path();
        let backend = realized
            .config
            .remote
            .as_ref()
            .and_then(bootty_config::config::RemoteConfig::as_ssh)
            .map_or_else(
                || crate::native::NativeBackend::for_workspace(workspace),
                |remote| {
                    crate::native::NativeBackend::for_remote(
                        workspace,
                        scope,
                        &realized.space_tag,
                        remote,
                    )
                },
            );
        let mux = MuxController::with_backend(backend);
        mux.native_backend().configure_launch(
            terminal_session_config_with_side_effects(
                &binding_config,
                variant,
                &terminal_owner.terminal_side_effect_tx,
            )
            .launch,
        );
        mux.native_backend()
            .configure_vt(crate::terminal_config::terminal_vt_policy(
                &binding_config,
                variant,
            ));
        terminal_owner
            .terminal
            .set_native_backend(mux.native_backend());
        let mut binding = Self {
            label: String::new(),
            placement,
            space_tag: realized.space_tag,
            multiplexer: realized.config,
            scope,
            terminal_owner,
            mux,
            sessions,
            pending_generated_names: HashMap::new(),
            session_roots: RefCell::default(),
            pane_layouts: HashMap::new(),
            pending_pane_focus: None,
            saved_selected_session_identity: None,
            restored_sessions: HashMap::new(),
            terminal_facts: BindingTerminalFacts::default(),
        };
        if let Some(error) = remote_error {
            binding.mux.set_configured_availability_error(Some(error));
        }
        Ok(binding)
    }

    pub(super) fn from_workspace(
        workspace_binding: &WorkspaceBinding,
        config: &BoottyConfig,
        space_tag: String,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) -> Result<Self> {
        let placement = SpaceMuxOverride {
            remote: workspace_binding.remote_override().clone(),
        };
        let realized = realize_binding(config, &placement.remote, space_tag);
        let mut binding = Self::new_with_binding_config(
            BindingStateCandidate {
                scope: workspace_binding.mux_scope(),
                sessions: workspace_binding.sessions().clone(),
            },
            config,
            placement,
            realized,
            variant,
            repaint.clone(),
        )?;
        binding.saved_selected_session_identity = workspace_binding
            .selected_session_identity()
            .map(str::to_owned);
        if let Some(selection) = workspace_binding.selection() {
            let window = selection.window_id();
            binding
                .mux
                .restore_selection(selection.session_id().to_owned(), window.map(str::to_owned));
        }
        Ok(binding)
    }

    pub(super) const fn placement(&self) -> &SpaceMuxOverride {
        &self.placement
    }

    fn rebuilt(
        &mut self,
        config: &BoottyConfig,
        placement: SpaceMuxOverride,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) -> Result<Self> {
        let state = BindingStateCandidate {
            scope: self.scope,
            sessions: self.sessions.clone(),
        };
        let label = self.label.clone();
        let realized = realize_binding(config, &placement.remote, self.space_tag.clone());
        let mut replacement = Self::new_with_binding_config(
            state,
            config,
            placement,
            realized,
            variant,
            repaint.clone(),
        )?;
        replacement.label = label;
        replacement.pending_generated_names = std::mem::take(&mut self.pending_generated_names);
        *replacement.session_roots.borrow_mut() = self.session_roots.take();
        Ok(replacement)
    }

    /// The stamp for a session this binding is about to create. A fresh identity every time.
    pub(super) fn new_session_tag(&self) -> MuxSessionTag {
        {}
        MuxSessionTag {
            identity: Some(crate::snapshot::new_session_identity()),
            space: (!self.space_tag.is_empty()).then(|| self.space_tag.clone()),
        }
    }

    /// The names bootty shows for `sessions`, in the same order.
    ///
    /// Names are unique across native Spaces; saved purpose titles remain independent of
    /// internal uniqueness suffixes.
    pub fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        sessions
            .iter()
            .map(|session| {
                session
                    .tag
                    .identity
                    .as_deref()
                    .and_then(|identity| self.sessions.get(identity))
                    .map_or(session.name.as_str(), WorkspaceSession::label)
                    .to_owned()
            })
            .collect()
    }

    /// The same names keyed by session id, for the UI groups that carry sessions from several
    /// bindings at once.
    pub(super) fn session_display_name_map(
        &self,
        sessions: &[MuxSession],
    ) -> HashMap<String, String> {
        sessions
            .iter()
            .map(|session| session.id.clone())
            .zip(self.session_display_names(sessions))
            .collect()
    }

    /// Bring this Space's claims in line with what the backend reports.
    ///
    /// Missing sessions survive only when their hosting server is gone or unknown.
    fn reconcile_session_state(&self, candidate: &mut BindingStateCandidate) {
        let backend = self.mux.all_sessions();
        if !self.mux.has_session_snapshot() {
            return;
        }
        let ended = candidate
            .sessions
            .sessions()
            .iter()
            .filter(|saved| {
                let attached = backend.iter().any(|session| {
                    session.tag.identity.as_deref() == Some(saved.identity.as_str())
                });
                !attached
                    && ((saved.state.server_generation.is_some()
                        && saved.state.server_generation == self.mux.server_generation())
                        || saved.terminal_snapshot.is_none())
            })
            .map(|saved| saved.identity.clone())
            .collect::<Vec<_>>();
        for identity in ended {
            candidate.sessions.release(&identity);
        }

        for session in backend {
            let Some(identity) = session.tag.identity.as_deref() else {
                continue;
            };
            if session.tag.space.as_deref() != Some(self.space_tag.as_str()) {
                continue;
            }
            if candidate.sessions.contains(identity) {
                candidate
                    .sessions
                    .observe_backend_name(identity, &session.name);
            } else {
                candidate.sessions.claim(WorkspaceSession {
                    identity: identity.to_owned(),
                    backend_name: session.name.clone(),
                    display_name: session.name.clone(),
                    explicit: false,
                    state: SessionState::default(),
                    terminal_snapshot: None,
                    cwd: session
                        .anchor
                        .cwd
                        .as_deref()
                        .map_or_else(Default::default, |cwd| self.session_cwd(cwd)),
                });
            }
            candidate
                .sessions
                .observe_server_generation(identity, self.mux.server_generation());
            // The project belongs to the saved session, not the shell's current directory.
            if candidate
                .sessions
                .get(identity)
                .is_some_and(|saved| saved.cwd.is_empty())
                && let Some(cwd) = session.anchor.cwd.as_deref()
            {
                candidate.sessions.set_cwd(identity, &self.session_cwd(cwd));
            }
        }
    }

    fn publish_session_state(&mut self, candidate: BindingStateCandidate) {
        self.sessions = candidate.sessions;
        let order = { self.sessions.backend_names() };
        self.mux.apply_session_order(&order);
    }

    pub(super) fn discard_terminal_side_effects(
        &self,
    ) -> Vec<(SpaceId, u64, TerminalSideEffectEvent)> {
        self.terminal_owner
            .terminal_side_effect_rx
            .try_iter()
            .filter(|event| is_terminal_notification(&event.effect))
            .map(|event| (self.scope, self.mux.binding_generation(), event))
            .collect()
    }

    pub const fn window_id(&self, session_id: String, window_id: String) -> ScopedWindowId {
        ScopedWindowId::new(self.scope, session_id, window_id)
    }

    pub(super) fn pane_id(&self, pane_id: impl Into<String>) -> ScopedPaneId {
        ScopedPaneId {
            scope: self.scope,
            pane_id: pane_id.into(),
        }
    }
}

pub struct SpaceRuntime {
    pub id: SpaceId,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub tint_sidebar: bool,
    pub position: i64,
    pub binding: BindingRuntime,
}

impl SpaceRuntime {
    pub(super) fn from_workspace(
        space: &WorkspaceSpace,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) -> Result<Self> {
        let binding = BindingRuntime::from_workspace(
            space.binding(),
            config,
            space.remote_id().to_owned(),
            variant,
            repaint,
        )?;
        Ok(Self {
            id: space.id(),
            name: space.name().to_owned(),
            icon: space.icon().to_owned(),
            color: space.color(),
            tint_sidebar: space.tint_sidebar(),
            position: space.position(),
            binding,
        })
    }

    pub(super) fn bindings(&self) -> impl Iterator<Item = &BindingRuntime> {
        std::iter::once(&self.binding)
    }

    fn bindings_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        std::iter::once(&mut self.binding)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SpaceTransition {
    pub(super) from: SpaceId,
    pub(super) to: SpaceId,
    pub(super) started: Instant,
}

impl SpaceTransition {
    pub(super) const DURATION: Duration = Duration::from_millis(180);

    pub(super) fn progress_at(self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32() / Self::DURATION.as_secs_f32())
            .clamp(0.0, 1.0)
    }
}

pub struct WorkspaceRuntime {
    pending_terminal_notifications: Vec<(SpaceId, u64, TerminalSideEffectEvent)>,
    repository: WorkspaceRepository,
    projects: Vec<crate::repository::RegisteredProject>,
    repaint: RepaintHandle,
    deferred_profile_binding_rebuilds: HashSet<SpaceId>,
    pending_session_completion_scopes: HashSet<SpaceId>,
    pub active: SpaceRuntime,
    inactive_spaces: Vec<SpaceRuntime>,
    space_transition: Option<SpaceTransition>,
}

impl WorkspaceRuntime {
    /// # Errors
    /// Returns an error if the window selection cannot be persisted.
    pub fn persist_window_space_selection(
        &mut self,
        window_state_key: &str,
        space_id: SpaceId,
    ) -> Result<()> {
        self.repository
            .set_selected_space(window_state_key, space_id)
            .map_err(Into::into)
    }

    #[must_use]
    pub fn project_repository(&self) -> WorkspaceRepository {
        self.repository.clone()
    }

    pub fn registered_projects(
        &self,
        scope: SpaceId,
    ) -> impl Iterator<Item = &crate::repository::RegisteredProject> {
        self.projects
            .iter()
            .filter(move |project| project.scope == scope)
    }

    /// # Errors
    /// A failed persistence commit leaves the live registry unchanged.
    pub fn register_project(&mut self, scope: SpaceId, cwd: &str) -> Result<()> {
        self.repository.register_project(scope, cwd)?;
        if !self
            .projects
            .iter()
            .any(|project| project.scope == scope && project.cwd == cwd)
        {
            self.projects.push(crate::repository::RegisteredProject {
                scope,
                cwd: cwd.to_owned(),
                collapsed: false,
                settings: crate::repository::ProjectSettings::default(),
            });
        }
        Ok(())
    }

    /// Remove only the captured Space's registration, preserving files and sessions.
    /// # Errors
    /// A failed commit leaves the live registry unchanged.
    pub fn remove_project(&mut self, scope: SpaceId, cwd: &str) -> Result<()> {
        self.repository.remove_project(scope, cwd)?;
        self.projects
            .retain(|project| project.scope != scope || project.cwd != cwd);
        Ok(())
    }

    /// # Errors
    /// A failed persistence commit leaves the live disclosure state unchanged.
    pub fn toggle_project_collapsed(&mut self, scope: SpaceId, cwd: &str) -> Result<()> {
        self.repository.toggle_project_collapsed(scope, cwd)?;
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.scope == scope && project.cwd == cwd)
        {
            project.collapsed = !project.collapsed;
        } else {
            self.projects.push(crate::repository::RegisteredProject {
                scope,
                cwd: cwd.to_owned(),
                collapsed: true,
                settings: crate::repository::ProjectSettings::default(),
            });
        }
        Ok(())
    }

    /// # Errors
    /// A failed commit leaves the live project defaults unchanged.
    pub fn configure_project(
        &mut self,
        scope: SpaceId,
        cwd: &str,
        settings: crate::repository::ProjectSettings,
    ) -> Result<()> {
        self.repository.configure_project(scope, cwd, &settings)?;
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.scope == scope && project.cwd == cwd)
        {
            project.settings = settings;
        } else {
            self.projects.push(crate::repository::RegisteredProject {
                scope,
                cwd: cwd.to_owned(),
                collapsed: false,
                settings,
            });
        }
        Ok(())
    }

    pub fn spaces(&self) -> impl Iterator<Item = &SpaceRuntime> {
        std::iter::once(&self.active).chain(self.inactive_spaces.iter())
    }

    fn spaces_mut(&mut self) -> impl Iterator<Item = &mut SpaceRuntime> {
        std::iter::once(&mut self.active).chain(self.inactive_spaces.iter_mut())
    }

    /// # Errors
    /// Returns workspace loading, configuration realization, or initialization errors.
    pub fn open(
        config: &BoottyConfig,
        window_state_key: &str,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<Self> {
        let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
        let projects = repository.registered_projects()?;
        let selected_space_id = match config.restore_on_startup {
            RestoreOnStartup::LastSession => snapshot.selected_space(window_state_key),
            RestoreOnStartup::LastWorkspace => snapshot.selected_space("main"),
            RestoreOnStartup::None => None,
        };
        let mut spaces = snapshot
            .spaces()
            .iter()
            .map(|space| {
                let runtime = SpaceRuntime::from_workspace(space, config, variant, &repaint)?;
                Ok(runtime)
            })
            .collect::<Result<Vec<_>>>()?;
        let active_index = selected_space_id
            .and_then(|id| spaces.iter().position(|space| space.id == id))
            .unwrap_or(0);
        anyhow::ensure!(
            active_index < spaces.len(),
            "workspace contains no selectable Space"
        );
        let active = spaces.remove(active_index);
        repository.set_selected_space(window_state_key, active.id)?;

        Ok(Self {
            repository,
            projects,
            repaint,
            deferred_profile_binding_rebuilds: HashSet::new(),
            pending_session_completion_scopes: HashSet::new(),
            active,
            inactive_spaces: spaces,
            space_transition: None,
            pending_terminal_notifications: Vec::new(),
        })
    }

    /// Drain every live terminal before the host interprets active terminal side effects.
    ///
    /// The host owns interpretation of active terminal side effects. The workspace owns all
    /// terminal traversal. Lifecycle work starts later in `advance_frame`.
    pub fn drain(&mut self) -> WorkspaceDrainResult {
        let active_drain = bootty_terminal::DrainStats::default();
        let mut terminal_notifications = std::mem::take(&mut self.pending_terminal_notifications);
        for binding in self.bindings_mut().skip(1) {
            terminal_notifications.extend(binding.discard_terminal_side_effects());
        }

        let active_terminal_side_effects = self
            .active
            .binding
            .terminal_owner
            .terminal_side_effect_rx
            .try_iter()
            .collect();
        WorkspaceDrainResult {
            active_drain,
            active_terminal_side_effects,
            terminal_notifications,
        }
    }

    pub fn publish_terminal_config(
        &mut self,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        live_config: Option<&TerminalLiveConfig>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for binding in self.bindings_mut() {
            let mut binding_config = config.clone();
            binding_config.multiplexer = binding.multiplexer.clone();
            if let Err(error) =
                binding
                    .terminal_owner
                    .publish_config(&binding_config, variant, live_config)
            {
                warnings.push(format!(
                    "terminal config publication failed for {:?}: {error}",
                    binding.scope
                ));
            }
        }
        warnings
    }

    /// Advance backend membership, persistence, naming, profile, and pane state for one frame.
    ///
    /// Each error is retained in order. The host applies them in order so the last error remains
    /// visible to the user.
    pub fn advance_frame(
        &mut self,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
        window_focused: bool,
    ) -> WorkspaceFrameOutcome {
        let mut errors = Vec::new();
        for binding in self.bindings_mut() {
            binding.poll_membership_command();
        }
        self.active.binding.mux.refresh_sessions(
            repaint,
            &self.active.binding.multiplexer.clone(),
            mux_session_refresh_interval(window_focused),
        );
        let requested_profile_scopes = self.deferred_profile_binding_rebuilds.clone();
        if !requested_profile_scopes.is_empty()
            && let Err(error) = self.rebuild_profile_bindings(
                config,
                Some(&requested_profile_scopes),
                variant,
                repaint,
            )
        {
            errors.push(error.to_string());
        }
        if let Err(error) = self.reconcile_binding_states() {
            errors.push(error.to_string());
        }

        if let Err(error) = self.active.binding.sync_terminal_panes() {
            errors.push(error.to_string());
        }
        WorkspaceFrameOutcome {
            next_wake: None,
            errors,
        }
    }

    /// # Errors
    /// Returns terminal startup, attachment, or pane synchronization errors.
    pub fn sync_active_terminal_panes(&mut self) -> Result<()> {
        self.active.binding.sync_terminal_panes()
    }

    pub const fn active_space_id(&self) -> SpaceId {
        self.active.id
    }

    fn space(&self, space_id: SpaceId) -> Option<&SpaceRuntime> {
        self.spaces().find(|space| space.id == space_id)
    }

    pub fn space_summaries(&self) -> Vec<SpaceSummary> {
        let mut spaces = self
            .spaces()
            .map(|space| {
                (
                    space.position,
                    SpaceSummary {
                        id: space.id,
                        name: space.name.clone(),
                        icon: space.icon.clone(),
                        color: space.color,
                        tint_sidebar: space.tint_sidebar,
                        active: space.id == self.active.id,
                        error: space.binding.mux.last_error().map(str::to_owned),
                        accepts_moves: self.session_move_is_possible(self.active.id, space.id),
                    },
                )
            })
            .collect::<Vec<_>>();
        spaces.sort_by_key(|(position, _)| *position);
        spaces.into_iter().map(|(_, summary)| summary).collect()
    }

    pub fn space_placement(&self, space_id: SpaceId) -> Option<SpaceMuxOverride> {
        self.space(space_id)
            .map(|space| space.binding.placement.clone())
    }

    pub fn transition(&self, now: Instant) -> Option<(SpaceId, SpaceId, f32)> {
        let transition = self.space_transition?;
        let progress = transition.progress_at(now);
        (progress < 1.0).then_some((transition.from, transition.to, progress))
    }

    /// Bring the freshly activated binding's multiplexer back in step with its persisted state.
    fn resume_active_binding(&mut self, repaint: &RepaintHandle) {
        if self.active.binding.sessions.is_empty() {
            return;
        }
        self.active.binding.mux.refresh_on_next_frame();
        self.active.binding.mux.refresh_sessions(
            repaint,
            &self.active.binding.multiplexer.clone(),
            mux_session_refresh_interval(true),
        );
        self.active
            .binding
            .mux
            .apply_session_order(&self.active.binding.sessions.backend_names());
    }

    /// Select a target within its exact Binding without changing the active Space.
    /// # Errors
    /// Returns a closed Binding or persistence error before publishing the requested selection.
    pub fn activate_target(
        &mut self,
        scope: SpaceId,
        session_id: &str,
        window_id: Option<&str>,
        repaint: &RepaintHandle,
    ) -> Result<()> {
        let binding = self.binding(scope).ok_or_else(|| {
            WorkspacePersistenceError::operation("The target Binding is no longer live")
        })?;
        let identity = binding
            .mux
            .backend_session_by_id_or_name(session_id)
            .and_then(|session| session.tag.identity.clone())
            .filter(|identity| binding.sessions.contains(identity));
        self.repository.set_binding_saved_selection(
            scope,
            identity.as_deref(),
            session_id,
            window_id,
        )?;
        let binding = self.binding_mut(scope).ok_or_else(|| {
            WorkspacePersistenceError::operation("The target Binding is no longer live")
        })?;
        binding.saved_selected_session_identity = identity;
        let config = binding.multiplexer.clone();
        match window_id {
            Some(window_id) => binding
                .mux
                .activate_window(session_id, window_id, repaint, &config),
            None => binding.mux.activate_session(session_id),
        }
        Ok(())
    }

    /// # Errors
    /// Returns persistence errors before publishing the requested selection.
    pub fn activate_space(
        &mut self,
        space_id: SpaceId,
        window_state_key: &str,
        repaint: &RepaintHandle,
        now: Instant,
    ) -> Result<bool, WorkspacePersistenceError> {
        if space_id == self.active.id {
            return Ok(false);
        }
        let Some(index) = self
            .inactive_spaces
            .iter()
            .position(|space| space.id == space_id)
        else {
            return Ok(false);
        };

        let selected_session = self
            .active
            .binding
            .mux
            .selected_session()
            .map(str::to_owned);
        let selected_window = self.active.binding.mux.selected_window().map(str::to_owned);
        self.repository.set_binding_restore_state(
            self.active.binding.scope,
            self.active.binding.mux.last_error().is_some(),
            selected_session.as_deref(),
            selected_window.as_deref(),
        )?;
        self.repository
            .set_selected_space(window_state_key, space_id)?;

        self.publish_space_activation(index, repaint, now);
        Ok(true)
    }

    fn publish_space_activation(&mut self, index: usize, repaint: &RepaintHandle, now: Instant) {
        let mut target = self.inactive_spaces.remove(index);
        self.pending_terminal_notifications
            .extend(self.active.binding.discard_terminal_side_effects());
        self.pending_terminal_notifications
            .extend(target.binding.discard_terminal_side_effects());
        std::mem::swap(
            &mut self.active.binding.terminal_owner,
            &mut target.binding.terminal_owner,
        );

        let current = std::mem::replace(&mut self.active, target);

        self.resume_active_binding(repaint);

        let previous_space_id = current.id;
        self.inactive_spaces.push(current);
        self.inactive_spaces.sort_by_key(|space| space.position);
        self.space_transition = Some(SpaceTransition {
            from: previous_space_id,
            to: self.active.id,
            started: now,
        });
    }

    fn space_terminal_owner(&mut self, scope: SpaceId) -> Result<&mut NativeTerminalOwner> {
        anyhow::ensure!(self.binding(scope).is_some(), "the target Space was closed");
        Ok(&mut self.active.binding.terminal_owner)
    }

    fn current_space_terminal_owner(&mut self, scope: SpaceId) -> Option<&mut NativeTerminalOwner> {
        self.binding(scope)?;
        Some(&mut self.active.binding.terminal_owner)
    }

    /// The running terminal for `pane` in `scope`, whether or not it is on screen.
    pub fn space_terminal_runtime(
        &mut self,
        scope: SpaceId,
        pane_id: &str,
    ) -> Option<&mut (dyn crate::terminal::TerminalRuntime + '_)> {
        self.current_space_terminal_owner(scope)?
            .terminal
            .scoped_terminal_runtime(scope, pane_id)
    }

    /// Forget a native pane the backend closed: kill its runtime wherever it lives and drop its
    /// layout leaf. Only the active Space re-syncs its visible panes.
    pub fn discard_space_pane(
        &mut self,
        scope: SpaceId,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) {
        if let Some(owner) = self.current_space_terminal_owner(scope) {
            owner.terminal.discard_scoped_pane(scope, pane_id);
        }
        let active = scope == self.active.binding.scope;
        if let Some(binding) = self.binding_mut(scope) {
            let window = binding.window_id(session_id.to_owned(), window_id.to_owned());
            binding.remove_pane_from_layout(&window, pane_id, active);
        }
    }

    fn rebuild_binding(
        &mut self,
        scope: SpaceId,
        config: &BoottyConfig,
        placement: SpaceMuxOverride,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) -> Result<(), WorkspacePersistenceError> {
        if self.active.id == scope {
            let mut replacement = self
                .active
                .binding
                .rebuilt(config, placement, variant, repaint)
                .map_err(|error| WorkspacePersistenceError::operation(error.to_string()))?;
            // The active binding owns shared native terminals across Spaces.
            std::mem::swap(
                &mut self.active.binding.terminal_owner,
                &mut replacement.terminal_owner,
            );
            self.active.binding = replacement;
        } else {
            let binding = self.binding_mut(scope).ok_or_else(|| {
                WorkspacePersistenceError::operation("binding to rebuild is no longer live")
            })?;
            *binding = binding
                .rebuilt(config, placement, variant, repaint)
                .map_err(|error| WorkspacePersistenceError::operation(error.to_string()))?;
        }
        Ok(())
    }

    /// # Errors
    /// Returns persistence or binding realization errors; failed writes preserve live state.
    pub fn create_space(
        &mut self,
        space: NewSpace<'_>,
        config: &BoottyConfig,
        variant: AppearanceVariant,
    ) -> Result<Option<SpaceId>, WorkspacePersistenceError> {
        let NewSpace {
            name,
            icon,
            color,
            tint_sidebar,
            mux,
        } = space;
        let Some(space) = self
            .repository
            .create_space(name, icon, color, tint_sidebar, mux)?
        else {
            return Ok(None);
        };
        let runtime = SpaceRuntime::from_workspace(&space, config, variant, &self.repaint)
            .map_err(|error| WorkspacePersistenceError::operation(error.to_string()))?;
        let id = runtime.id;
        self.inactive_spaces.push(runtime);
        self.inactive_spaces.sort_by_key(|space| space.position);
        Ok(Some(id))
    }

    /// # Errors
    /// Returns persistence or terminal preparation errors; failed closes preserve selection.
    pub fn close_space(
        &mut self,
        space_id: SpaceId,
        window_state_key: &str,
        repaint: &RepaintHandle,
        now: Instant,
    ) -> Result<SpaceCloseOutcome, WorkspacePersistenceError> {
        if self.space(space_id).is_none() {
            return Ok(SpaceCloseOutcome::Unchanged);
        }
        let active_changed = space_id == self.active.id;
        let deleted = if active_changed {
            // Spaces stay ordered; prefer the next neighbor, then the previous one.
            let Some(index) = self
                .inactive_spaces
                .iter()
                .position(|space| space.position > self.active.position)
                .or_else(|| self.inactive_spaces.len().checked_sub(1))
            else {
                return Ok(SpaceCloseOutcome::LastSpace);
            };
            let Some(target) = self.inactive_spaces.get(index) else {
                return Ok(SpaceCloseOutcome::Unchanged);
            };
            if !self
                .repository
                .delete_space_and_select(space_id, window_state_key, target.id)?
            {
                return Ok(SpaceCloseOutcome::Unchanged);
            }
            self.publish_space_activation(index, repaint, now);
            true
        } else {
            self.repository.delete_space(space_id)?
        };
        // Backend sessions survive; only the Space's claim and journal rows are removed.
        if deleted {
            self.inactive_spaces.retain(|space| space.id != space_id);
            Ok(SpaceCloseOutcome::Closed { active_changed })
        } else {
            Ok(SpaceCloseOutcome::Unchanged)
        }
    }

    /// # Errors
    /// Returns persistence or binding realization errors; failed writes preserve live state.
    pub fn update_space(
        &mut self,
        summary: &SpaceSummary,
        mux: SpaceMuxOverride,
        config: &BoottyConfig,
        variant: AppearanceVariant,
    ) -> Result<SpaceUpdateOutcome, WorkspacePersistenceError> {
        let Some(scope) = self.space(summary.id).map(|space| space.binding.scope) else {
            return Ok(SpaceUpdateOutcome {
                changed: false,
                active_placement_changed: false,
            });
        };
        let placement_changed = self
            .binding(scope)
            .is_some_and(|binding| binding.placement != mux);
        let active_placement_changed = self.active.id == scope && placement_changed;
        let space = std::iter::once(&mut self.active)
            .chain(self.inactive_spaces.iter_mut())
            .find(|space| space.id == scope)
            .ok_or_else(|| {
                WorkspacePersistenceError::operation("Space to update is no longer live")
            })?;
        let updated = self.repository.update_space(
            scope,
            &summary.name,
            &summary.icon,
            summary.color,
            summary.tint_sidebar,
            &mux,
        )?;
        if updated {
            let repaint = self.repaint.clone();
            summary.name.trim().clone_into(&mut space.name);
            summary.icon.trim().clone_into(&mut space.icon);
            space.color = summary.color;
            space.tint_sidebar = summary.tint_sidebar;
            if placement_changed {
                self.rebuild_binding(scope, config, mux, variant, &repaint)?;
            }
            return Ok(SpaceUpdateOutcome {
                changed: true,
                active_placement_changed,
            });
        }
        Ok(SpaceUpdateOutcome {
            changed: updated,
            active_placement_changed: false,
        })
    }

    /// Project accepted command scopes while the host still owns their startup or checkpoint result.
    /// The host clears finished scopes before rebuilding; this does not own command lifetime.
    pub fn set_pending_session_completion_scopes(&mut self, scopes: HashSet<SpaceId>) {
        self.pending_session_completion_scopes = scopes;
    }

    /// # Errors
    /// Returns configuration realization or persistence errors while rebuilding affected bindings.
    pub fn rebuild_profile_bindings(
        &mut self,
        config: &BoottyConfig,
        requested_scopes: Option<&HashSet<SpaceId>>,
        variant: AppearanceVariant,
        repaint: &RepaintHandle,
    ) -> Result<(), WorkspacePersistenceError> {
        let profile_scopes = self
            .all_bindings()
            .map(|binding| (binding.scope, binding.placement()))
            .filter(|(scope, _)| requested_scopes.is_none_or(|scopes| scopes.contains(scope)))
            .filter(|(_, placement)| matches!(placement.remote, SpaceRemoteOverride::Profile(_)))
            .map(|(scope, placement)| (scope, placement.clone()))
            .collect::<Vec<_>>();
        let mut pending_scopes = HashSet::new();
        for (scope, _) in &profile_scopes {
            if self.pending_session_completion_scopes.contains(scope) {
                pending_scopes.insert(*scope);
            }
        }
        self.deferred_profile_binding_rebuilds
            .extend(pending_scopes.iter().copied());
        for (scope, placement) in profile_scopes {
            if pending_scopes.contains(&scope) {
                continue;
            }
            self.rebuild_binding(scope, config, placement, variant, repaint)?;
            self.deferred_profile_binding_rebuilds.remove(&scope);
        }
        Ok(())
    }

    pub(super) fn binding_state_candidate(&self, scope: SpaceId) -> Option<BindingStateCandidate> {
        let binding = self.binding(scope)?;
        Some(BindingStateCandidate {
            scope,
            sessions: binding.sessions.clone(),
        })
    }

    pub(super) fn active_binding_state_candidate(&self) -> BindingStateCandidate {
        BindingStateCandidate {
            scope: self.active.binding.scope,
            sessions: self.active.binding.sessions.clone(),
        }
    }

    /// The identity a backend session carries, or `None` when no Space claims it.
    pub fn session_identity(&self, scope: SpaceId, session_id: &str) -> Option<String> {
        let binding = self.binding(scope)?;
        if binding.sessions.contains(session_id) {
            return Some(session_id.to_owned());
        }
        binding
            .mux
            .backend_session_by_id_or_name(session_id)?
            .tag
            .identity
            .clone()
    }

    fn active_session_identity(&self, session_id: &str) -> Option<String> {
        self.session_identity(self.active.binding.scope, session_id)
    }

    /// # Errors
    /// Returns an error if the new session order cannot be committed.
    pub fn move_active_session(
        &mut self,
        session_id: &str,
        delta: i32,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(identity) = self.active_session_identity(session_id) else {
            return Ok(false);
        };
        let mut candidate = self.active_binding_state_candidate();
        if !candidate.sessions.move_by(&identity, delta) {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate).map(|_| true)
    }

    /// # Errors
    /// Returns an error if the new session order cannot be committed.
    pub fn reorder_active_session_before(
        &mut self,
        source: &str,
        before: Option<&str>,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(source) = self.active_session_identity(source) else {
            return Ok(false);
        };
        let before = match before {
            Some(before) => match self.active_session_identity(before) {
                Some(identity) => Some(identity),
                None => return Ok(false),
            },
            None => None,
        };
        let mut candidate = self.active_binding_state_candidate();
        if !candidate.sessions.move_before(&source, before.as_deref()) {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate).map(|_| true)
    }

    /// Bring a session this Space does not hold into it, minting an identity if it has none.
    /// # Errors
    /// Returns invalid binding, membership, or persistence errors before publishing ownership.
    pub fn adopt_session_into_binding(
        &mut self,
        scope: SpaceId,
        session_id: &str,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        let Some(binding) = self.binding(scope) else {
            return Ok(false);
        };
        {}
        let Some(session) = binding.mux.backend_session_by_id_or_name(session_id) else {
            return Ok(false);
        };
        let identity = session
            .tag
            .identity
            .clone()
            .unwrap_or_else(crate::snapshot::new_session_identity);
        let claimed = WorkspaceSession {
            identity: identity.clone(),
            backend_name: session.name.clone(),
            display_name: session.name.clone(),
            explicit: false,
            cwd: session.anchor.cwd.clone().unwrap_or_default(),
            state: SessionState::default(),
            terminal_snapshot: None,
        };
        let backend_session_id = session.id.clone();
        let space_tag = binding.space_tag.clone();

        let Some(mut candidate) = self.binding_state_candidate(scope) else {
            return Ok(false);
        };
        if !candidate.sessions.claim(claimed) {
            return Ok(false);
        }
        let binding = self.commit_binding_state_candidate(candidate)?;
        let config = binding.multiplexer.clone();
        binding.mux.execute_command(
            repaint,
            &config,
            MuxCommand::StampSession {
                session_id: backend_session_id,
                tag: MuxSessionTag {
                    identity: Some(identity),
                    space: (!space_tag.is_empty()).then_some(space_tag),
                },
            },
        );
        Ok(true)
    }

    /// Whether `session_id` can move from `from` into `to`.
    ///
    /// Only local Spaces share a server workspace. Each remote Space has its own workspace,
    /// even when both use the same SSH remote.
    pub fn session_move_is_possible(&self, from: SpaceId, to: SpaceId) -> bool {
        let Some(source) = self.binding(from) else {
            return false;
        };
        {}
        self.space(to).is_some_and(|space| {
            space.binding.scope != from
                && source.multiplexer.remote.is_none()
                && space.binding.multiplexer.remote.is_none()
        })
    }

    /// Hand a session over to another Space on the same multiplexer.
    ///
    /// The session itself is untouched -- only which Space claims it changes, in both bootty's
    /// record and the tag the multiplexer holds.
    /// # Errors
    /// Returns invalid binding, membership, or persistence errors before publishing ownership.
    pub fn move_session_to_space(
        &mut self,
        from: SpaceId,
        session_id: &str,
        to: SpaceId,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        if !self.session_move_is_possible(from, to) {
            return Ok(false);
        }
        let Some(identity) = self.session_identity(from, session_id) else {
            return Ok(false);
        };
        let backend_session_id = self
            .binding(from)
            .and_then(|binding| binding.mux.backend_session_by_id_or_name(session_id))
            .map(|session| session.id.clone());
        let Some(target) = self.space(to).map(|space| space.binding.scope) else {
            return Ok(false);
        };
        let space_tag = self
            .binding(target)
            .map_or_else(Default::default, |binding| binding.space_tag.clone());

        let Some(mut source_state) = self.binding_state_candidate(from) else {
            return Ok(false);
        };
        let Some(claimed) = source_state.sessions.release(&identity) else {
            return Ok(false);
        };
        let Some(mut target_state) = self.binding_state_candidate(target) else {
            return Ok(false);
        };
        target_state.sessions.claim(claimed);
        self.commit_binding_state_candidates(vec![source_state, target_state])?;

        if let Some(backend_session_id) = backend_session_id
            && let Some(binding) = self.binding_mut(from)
        {
            let config = binding.multiplexer.clone();
            binding.mux.execute_command(
                repaint,
                &config,
                MuxCommand::StampSession {
                    session_id: backend_session_id,
                    tag: MuxSessionTag {
                        identity: Some(identity),
                        space: (!space_tag.is_empty()).then_some(space_tag),
                    },
                },
            );
        }
        Ok(true)
    }

    /// Let go of a session, leaving it running and claimed by nobody.
    /// # Errors
    /// Returns invalid binding, membership, or persistence errors before publishing ownership.
    pub fn detach_session_from_space(
        &mut self,
        scope: SpaceId,
        session_id: &str,
        repaint: &RepaintHandle,
    ) -> Result<bool, WorkspacePersistenceError> {
        if self.binding(scope).is_none() {
            return Ok(false);
        }
        let Some(identity) = self.session_identity(scope, session_id) else {
            return Ok(false);
        };
        let backend_session_id = self
            .binding(scope)
            .and_then(|binding| binding.mux.backend_session_by_id_or_name(session_id))
            .map(|session| session.id.clone());
        let Some(mut candidate) = self.binding_state_candidate(scope) else {
            return Ok(false);
        };
        if candidate.sessions.release(&identity).is_none() {
            return Ok(false);
        }
        self.commit_binding_state_candidate(candidate)?;

        // The identity stays on the session so a Space can take it back without minting a new one;
        // only the Space claim is cleared.
        if let Some(backend_session_id) = backend_session_id
            && let Some(binding) = self.binding_mut(scope)
        {
            let config = binding.multiplexer.clone();
            binding.mux.execute_command(
                repaint,
                &config,
                MuxCommand::StampSession {
                    session_id: backend_session_id,
                    tag: MuxSessionTag {
                        identity: Some(identity),
                        space: None,
                    },
                },
            );
        }
        Ok(true)
    }

    /// Prepare what Bootty is about to ask the active binding's backend for.
    /// # Errors
    /// Returns invalid mutation errors.
    pub fn begin_active_binding_membership_mutation(
        &mut self,
        command: &MuxCommand,
        naming: Option<&PendingGeneratedName>,
    ) -> Result<Option<BindingMembershipMutation>, WorkspacePersistenceError> {
        self.begin_binding_membership_mutation(self.active.binding.scope, command, naming)
    }

    /// Prepare what Bootty is about to ask the backend of the binding at `scope` for, keyed on
    /// the session's identity, so an ambiguous answer is recoverable without guessing from names.
    /// The binding need not be active. Without `naming`, a name counts as the caller's choice.
    /// # Errors
    /// Returns missing binding or invalid mutation errors.
    pub fn begin_binding_membership_mutation(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        naming: Option<&PendingGeneratedName>,
    ) -> Result<Option<BindingMembershipMutation>, WorkspacePersistenceError> {
        let binding = self.binding(scope).ok_or_else(|| {
            WorkspacePersistenceError::operation("the Space's binding is no longer live")
        })?;
        {}
        let display_name = |fallback: &str| {
            naming.map_or_else(|| fallback.to_owned(), |naming| naming.display_name.clone())
        };
        let mutation = match command {
            MuxCommand::CreateProjectSession {
                session_id,
                cwd,
                tag,
                ..
            } => tag
                .identity
                .clone()
                .map(|identity| BindingMembershipMutation::Create {
                    identity,
                    session_name: session_id.clone(),
                    display_name: display_name(session_id),
                    explicit: naming.is_none_or(|naming| naming.explicit),
                    cwd: cwd.clone(),
                    initial_state: naming
                        .map_or_else(crate::session_membership::SessionState::default, |naming| {
                            naming.initial_state
                        }),
                }),
            MuxCommand::RestoreSession {
                session_id, tag, ..
            } => tag
                .identity
                .as_ref()
                .and_then(|identity| binding.sessions.get(identity))
                .map(|saved| BindingMembershipMutation::Create {
                    identity: saved.identity.clone(),
                    session_name: session_id.clone(),
                    display_name: saved.display_name.clone(),
                    explicit: saved.explicit,
                    cwd: saved.cwd.clone(),
                    initial_state: saved.state,
                }),
            MuxCommand::RenameSession { session_id, name } => {
                let identity = self.session_identity(scope, session_id).ok_or_else(|| {
                    WorkspacePersistenceError::operation(format!(
                        "rename session {session_id}: this Space does not hold it"
                    ))
                })?;
                Some(BindingMembershipMutation::Rename {
                    identity,
                    new_name: name.clone(),
                    display_name: display_name(name),
                    explicit: naming.is_none_or(|naming| naming.explicit),
                })
            }
            _ => None,
        };
        Ok(mutation)
    }

    /// # Errors
    /// Returns membership persistence errors.
    pub fn complete_binding_membership_command(
        &mut self,
        scope: SpaceId,
        membership: Option<&BindingMembershipMutation>,
        result: &MuxCommandResult,
    ) -> Result<(), WorkspacePersistenceError> {
        let Some(membership) = membership else {
            return Ok(());
        };
        let committable = result.as_ref().is_ok_and(|completion| {
            self.binding(scope)
                .is_some_and(|binding| completion.matches_config(&binding.multiplexer))
        });
        if !committable && !matches!(result, Err(MuxCommandError::LaunchFailed(_))) {
            return Ok(());
        }
        let Some(mut candidate) = self.binding_state_candidate(scope) else {
            return Ok(());
        };
        crate::repository::apply_binding_membership_mutation(membership, &mut candidate.sessions)?;
        if let Some(binding) = self.binding_mut(candidate.scope) {
            binding.publish_session_state(candidate);
        }
        Ok(())
    }

    pub fn complete_authoritative_command(
        &mut self,
        scope: SpaceId,
        command: &MuxCommand,
        result: MuxCommandResult,
    ) -> (MuxCommandResult, Option<String>) {
        let completion = {
            let Some(binding) = self.binding_mut(scope) else {
                return (Err(MuxCommandError::Stale), None);
            };
            let config = binding.multiplexer.clone();
            binding.mux.complete_authoritative_command(result, &config)
        };
        // Before the sync below, which would otherwise start a shown pane with a shell.
        let completion = completion.and_then(|completion| {
            self.restore_session_command(scope, command)?;
            self.start_session_command(scope, command)
                .map(|()| completion)
        });
        let sync_error = if completion.is_ok() && self.active.binding.scope == scope {
            self.sync_active_terminal_panes()
                .err()
                .map(|error| error.to_string())
        } else {
            None
        };
        (completion, sync_error)
    }

    /// # Errors
    /// Returns membership persistence errors.
    pub fn reconcile_binding_states(&mut self) -> Result<(), WorkspacePersistenceError> {
        let mut candidates = Vec::new();
        for binding in self.all_bindings() {
            let mut candidate = BindingStateCandidate {
                scope: binding.scope,
                sessions: binding.sessions.clone(),
            };
            binding.reconcile_session_state(&mut candidate);
            candidates.push(candidate);
        }
        self.commit_binding_state_candidates(candidates)?;
        Ok(())
    }

    pub(super) fn commit_binding_state_candidate(
        &mut self,
        candidate: BindingStateCandidate,
    ) -> Result<&mut BindingRuntime, WorkspacePersistenceError> {
        let binding = std::iter::once(&mut self.active.binding)
            .chain(
                self.inactive_spaces
                    .iter_mut()
                    .map(|space| &mut space.binding),
            )
            .find(|binding| binding.scope == candidate.scope)
            .ok_or_else(|| {
                WorkspacePersistenceError::operation("candidate binding is no longer live")
            })?;
        if binding.sessions != candidate.sessions {
            self.repository
                .commit_binding_state(candidate.scope, &candidate.sessions)?;
        }
        binding.publish_session_state(candidate);
        Ok(binding)
    }

    fn commit_binding_state_candidates(
        &mut self,
        candidates: Vec<BindingStateCandidate>,
    ) -> Result<(), WorkspacePersistenceError> {
        let mut bindings = std::iter::once(&mut self.active.binding)
            .chain(
                self.inactive_spaces
                    .iter_mut()
                    .map(|space| &mut space.binding),
            )
            .map(|binding| (binding.scope, binding))
            .collect::<HashMap<_, _>>();
        let updates = candidates
            .into_iter()
            .map(|candidate| {
                bindings
                    .remove(&candidate.scope)
                    .map(|binding| (binding, candidate))
                    .ok_or_else(|| {
                        WorkspacePersistenceError::operation(
                            "candidate binding is absent or duplicated",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let changed = updates
            .iter()
            .filter(|(binding, candidate)| binding.sessions != candidate.sessions)
            .map(|(_, candidate)| (candidate.scope, candidate.sessions.clone()))
            .collect::<Vec<_>>();
        self.repository.commit_binding_states(&changed)?;
        for (binding, candidate) in updates {
            binding.publish_session_state(candidate);
        }
        Ok(())
    }

    pub fn all_bindings(&self) -> impl Iterator<Item = &BindingRuntime> {
        self.spaces().flat_map(SpaceRuntime::bindings)
    }

    fn bindings_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        self.spaces_mut().flat_map(SpaceRuntime::bindings_mut)
    }

    pub fn binding_mut(&mut self, scope: SpaceId) -> Option<&mut BindingRuntime> {
        self.bindings_mut().find(|binding| binding.scope == scope)
    }

    pub fn binding(&self, scope: SpaceId) -> Option<&BindingRuntime> {
        self.all_bindings().find(|binding| binding.scope == scope)
    }
}
