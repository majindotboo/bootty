#![cfg(test)]

use bootty_config::config::BoottyConfig;
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use assert_fs::prelude::*;
use bootty_config::config::{MultiplexerBackendConfig, SshProfileConfig, load_config_from_path};
use bootty_control::{
    AppCommandRequest, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_mux::repository::{
    BindingMembershipMutation, RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride,
    WorkspaceRepository, WorkspaceSpace,
};
use bootty_mux::workspace::ScopedSessionTarget;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::{MuxBackend, PaneCapture, PaneInput, PaneText},
    capability::{BindingCapabilityDescriptor, BindingOperation},
    command::MuxCommand,
    controller::{CommandSelection, SpaceId},
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, MuxWindow},
    terminal::{
        BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget,
        TerminalRuntime,
    },
};
use bootty_ui::{
    AppState, ModalDialog,
    presentation::dialogs::{DitchAction, DitchSessionEvent, NewSessionPickerEvent},
};
use rusqlite::Connection;

#[path = "support/idle_frames.rs"]
mod frames;
mod support;
#[path = "support/config.rs"]
mod test_config;

fn create_space(
    repository: &mut WorkspaceRepository,
    name: &str,
    sort_key: &str,
    color: [u8; 3],
    mux: SpaceMuxOverride,
) -> WorkspaceSpace {
    repository
        .create_space(name, sort_key, color, false, mux, false)
        .expect("create Space")
        .expect("valid Space")
}

type PaneInputs = Arc<Mutex<Vec<(String, PaneInput)>>>;

struct RestoreBackend {
    sessions: Arc<Mutex<Vec<MuxSession>>>,
    create_calls: Arc<AtomicUsize>,
    release: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    create_release: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    fail_commands: bool,
    created_panes: bool,
    pane_inputs: PaneInputs,
}

impl MuxBackend for RestoreBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let sessions = self
            .sessions
            .lock()
            .expect("restore backend sessions lock")
            .clone();
        Ok(MuxSnapshot {
            active_session_id: sessions.first().map(|session| session.id.clone()),
            sessions,
            ..MuxSnapshot::default()
        })
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        if self.fail_commands {
            // Held until released, so the failure lands after the command was dispatched.
            if let Some(release) = &self.release {
                let _ = release.lock().expect("restore backend release lock").recv();
            }
            anyhow::bail!("the scripted backend refused the command");
        }
        match command {
            MuxCommand::CreateProjectSession {
                session_id,
                cwd,
                tag,
                ..
            } => {
                if let Some(release) = &self.create_release {
                    release
                        .lock()
                        .expect("create release lock")
                        .recv()
                        .expect("release create");
                }
                self.create_calls.fetch_add(1, Ordering::SeqCst);
                let mut sessions = self.sessions.lock().expect("restore backend sessions lock");
                let session = if self.created_panes {
                    let pane = format!("%{}", sessions.len().saturating_add(1));
                    MuxSession {
                        tag,
                        ..session_on_pane(&session_id, &pane, Some(cwd))
                    }
                } else {
                    mux_session(&session_id, cwd, tag, true)
                };
                sessions.push(session);
            }
            MuxCommand::DitchSession { session_id } => {
                if let Some(release) = &self.release {
                    release
                        .lock()
                        .expect("restore backend release lock")
                        .recv()
                        .expect("release delayed ditch");
                }
                self.sessions
                    .lock()
                    .expect("restore backend sessions lock")
                    .retain(|session| session.id != session_id);
            }
            MuxCommand::StampSession { session_id, tag } => {
                if let Some(session) = self
                    .sessions
                    .lock()
                    .expect("restore backend sessions lock")
                    .iter_mut()
                    .find(|session| session.id == session_id)
                {
                    session.tag = tag;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn send_pane_input(&self, pane_id: &str, input: &PaneInput) -> Result<()> {
        self.pane_inputs
            .lock()
            .expect("pane inputs lock")
            .push((pane_id.to_owned(), input.clone()));
        Ok(())
    }

    fn capture_pane(&self, pane_id: &str, _capture: PaneCapture) -> Result<PaneText> {
        Ok(PaneText {
            text: format!("{pane_id} read by its backend"),
            captured_lines: 1,
            omitted_lines: 0,
        })
    }
}

struct RestoreProvider {
    kind: MuxBackendKind,
    dispatch: MuxCommandDispatch,
    sessions: Arc<Mutex<Vec<MuxSession>>>,
    create_calls: Arc<AtomicUsize>,
    release: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    create_release: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    topology: PaneTopology,
    selection_publication: SelectionPublicationPolicy,
    stamp_sessions: bool,
    server_socket: Option<std::path::PathBuf>,
    fail_commands: bool,
    created_panes: bool,
    pane_inputs: PaneInputs,
}

fn restore_provider(
    kind: MuxBackendKind,
    sessions: Arc<Mutex<Vec<MuxSession>>>,
    create_calls: Arc<AtomicUsize>,
) -> RestoreProvider {
    RestoreProvider {
        kind,
        dispatch: MuxCommandDispatch::CallerThread,
        sessions,
        create_calls,
        release: None,
        create_release: None,
        topology: PaneTopology::Attach,
        selection_publication: SelectionPublicationPolicy::Direct,
        stamp_sessions: true,
        server_socket: None,
        fail_commands: false,
        created_panes: false,
        pane_inputs: PaneInputs::default(),
    }
}

fn app_state(
    config: bootty_config::config::BoottyConfig,
    backends: Arc<MuxBackendRegistry>,
) -> AppState {
    AppState::new(config, backends, Arc::new(|| {}), None, None).expect("app state")
}

fn lock_workspace(path: &Path) -> Connection {
    let lock = Connection::open(path).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");
    lock
}

fn registry<const N: usize>(
    providers: [Arc<RestoreProvider>; N],
    fallbacks: [MuxBackendKind; N],
) -> Arc<MuxBackendRegistry> {
    Arc::new(
        MuxBackendRegistry::from_app_providers(providers, fallbacks)
            .expect("test backend registry"),
    )
}

struct TestPanePolicy {
    fail_start: bool,
}

impl BackendPanePolicy for TestPanePolicy {
    fn remote_target(&self) -> Option<bootty_mux::RemoteTarget> {
        None
    }

    fn start_terminal(
        &mut self,
        _request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        if self.fail_start {
            anyhow::bail!("native pane publication failed")
        }
        Ok(None)
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, _request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }

    fn deactivate(&mut self) {}
}

impl MuxBackendProvider for RestoreProvider {
    fn kind(&self) -> MuxBackendKind {
        self.kind
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        self.dispatch
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(RestoreBackend {
            sessions: Arc::clone(&self.sessions),
            create_calls: Arc::clone(&self.create_calls),
            release: self.release.clone(),
            create_release: self.create_release.clone(),
            fail_commands: self.fail_commands,
            created_panes: self.created_panes,
            pane_inputs: Arc::clone(&self.pane_inputs),
        })
    }
}

impl MuxAppBackendProvider for RestoreProvider {
    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(TestPanePolicy {
            fail_start: self.topology == PaneTopology::ProcessLocal,
        })
    }

    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: self.topology,
                cache_terminals: false,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: if self.kind == MuxBackendKind::Herdr {
                PersistedSessionPolicy::Never
            } else {
                PersistedSessionPolicy::AfterEmptyInitialSnapshot
            },
            generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: self.selection_publication,
        }
    }

    fn local_server_socket(&self, _config: &MuxBindingConfig) -> Option<std::path::PathBuf> {
        self.server_socket.clone()
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        let mut operations = vec![
            BindingOperation::CreateProjectSession,
            BindingOperation::CreateWindow,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ];
        if self.stamp_sessions {
            operations.push(BindingOperation::StampSession);
        }
        BindingCapabilityDescriptor::new(scope, operations)
    }
}

fn backends_after_empty_restore() -> (Arc<MuxBackendRegistry>, Arc<AtomicUsize>) {
    let sessions = Arc::new(Mutex::new(Vec::new()));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let provider = || {
        restore_provider(
            MuxBackendKind::Tmux,
            Arc::clone(&sessions),
            Arc::clone(&create_calls),
        )
    };
    let registry = registry([Arc::new(provider())], [MuxBackendKind::Tmux]);
    (registry, create_calls)
}

#[fixture]
fn directory() -> assert_fs::TempDir {
    assert_fs::TempDir::new().expect("temporary workspace")
}

fn claimed_session(
    identity: &str,
    backend_name: &str,
    cwd: &str,
) -> bootty_mux::session_membership::WorkspaceSession {
    bootty_mux::session_membership::WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: backend_name.to_owned(),
        display_name: String::new(),
        explicit: false,
        cwd: cwd.to_owned(),
    }
}

fn claim_first_space(
    config_path: &Path,
    identity: &str,
    backend_name: &str,
    cwd: &str,
) -> (WorkspaceRepository, WorkspaceSpace) {
    let (mut repository, snapshot) = WorkspaceRepository::open(config_path).expect("workspace");
    let space = snapshot.spaces()[0].clone();
    let mut sessions = space.binding().sessions().clone();
    assert!(sessions.claim(claimed_session(identity, backend_name, cwd)));
    repository
        .commit_binding_state(space.binding().mux_scope(), &sessions)
        .expect("persist claimed session");
    (repository, space)
}

fn mux_session(id: &str, cwd: String, tag: MuxSessionTag, active: bool) -> MuxSession {
    MuxSession {
        anchor: MuxPaneAnchor {
            session_id: id.to_owned(),
            cwd: Some(cwd),
            ..MuxPaneAnchor::default()
        },
        id: id.to_owned(),
        name: id.to_owned(),
        active,
        active_window_id: None,
        tag,
        windows: Vec::new(),
    }
}

#[rstest]
fn moving_a_session_publishes_the_new_order_immediately(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Tmux);
    let cwd = directory.path().to_string_lossy().into_owned();
    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).expect("workspace");
    let space = &snapshot.spaces()[0];
    let mut claimed = space.binding().sessions().clone();
    for (identity, name) in [("first-id", "first"), ("second-id", "second")] {
        assert!(claimed.claim(claimed_session(identity, name, &cwd)));
    }
    repository
        .commit_binding_state(space.binding().mux_scope(), &claimed)
        .expect("claim sessions");
    let sessions = Arc::new(Mutex::new(
        [("first-id", "first"), ("second-id", "second")]
            .into_iter()
            .map(|(identity, name)| {
                mux_session(
                    name,
                    cwd.clone(),
                    MuxSessionTag {
                        identity: Some(identity.to_owned()),
                        space: Some(space.remote_id().to_owned()),
                    },
                    name == "first",
                )
            })
            .collect(),
    ));
    drop(repository);
    let backends = registry(
        [Arc::new(restore_provider(
            MuxBackendKind::Tmux,
            sessions,
            Arc::new(AtomicUsize::new(0)),
        ))],
        [MuxBackendKind::Tmux],
    );
    let repaints = Arc::new(AtomicUsize::new(0));
    let repaint = {
        let repaints = Arc::clone(&repaints);
        Arc::new(move || {
            repaints.fetch_add(1, Ordering::Relaxed);
        })
    };
    let mut state = AppState::new(config, backends, repaint, None, None).expect("app state");
    state.update_frame(frames::idle_frame(Instant::now()));
    let order = |state: &AppState| {
        state
            .mux()
            .sessions()
            .iter()
            .map(|session| session.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&state), ["first", "second"]);
    let before = repaints.load(Ordering::Relaxed);

    assert!(state.move_session_from_ui("second", -1));
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(order(&state), ["second", "first"]);
    assert!(repaints.load(Ordering::Relaxed) > before);

    assert!(state.move_session_from_ui("second", 1));
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(order(&state), ["first", "second"]);

    assert!(state.reorder_session_before("second", Some("first")));
    assert_eq!(order(&state), ["second", "first"]);

    state.activate_session_from_ui("second");
    for (delta, expected) in [
        (1, ["first", "second"]),
        (-1, ["second", "first"]),
        (1, ["first", "second"]),
    ] {
        let outcome = submit_command(
            &mut state,
            CommandInvocation::new("move_session", vec![delta.to_string()], Caller::Keybinding),
            Instant::now(),
        );
        assert!(matches!(outcome, CommandOutcome::Success { .. }));
        assert_eq!(order(&state), expected);
    }
}

fn session_with_pane(id: &str) -> MuxSession {
    session_on_pane(id, &format!("{id}-pane"), None)
}

fn session_on_pane(id: &str, pane_id: &str, cwd: Option<String>) -> MuxSession {
    let pane = MuxPaneAnchor {
        session_id: id.to_owned(),
        pane_id: Some(pane_id.to_owned()),
        cwd,
        ..MuxPaneAnchor::default()
    };
    MuxSession {
        id: id.to_owned(),
        name: id.to_owned(),
        active: id == "first",
        anchor: pane.clone(),
        active_window_id: Some(format!("{id}-window")),
        tag: MuxSessionTag::default(),
        windows: vec![MuxWindow {
            id: format!("{id}-window"),
            index: 0,
            name: "window".to_owned(),
            active: true,
            anchor: pane.clone(),
            panes: vec![pane],
            layout: None,
            progress: None,
        }],
    }
}

fn submit_command(
    state: &mut AppState,
    invocation: CommandInvocation,
    started: Instant,
) -> CommandOutcome {
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation,
            deadline: started
                .checked_add(Duration::from_secs(1))
                .expect("test timestamp fits"),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    (0..250)
        .find_map(|tick| {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(tick))
                    .expect("test timestamp fits"),
            ));
            // A native create answers once its pane's process has started.
            outcomes.recv_timeout(Duration::from_millis(5)).ok()
        })
        .expect("command completes")
}

/// A following create selects the new session; a preserving one keeps whatever is selected when
/// its result lands, including a switch the user made while it ran.
#[rstest]
fn detached_session_creation_preserves_the_requested_selection(
    #[values(MuxCommandDispatch::CallerThread, MuxCommandDispatch::WorkerThread)]
    dispatch: MuxCommandDispatch,
    #[values(CommandSelection::Follow, CommandSelection::Preserve)] selection: CommandSelection,
) {
    let sessions = Arc::new(Mutex::new(
        ["old", "other"]
            .map(|name| mux_session(name, String::new(), MuxSessionTag::default(), true))
            .to_vec(),
    ));
    let backends = registry(
        [Arc::new(RestoreProvider {
            dispatch,
            ..restore_provider(
                MuxBackendKind::Tmux,
                sessions,
                Arc::new(AtomicUsize::new(0)),
            )
        })],
        [MuxBackendKind::Tmux],
    );
    let mut mux =
        bootty_mux::controller::MuxController::new(SpaceId::from_persistence(1), backends, None);
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        ..MuxBindingConfig::default()
    };
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    mux.activate_session("old");
    let response = mux.execute_command_authoritatively(
        &repaint,
        &config,
        MuxCommand::CreateProjectSession {
            session_id: "new".to_owned(),
            cwd: String::new(),
            tag: MuxSessionTag::default(),
            argv: None,
        },
        selection,
        Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("command deadline"),
        CommandCancellation::new(),
    );
    mux.activate_session("other");
    let result = response
        .recv_timeout(Duration::from_secs(10))
        .expect("command completion");
    let completion = mux
        .complete_authoritative_command(result, &config)
        .expect("session created");
    let (requested, selected) = match selection {
        CommandSelection::Follow => (Some("new"), "new"),
        CommandSelection::Preserve => (None, "other"),
    };
    assert_eq!(completion.selected_session.as_deref(), requested);
    assert_eq!(mux.selected_session(), Some(selected));
}

#[rstest]
fn persist_before_publish_blocks_selection_when_restore_write_fails(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path, MultiplexerBackendConfig::Tmux);
    let sessions = Arc::new(Mutex::new(vec![
        session_with_pane("first"),
        session_with_pane("second"),
    ]));
    let backends = registry(
        [Arc::new(RestoreProvider {
            selection_publication: SelectionPublicationPolicy::PersistBeforePublish,
            ..restore_provider(
                MuxBackendKind::Tmux,
                sessions,
                Arc::new(AtomicUsize::new(0)),
            )
        })],
        [MuxBackendKind::Tmux],
    );
    let mut state = app_state(config, backends);
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(state.mux().selected_session(), Some("first"));

    let database = directory.path().join("session-order.sqlite3");
    let _lock = lock_workspace(&database);
    state.activate_session_from_ui("second");

    assert_eq!(state.mux().selected_session(), Some("first"));
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("save binding restore state"))
    );
}

#[rstest]
fn native_pane_publication_error_is_preserved_on_successful_command(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path, MultiplexerBackendConfig::Tmux);
    let backends = registry(
        [Arc::new(RestoreProvider {
            topology: PaneTopology::ProcessLocal,
            ..restore_provider(
                MuxBackendKind::Tmux,
                Arc::new(Mutex::new(vec![session_with_pane("first")])),
                Arc::new(AtomicUsize::new(0)),
            )
        })],
        [MuxBackendKind::Tmux],
    );
    let mut state = app_state(config, backends);
    state.update_frame(frames::idle_frame(Instant::now()));
    state.clear_last_error();

    let outcome = submit_command(
        &mut state,
        CommandInvocation::from_action("new_tab", Caller::Socket),
        Instant::now(),
    );

    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        state.last_error().as_deref(),
        Some("native pane publication failed")
    );
}

/// Handing a session to another Space is a change of claim, not of session: it keeps its identity,
/// its name, and the process it was running.
#[rstest]
fn a_session_moves_between_spaces_on_one_multiplexer_and_stays_put_across_a_restart(
    directory: assert_fs::TempDir,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Tmux);
    let cwd = directory.path().to_string_lossy().into_owned();

    let (mut repository, first_space) =
        claim_first_space(&config_path, "moving-id", "moving", &cwd);
    let first_scope = first_space.binding().mux_scope();
    let second_space = create_space(
        &mut repository,
        "Second",
        "2",
        [0x22, 0x44, 0x66],
        SpaceMuxOverride::default(),
    );
    let second_id = second_space.id();
    let second_scope = second_space.binding().mux_scope();
    // A Space on another multiplexer: a session cannot follow it there.
    let elsewhere = create_space(
        &mut repository,
        "Elsewhere",
        "3",
        [0x33, 0x55, 0x77],
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Rmux),
            remote: SpaceRemoteOverride::Local,
        },
    );
    let elsewhere_id = elsewhere.id();
    drop(repository);

    let sessions = Arc::new(Mutex::new(vec![mux_session(
        "moving",
        cwd,
        MuxSessionTag {
            identity: Some("moving-id".to_owned()),
            space: Some(first_space.remote_id().to_owned()),
        },
        true,
    )]));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let provider = |kind| {
        Arc::new(restore_provider(
            kind,
            Arc::clone(&sessions),
            Arc::clone(&create_calls),
        ))
    };
    let backends = registry(
        [
            provider(MuxBackendKind::Tmux),
            provider(MuxBackendKind::Rmux),
        ],
        [MuxBackendKind::Tmux, MuxBackendKind::Rmux],
    );

    let mut state = app_state(config, backends);
    state.update_frame(frames::idle_frame(Instant::now()));
    let target = ScopedSessionTarget::new(first_scope, "moving".to_owned());

    // Both Spaces run the same multiplexer, so the move is a change of tag.
    let targets = state.session_move_targets(&target);
    assert!(
        targets
            .iter()
            .any(|space| space.id == second_id && space.reachable)
    );
    assert!(
        targets
            .iter()
            .any(|space| space.id == first_scope && space.current)
    );
    // Listed, so the answer is "not from here" rather than a Space that seems not to exist.
    assert!(
        targets
            .iter()
            .any(|space| space.id == elsewhere_id && !space.reachable),
        "a Space on another multiplexer is offered and refused, not hidden"
    );
    assert!(!state.move_scoped_session_to_space(&target, elsewhere_id));

    assert!(state.move_scoped_session_to_space(&target, second_id));
    assert!(
        !state.move_scoped_session_to_space(&target, first_scope),
        "the session no longer belongs to the Space it came from"
    );

    drop(state);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let claims = |space_id| {
        reopened
            .spaces()
            .iter()
            .find(|space| space.id() == space_id)
            .expect("Space")
            .binding()
            .sessions()
            .backend_names()
    };
    assert_eq!(claims(first_scope), Vec::<String>::new());
    assert_eq!(claims(second_scope), vec!["moving"]);
    assert_eq!(
        sessions.lock().expect("sessions")[0].tag.space.as_deref(),
        Some(second_space.remote_id()),
        "the multiplexer carries the new claim, so every bootty window agrees"
    );
}

/// Letting go of a session must not make it disappear. Membership is explicit, so a session no
/// Space claims has to be somewhere the sidebar can show it.
#[rstest]
fn an_unassigned_session_keeps_running_and_stays_visible_as_unclaimed(
    directory: assert_fs::TempDir,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Tmux);
    let cwd = directory.path().to_string_lossy().into_owned();

    let (repository, space) = claim_first_space(&config_path, "kept-id", "kept", &cwd);
    let scope = space.binding().mux_scope();
    drop(repository);

    let sessions = Arc::new(Mutex::new(vec![mux_session(
        "kept",
        cwd,
        MuxSessionTag {
            identity: Some("kept-id".to_owned()),
            space: Some(space.remote_id().to_owned()),
        },
        true,
    )]));
    let backends = registry(
        [Arc::new(restore_provider(
            MuxBackendKind::Tmux,
            Arc::clone(&sessions),
            Arc::new(AtomicUsize::new(0)),
        ))],
        [MuxBackendKind::Tmux],
    );

    let mut state = app_state(config, backends);
    state.update_frame(frames::idle_frame(Instant::now()));
    let target = ScopedSessionTarget::new(scope, "kept".to_owned());
    assert_eq!(state.unclaimed_sessions(), []);

    assert!(state.detach_scoped_session_from_space(&target));
    state.update_frame(frames::idle_frame(Instant::now()));

    assert_eq!(
        sessions.lock().expect("sessions")[0].tag.space,
        None,
        "the session is running and claimed by nobody"
    );
    assert_eq!(
        sessions.lock().expect("sessions")[0]
            .tag
            .identity
            .as_deref(),
        Some("kept-id"),
        "its identity survives, so a Space can take it back without minting a new one"
    );
    let unclaimed = state.unclaimed_sessions();
    assert_eq!(
        unclaimed
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["kept"],
        "an unassigned session is visible, not gone"
    );

    // And taking it back is one click.
    assert!(state.adopt_and_activate_scoped_session(&target));
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(state.unclaimed_sessions(), []);
}

#[rstest]
fn backend_without_session_stamping_projects_direct_sessions_without_membership(
    directory: assert_fs::TempDir,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Herdr);
    let sessions = Arc::new(Mutex::new(vec![mux_session(
        "default",
        directory.path().to_string_lossy().into_owned(),
        MuxSessionTag::default(),
        true,
    )]));
    let backends = registry(
        [Arc::new(RestoreProvider {
            stamp_sessions: false,
            ..restore_provider(
                MuxBackendKind::Herdr,
                Arc::clone(&sessions),
                Arc::new(AtomicUsize::new(0)),
            )
        })],
        [MuxBackendKind::Herdr],
    );

    let mut state = app_state(config, backends);
    for tick in 0..3 {
        state.update_frame(frames::idle_frame(
            Instant::now()
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
    }
    assert_eq!(state.multiplexer_backend(), MultiplexerBackendConfig::Herdr);
    assert_eq!(state.last_error(), None);
    assert_eq!(
        state
            .mux()
            .all_sessions()
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["default"],
        "the direct backend snapshot reaches the mux controller"
    );
    assert_eq!(
        state
            .mux()
            .sessions()
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["default"],
        "workspace reconciliation keeps direct sessions visible and attachable"
    );

    let groups = state.binding_session_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["default"],
        "a direct binding projects the backend snapshot as its active sessions"
    );
    assert!(
        state.unclaimed_sessions().is_empty(),
        "direct sessions are never adoptable or rendered under Unassigned"
    );
    assert_eq!(
        state
            .session_finder_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["default"],
        "the session finder uses the same direct-binding projection"
    );
    let target = ScopedSessionTarget::new(state.mux_scope(), "default");
    assert!(
        !state.adopt_and_activate_scoped_session(&target),
        "a direct session cannot enter the tag-ownership adoption path"
    );
    assert_eq!(
        sessions.lock().expect("sessions")[0].tag,
        MuxSessionTag::default(),
        "no StampSession command was enqueued"
    );

    drop(state);
    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert!(
        snapshot.spaces()[0].binding().sessions().is_empty(),
        "direct sessions do not create durable tag-ownership records"
    );
}

#[rstest]
#[case(DitchAction::KillOnly)]
#[case(DitchAction::DetachWorktree)]
fn pending_ditch_completes_in_its_original_space(
    directory: assert_fs::TempDir,
    #[case] action: DitchAction,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Tmux);
    let cwd = directory.path().to_string_lossy().into_owned();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Arc::new(Mutex::new(release_rx));
    let first_sessions = Arc::new(Mutex::new(vec![mux_session(
        "delayed",
        cwd.clone(),
        MuxSessionTag::default(),
        true,
    )]));
    let second_sessions = Arc::new(Mutex::new(Vec::new()));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let backends = registry(
        [
            Arc::new(RestoreProvider {
                dispatch: MuxCommandDispatch::WorkerThread,
                release: Some(Arc::clone(&release)),
                ..restore_provider(
                    MuxBackendKind::Tmux,
                    Arc::clone(&first_sessions),
                    Arc::clone(&create_calls),
                )
            }),
            Arc::new(restore_provider(
                MuxBackendKind::Rmux,
                Arc::clone(&second_sessions),
                Arc::clone(&create_calls),
            )),
        ],
        [MuxBackendKind::Tmux, MuxBackendKind::Rmux],
    );

    let (mut repository, first_space) =
        claim_first_space(&config_path, "delayed-id", "delayed", &cwd);
    let first_scope = first_space.binding().mux_scope();
    // The session is already running and already carries its Space's tag, which is what the
    // workspace reads membership from.
    first_sessions.lock().expect("seed the delayed session tag")[0].tag = MuxSessionTag {
        identity: Some("delayed-id".to_owned()),
        space: Some(first_space.remote_id().to_owned()),
    };
    let second_space = create_space(
        &mut repository,
        "Second",
        "2",
        [0x22, 0x44, 0x66],
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Rmux),
            remote: SpaceRemoteOverride::Local,
        },
    );
    let second_id = second_space.id();
    let second_scope = second_space.binding().mux_scope();
    drop(repository);

    let mut state = app_state(config, backends);
    assert!((0..250).any(|tick| {
        state.update_frame(frames::idle_frame(
            Instant::now()
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
        std::thread::sleep(Duration::from_millis(1));
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| group.sessions.iter())
            .any(|session| session.id == "delayed")
    }));
    assert!(state.open_ditch_session_dialog_for("delayed"));
    assert!(matches!(
        state.modal_dialog(),
        Some(ModalDialog::DitchSession(_))
    ));
    state.clear_last_error();
    state.apply_ditch_session_event(DitchSessionEvent::Ditch {
        session_id: "delayed".to_owned(),
        cwd: None,
        action,
    });
    assert!(state.activate_space_from_ui(second_id));
    assert_eq!(state.binding_session_groups()[0].sessions, []);
    release_tx.send(()).expect("release delayed ditch");

    for tick in 0..250 {
        state.update_frame(frames::idle_frame(
            Instant::now()
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(state.last_error().is_none(), "{:#?}", state.last_error());
    assert!(state.activate_space_from_ui(first_scope));
    let removed = (0..250).any(|tick| {
        state.update_frame(frames::idle_frame(
            Instant::now()
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
        std::thread::sleep(Duration::from_millis(1));
        state.binding_session_groups()[0].sessions.is_empty()
    });
    assert!(removed, "original Space must publish the ditch completion");

    drop(state);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let first = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == first_scope)
        .expect("first Space");
    let second = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == second_scope)
        .expect("second Space");
    assert!(first.binding().sessions().is_empty());
    assert!(second.binding().sessions().is_empty());
}

#[rstest]
fn a_failed_placement_commit_preserves_the_live_and_durable_binding(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Native);
    let mut state = app_state(config, support::backends());
    let space = state.space_summaries()[0].clone();
    let database = directory.path().join("session-order.sqlite3");
    let lock = lock_workspace(&database);

    assert!(!state.update_space_from_ui(
        space.id,
        &space.name,
        &space.icon,
        space.color,
        space.tint_sidebar,
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Tmux),
            remote: SpaceRemoteOverride::Local,
        },
    ));
    assert_eq!(
        state.multiplexer_backend(),
        MultiplexerBackendConfig::Native
    );

    lock.execute_batch("ROLLBACK").expect("release write lock");
    drop(lock);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    let binding = &reopened.spaces()[0].binding();
    assert_eq!(binding.backend_override(), None);
    assert_eq!(binding.remote_override(), &SpaceRemoteOverride::Inherit);
}

#[rstest]
fn a_failed_session_membership_commit_preserves_the_live_runtime_and_database(
    directory: assert_fs::TempDir,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Native);
    let mut state = app_state(config, support::backends());
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            invocation: CommandInvocation::from_action("new_tab", Caller::Socket),
            // The budget bounds a genuine hang. It stays far above the scheduler jitter that a
            // fully parallel test run adds to a pane spawn.
            deadline: Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test timestamp fits"),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");

    let started = Instant::now();
    let outcome = (0..250)
        .find_map(|tick| {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(tick))
                    .expect("test timestamp fits"),
            ));
            std::thread::sleep(Duration::from_millis(1));
            outcomes.try_recv().ok()
        })
        .expect("create session command completes");
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "unexpected command outcome: {outcome:?}"
    );
    let target = (0..250)
        .find_map(|tick| {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(
                        250_u64.checked_add(tick).expect("test tick fits"),
                    ))
                    .expect("test timestamp fits"),
            ));
            std::thread::sleep(Duration::from_millis(1));
            state
                .binding_session_groups()
                .into_iter()
                .find_map(|group| group.sessions.first().map(|session| group.target(session)))
        })
        .expect("native session becomes available");
    let original_name = state
        .binding_session_groups()
        .iter()
        .flat_map(|group| group.sessions.iter())
        .find(|session| session.id == target.session_id)
        .expect("live session")
        .name
        .clone();

    let database = directory.path().join("session-order.sqlite3");
    let lock = lock_workspace(&database);

    assert!(!state.detach_scoped_session_from_space(&target));
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| group.sessions.iter())
            .any(|session| session.id == target.session_id)
    );

    lock.execute_batch("ROLLBACK").expect("release write lock");
    drop(lock);
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert!(
        reopened.spaces()[0]
            .binding()
            .sessions()
            .backend_names()
            .contains(&original_name)
    );
}

#[rstest]
fn an_inactive_placement_update_rebuilds_before_activation(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Native);
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let second_space = create_space(
        &mut repository,
        "Second",
        "2",
        [0x22, 0x44, 0x66],
        SpaceMuxOverride::default(),
    );
    let second_id = second_space.id();
    drop(repository);

    let mut state = app_state(config, support::backends());
    let second = state
        .space_summaries()
        .into_iter()
        .find(|space| space.id == second_id)
        .expect("inactive second Space");
    assert!(state.update_space_from_ui(
        second.id,
        &second.name,
        &second.icon,
        second.color,
        second.tint_sidebar,
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Tmux),
            remote: SpaceRemoteOverride::Local,
        },
    ));
    assert_eq!(
        state.multiplexer_backend(),
        MultiplexerBackendConfig::Native
    );
    assert!(state.activate_space_from_ui(second_id));
    assert_eq!(state.multiplexer_backend(), MultiplexerBackendConfig::Tmux);
}

#[rstest]
#[case("files.open")]
#[case("files.browse")]
#[case("git.open")]
fn captured_host_commands_keep_their_binding_after_switching_spaces(
    directory: assert_fs::TempDir,
    #[case] command: &str,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Native);
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let second = create_space(
        &mut repository,
        "Second",
        "2",
        [1, 2, 3],
        SpaceMuxOverride::default(),
    );
    let mut state = app_state(config, support::backends());
    let first = state.active_space_id();
    let started = Instant::now();
    let CommandOutcome::Success { value, .. } = submit_command(
        &mut state,
        CommandInvocation::new(
            "resource.current",
            vec!["binding".to_owned()],
            Caller::Socket,
        ),
        started,
    ) else {
        panic!("current binding target");
    };
    let target: bootty_control::CommandTarget =
        serde_json::from_value(value["target"].clone()).unwrap();
    assert!(state.activate_space_from_ui(second.id()));

    let mut invocation = CommandInvocation::new(
        command,
        vec![directory.path().to_string_lossy().into_owned()],
        Caller::Socket,
    );
    invocation.target = Some(target.clone());
    let response = state
        .app_command_sender(Caller::Socket)
        .submit(
            invocation.clone(),
            started
                .checked_add(Duration::from_secs(1))
                .expect("test deadline fits"),
            CommandCancellation::new(),
        )
        .expect("submit captured command");
    let effects = state.update_frame(frames::idle_frame(started));
    assert!(matches!(
        response.try_recv().unwrap(),
        CommandOutcome::Success { .. }
    ));
    let opened = effects.iter().find_map(|effect| match effect {
        bootty_ui::AppEffect::OpenFiles(request) => Some((request.scope, &request.target)),
        bootty_ui::AppEffect::OpenGitChanges { scope, target, .. } => Some((*scope, target)),
        _ => None,
    });
    assert_eq!(opened, Some((first, &target)));
    assert_eq!(state.active_space_id(), second.id());

    invocation.target.as_mut().unwrap().generation = target
        .generation
        .checked_add(1)
        .expect("test generation fits");
    assert!(matches!(
        submit_command(&mut state, invocation, started),
        CommandOutcome::StaleTarget { .. }
    ));
    let mut active_only = CommandInvocation::from_action("edit_space", Caller::Socket);
    active_only.target = Some(target);
    assert!(matches!(
        submit_command(&mut state, active_only, started),
        CommandOutcome::StaleTarget { .. }
    ));
    assert_eq!(state.active_space_id(), second.id());
}

#[rstest]
fn deleting_an_inactive_space_removes_live_and_durable_state(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        ..BoottyConfig::default()
    };
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let second_space = create_space(
        &mut repository,
        "Second",
        "2",
        [0x22, 0x44, 0x66],
        SpaceMuxOverride::default(),
    );
    let second_id = second_space.id();
    drop(repository);

    let mut state = app_state(config, support::backends());
    assert_eq!(state.space_summaries().len(), 2);
    assert!(state.close_space_from_ui(second_id));
    assert!(
        state
            .space_summaries()
            .into_iter()
            .all(|space| space.id != second_id)
    );

    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert_eq!(snapshot.spaces().len(), 1);
    assert!(
        snapshot
            .spaces()
            .iter()
            .all(|space| space.id() != second_id)
    );
}

#[rstest]
fn one_frame_recovers_active_and_inactive_binding_membership(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Tmux);
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let first_scope = snapshot.spaces()[0].binding().mux_scope();
    let first_binding = snapshot.spaces()[0].binding().clone();
    let mut sessions = first_binding.sessions().clone();
    assert!(sessions.claim(claimed_session(
        "persisted-first-id",
        "persisted-first",
        directory.path().to_str().expect("workspace path"),
    )));
    repository
        .commit_binding_state(first_scope, &sessions)
        .expect("persist first binding state");
    let second_space = create_space(
        &mut repository,
        "Second",
        "2",
        [0x22, 0x44, 0x66],
        SpaceMuxOverride::default(),
    );
    let second_scope = second_space.binding().mux_scope();
    for (scope, name) in [
        (first_scope, "interrupted-first"),
        (second_scope, "interrupted-second"),
    ] {
        repository
            .begin_binding_membership_mutation(
                scope,
                &BindingMembershipMutation::Create {
                    identity: format!("{name}-id"),
                    session_name: name.to_owned(),
                    display_name: name.to_owned(),
                    explicit: true,
                    cwd: String::new(),
                },
            )
            .expect("journal interrupted membership operation");
    }

    let (backends, create_calls) = backends_after_empty_restore();
    let mut state = app_state(config, backends);
    assert_eq!(create_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .all(|session| session.name != "persisted-first")
    );
    let spaces = state.space_summaries();
    let first_space = spaces
        .iter()
        .find(|space| space.id == first_scope)
        .expect("first Space")
        .clone();
    let second_space = spaces
        .iter()
        .find(|space| space.id == second_scope)
        .expect("second Space")
        .clone();
    let local_override = SpaceMuxOverride {
        backend: None,
        remote: SpaceRemoteOverride::Local,
    };
    state.update_frame(frames::idle_frame(Instant::now()));
    assert!(
        repository
            .pending_binding_membership_mutations(first_scope)
            .expect("read first pending operation")
            .is_empty(),
        "one active frame must resolve the first journal"
    );
    assert!(
        repository
            .pending_binding_membership_mutations(second_scope)
            .expect("read second pending operation")
            .is_empty(),
        "one frame must resolve the inactive binding journal"
    );
    let active_groups = state.binding_session_groups();
    assert_eq!(active_groups.len(), 1);
    assert_eq!(active_groups[0].scope, first_scope);
    assert!(active_groups[0].active);
    assert_eq!(create_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    // A backend change is never refused on account of bootty's own journal.
    assert!(state.update_space_from_ui(
        first_space.id,
        &first_space.name,
        &first_space.icon,
        first_space.color,
        first_space.tint_sidebar,
        local_override.clone(),
    ));
    assert!(state.update_space_from_ui(
        second_space.id,
        &second_space.name,
        &second_space.icon,
        second_space.color,
        second_space.tint_sidebar,
        local_override,
    ));

    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    assert!(
        reopened
            .spaces()
            .iter()
            .all(|space| { space.binding().remote_override() == &SpaceRemoteOverride::Local })
    );
}

#[rstest]
fn a_deferred_profile_rebuild_preserves_the_intended_display_name(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let mut config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Native);
    config.ssh_profiles.insert(
        "test".to_owned(),
        SshProfileConfig {
            name: "Initial".to_owned(),
            host: "localhost".to_owned(),
            user: None,
            port: None,
            authentication: bootty_config::config::SshAuthenticationConfig::default(),
            host_key_policy: bootty_config::config::SshHostKeyPolicyConfig::default(),
            identity_file: None,
            proxy_jump: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        },
    );
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let space = &snapshot.spaces()[0];
    let scope = space.binding().mux_scope();
    repository
        .update_space(
            scope,
            space.name(),
            space.icon(),
            space.color(),
            space.tint_sidebar(),
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Native),
                remote: SpaceRemoteOverride::Profile(RemoteSpaceRef {
                    profile_id: "test".to_owned(),
                    remote_space_id: "test-space".to_owned(),
                    remote_space_name: "Test Space".to_owned(),
                    backend: MultiplexerBackendConfig::Native,
                }),
            },
        )
        .expect("configure profile binding");

    let first_project = directory.child("first/project");
    let second_project = directory.child("second/project");
    first_project
        .create_dir_all()
        .expect("first project directory");
    second_project
        .create_dir_all()
        .expect("second project directory");
    let first_cwd = first_project.path();
    let second_cwd = second_project.path();
    let (create_release, create_wait) = mpsc::channel();
    let provider = RestoreProvider {
        dispatch: MuxCommandDispatch::WorkerThread,
        create_release: Some(Arc::new(Mutex::new(create_wait))),
        ..restore_provider(MuxBackendKind::Native, Arc::default(), Arc::default())
    };
    let mut state = app_state(
        config,
        registry([Arc::new(provider)], [MuxBackendKind::Native]),
    );
    create_release.send(()).expect("allow initial create");
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: first_cwd.to_string_lossy().into_owned(),
        command: None,
    });
    let started = Instant::now();
    assert!((0..250).any(|tick| {
        state.update_frame(frames::idle_frame(
            started
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
        std::thread::sleep(Duration::from_millis(1));
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .any(|session| session.name == "project")
    }));

    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: second_cwd.to_string_lossy().into_owned(),
        command: None,
    });
    assert_fs::fixture::ChildPath::new(config_path.clone())
        .write_str(
            "[ssh-profiles.test]\nname = \"Changed\"\nhost = \"localhost\"\nprogram = \"ssh\"\n",
        )
        .expect("write changed profile");
    assert!(state.reload_config(&mut Vec::new()));
    assert!(
        !repository
            .pending_binding_membership_mutations(scope)
            .expect("read pending operation")
            .is_empty(),
        "profile reload must defer while the membership command is pending"
    );
    create_release.send(()).expect("complete pending create");

    assert!((0..250).any(|tick| {
        state.update_frame(frames::idle_frame(
            started
                .checked_add(Duration::from_millis(
                    250_u64.checked_add(tick).expect("test tick fits"),
                ))
                .expect("test timestamp fits"),
        ));
        std::thread::sleep(Duration::from_millis(1));
        repository
            .pending_binding_membership_mutations(scope)
            .expect("read pending operation")
            .is_empty()
    }));
    let (_, reopened) = WorkspaceRepository::open(&config_path).expect("reopen workspace");
    // The server needed a suffix to tell the two apart; bootty shows the name it asked for.
    let sessions = reopened.spaces()[0].binding().sessions();
    let claimed = sessions
        .sessions()
        .iter()
        .find(|session| session.backend_name == "project-2")
        .expect("the uniquified session");
    assert_eq!(claimed.label(), "project");
}

#[rstest]
fn a_corrected_ssh_profile_rebuilds_an_unavailable_binding(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        ..BoottyConfig::default()
    };
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let space = &snapshot.spaces()[0];
    let scope = space.binding().mux_scope();
    repository
        .update_space(
            scope,
            space.name(),
            space.icon(),
            space.color(),
            space.tint_sidebar(),
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Native),
                remote: SpaceRemoteOverride::Profile(RemoteSpaceRef {
                    profile_id: "development".to_owned(),
                    remote_space_id: "remote-space".to_owned(),
                    remote_space_name: "Remote Space".to_owned(),
                    backend: MultiplexerBackendConfig::Native,
                }),
            },
        )
        .expect("configure missing profile binding");
    let binding = space.binding().clone();
    let mut sessions = binding.sessions().clone();
    assert!(sessions.claim(claimed_session(
        "fallback-id",
        "persisted-local-fallback",
        directory.path().to_str().expect("workspace path"),
    )));
    repository
        .commit_binding_state(scope, &sessions)
        .expect("persist unavailable binding restore state");

    let mut state = app_state(config, support::backends());
    assert_eq!(
        state.space_summaries()[0].error.as_deref(),
        Some("SSH profile 'development' is unavailable")
    );
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .all(|session| session.name != "persisted-local-fallback")
    );
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(
        state.space_summaries()[0].error.as_deref(),
        Some("SSH profile 'development' is unavailable")
    );
    assert!(
        state
            .binding_session_groups()
            .iter()
            .flat_map(|group| &group.sessions)
            .all(|session| session.name != "persisted-local-fallback")
    );

    assert_fs::fixture::ChildPath::new(config_path)
        .write_str(
            r#"
[ssh-profiles.development]
name = "Development"
host = "devbox"
user = "dev"
port = 2222
program = "ssh-wrapper"
args = ["-i", "key"]
"#,
        )
        .expect("write corrected profile");

    assert!(state.reload_config(&mut Vec::new()));
    assert_eq!(state.space_summaries()[0].error, None);
}

/// A binding recorded unavailable when the app last closed must be able to come back. Marking it
/// with a *configured* error stopped it refreshing at all, so it could never succeed and never
/// clear the flag — and because reconciliation is what clears the membership journal, that also
/// left every later membership change failing on the journal's unique scope.
#[rstest]
fn a_binding_persisted_as_unavailable_recovers_on_a_successful_refresh(
    directory: assert_fs::TempDir,
) {
    let config_path = directory.path().join("config.toml");
    let config = BoottyConfig {
        config_path: config_path.clone(),
        ..BoottyConfig::default()
    };
    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("workspace repository");
    let scope = snapshot.spaces()[0].binding().mux_scope();
    repository
        .set_binding_restore_state(scope, true, None, None)
        .expect("persist the binding as unavailable");

    let mut state = app_state(config, support::backends());
    assert_eq!(
        state.space_summaries()[0].error.as_deref(),
        Some("binding unavailable; reconnect to restore it"),
        "the last session's failure is still reported"
    );

    let started = Instant::now();
    assert!(
        (0..250).any(|tick| {
            state.update_frame(frames::idle_frame(
                started
                    .checked_add(Duration::from_millis(
                        250_u64.checked_add(tick).expect("test tick fits"),
                    ))
                    .expect("test timestamp fits"),
            ));
            std::thread::sleep(Duration::from_millis(1));
            state.space_summaries()[0].error.is_none()
        }),
        "a refresh that works clears the flag: {:?}",
        state.space_summaries()[0].error
    );
}

/// A steady-state frame forks nothing.
///
/// Resolving a session's directory means asking `git` for its worktree root, which forks and blocks
/// the frame thread for as long as the child takes. The reconciler asks for every session's
/// directory on every frame, so without a memo that is one fork per session per frame and typing
/// visibly lags. `guard_frame_path` panics naming whatever spawns, so this fails at the offender
/// rather than as a slow frame nobody measures.
#[rstest]
fn steady_state_frames_do_not_fork_a_subprocess(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Tmux);
    let cwd = directory.path().to_string_lossy().into_owned();
    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("workspace repository");
    let space_tag = snapshot.spaces()[0].remote_id().to_owned();
    // Two sessions in one directory and one in another: the memo has to answer for a directory it
    // has already resolved, whoever asks for it.
    let sessions = Arc::new(Mutex::new(
        [
            ("work", cwd.clone()),
            ("review", cwd.clone()),
            ("docs", cwd),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (name, cwd))| {
            mux_session(
                name,
                cwd,
                MuxSessionTag {
                    identity: Some(format!("{name}-id")),
                    space: Some(space_tag.clone()),
                },
                index == 0,
            )
        })
        .collect::<Vec<_>>(),
    ));
    let backends = registry(
        [Arc::new(restore_provider(
            MuxBackendKind::Tmux,
            Arc::clone(&sessions),
            Arc::new(AtomicUsize::new(0)),
        ))],
        [MuxBackendKind::Tmux],
    );

    let mut state = app_state(config, backends);
    // Settle first: claiming these sessions resolves each directory once, which is allowed to fork.
    for tick in 0..40 {
        state.update_frame(frames::idle_frame(
            Instant::now()
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
    }
    assert_eq!(
        state.binding_session_groups()[0].sessions.len(),
        3,
        "the Space claims the sessions its tag names"
    );

    let _guard = bootty_terminal::perf::guard_frame_path();
    for tick in 40..80 {
        state.update_frame(frames::idle_frame(
            Instant::now()
                .checked_add(Duration::from_millis(tick))
                .expect("test timestamp fits"),
        ));
    }
}

#[rstest]
fn manual_reconnect_targets_only_a_remote_binding(directory: assert_fs::TempDir) {
    let local_path = directory.child("local.toml");
    local_path
        .write_str("[multiplexer]\nbackend = \"native\"\n")
        .expect("write local config");
    let local = load_config_from_path(local_path.path()).expect("load local config");
    let mut local_state = app_state(local, support::backends());
    assert!(!local_state.reconnect_space_from_ui(local_state.active_space_id()));

    let remote_path = directory.child("remote.toml");
    remote_path
        .write_str(
            r#"
[multiplexer]
backend = "tmux"

[multiplexer.remote]
host = "reconnect.test"
program = "/bootty/missing-ssh"
"#,
        )
        .expect("write remote config");
    let remote = load_config_from_path(remote_path.path()).expect("load remote config");
    let mut remote_state = app_state(remote, support::backends());
    let space_id = remote_state.active_space_id();

    assert!(remote_state.reconnect_space_from_ui(space_id));
    let summary = remote_state
        .space_summaries()
        .into_iter()
        .find(|space| space.id == space_id)
        .expect("active Space summary");
    assert_eq!(
        summary.error.as_deref(),
        Some("reconnecting to reconnect.test")
    );
}

#[rstest]
#[case(MultiplexerBackendConfig::Native)]
#[case(MultiplexerBackendConfig::Rmux)]
#[case(MultiplexerBackendConfig::Tmux)]
fn failed_active_space_delete_preserves_selection(
    directory: assert_fs::TempDir,
    #[case] backend: MultiplexerBackendConfig,
    #[values(true, false)] reject_delete: bool,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), backend);
    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).unwrap();
    let first = snapshot.spaces()[0].id();
    create_space(
        &mut repository,
        "Second",
        "2",
        [0x22, 0x44, 0x66],
        SpaceMuxOverride::default(),
    );
    repository.set_selected_space("close-test", first).unwrap();
    let mut state = AppState::new_for_window(
        config,
        "close-test".to_owned(),
        support::backends(),
        Arc::new(|| {}),
        None,
        None,
    )
    .unwrap();
    let database = Connection::open(directory.path().join("session-order.sqlite3")).unwrap();
    let trigger = if reject_delete {
        "CREATE TRIGGER reject_space_delete BEFORE DELETE ON workspace_spaces
         BEGIN SELECT RAISE(ABORT, 'Space close rejected'); END;"
    } else {
        "CREATE TRIGGER reject_space_selection BEFORE INSERT ON workspace_window_state
         BEGIN SELECT RAISE(ABORT, 'Space close rejected'); END;"
    };
    database.execute_batch(trigger).unwrap();

    assert!(!state.close_space_from_ui(first));
    assert_eq!(state.active_space_id(), first);
    assert_eq!(state.space_summaries().len(), 2);
    assert!(state.last_error().unwrap().contains("Space close rejected"));
    let (_, reopened) = WorkspaceRepository::open(&config_path).unwrap();
    assert_eq!(reopened.selected_space("close-test"), Some(first));
    assert_eq!(reopened.spaces().len(), 2);

    let mut invocation = CommandInvocation::from_action("close_space", Caller::Socket);
    let outcome = submit_command(&mut state, invocation.clone(), Instant::now());
    let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
        panic!("expected close confirmation: {outcome:?}");
    };
    invocation.target.clone_from(&confirmation.target);
    invocation.confirmation = Some(*confirmation);
    let outcome = submit_command(&mut state, invocation, Instant::now());
    assert!(
        matches!(&outcome, CommandOutcome::Failed { message, .. } if message.contains("Space close rejected")),
        "report the actual persistence failure: {outcome:?}"
    );
    assert_eq!(state.active_space_id(), first);
}

#[rstest]
#[case::first(0, 1)]
#[case::middle(1, 2)]
#[case::last(2, 1)]
fn closing_active_space_selects_the_neighbor_and_preserves_the_last_space(
    directory: assert_fs::TempDir,
    #[case] closing: usize,
    #[case] neighbor: usize,
    #[values(
        MultiplexerBackendConfig::Native,
        MultiplexerBackendConfig::Rmux,
        MultiplexerBackendConfig::Tmux
    )]
    backend: MultiplexerBackendConfig,
) {
    let config_path = directory.path().join("config.toml");
    let config = test_config::config(config_path.clone(), backend);
    let (mut repository, snapshot) = WorkspaceRepository::open(&config_path).unwrap();
    let spaces = [
        snapshot.spaces()[0].id(),
        create_space(
            &mut repository,
            "Second",
            "2",
            [2; 3],
            SpaceMuxOverride::default(),
        )
        .id(),
        create_space(
            &mut repository,
            "Third",
            "3",
            [3; 3],
            SpaceMuxOverride::default(),
        )
        .id(),
    ];
    repository
        .set_selected_space("close-test", spaces[closing])
        .unwrap();
    let mut state = AppState::new_for_window(
        config,
        "close-test".to_owned(),
        support::backends(),
        Arc::new(|| {}),
        None,
        None,
    )
    .unwrap();

    assert!(state.close_space_from_ui(spaces[closing]));
    assert_eq!(state.active_space_id(), spaces[neighbor]);
    let (_, reopened) = WorkspaceRepository::open(&config_path).unwrap();
    assert_eq!(
        reopened.selected_space("close-test"),
        Some(spaces[neighbor])
    );
    assert_eq!(reopened.spaces().len(), 2);
    assert!(
        reopened
            .spaces()
            .iter()
            .all(|space| space.id() != spaces[closing])
    );

    assert!(state.close_space_from_ui(spaces[neighbor]));
    let last = state.active_space_id();
    assert!(!state.close_space_from_ui(last));
    assert_eq!(
        state.last_error(),
        Some("the last space cannot be closed".to_owned())
    );
    assert_eq!(state.active_space_id(), last);
    assert_eq!(state.space_summaries().len(), 1);
    let (_, reopened) = WorkspaceRepository::open(&config_path).unwrap();
    assert_eq!(reopened.selected_space("close-test"), Some(last));
    assert_eq!(reopened.spaces().len(), 1);
}

/// Either the real native provider or the scripted one, so one workspace can hold both.
enum MixedProvider {
    Native(bootty_mux::native::NativeProvider),
    Scripted(RestoreProvider),
}

impl MixedProvider {
    fn provider(&self) -> &dyn MuxAppBackendProvider {
        match self {
            Self::Native(provider) => provider,
            Self::Scripted(provider) => provider,
        }
    }
}

impl MuxBackendProvider for MixedProvider {
    fn kind(&self) -> MuxBackendKind {
        self.provider().kind()
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        self.provider().command_dispatch()
    }

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        self.provider().build_backend(config, workspace)
    }
}

impl MuxAppBackendProvider for MixedProvider {
    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        self.provider().build_pane_policy(config)
    }

    fn app_policy(&self) -> MuxAppBackendPolicy {
        self.provider().app_policy()
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        self.provider().capabilities(scope)
    }
}

/// Run frames until `ready` answers, bounded for real process startup.
fn wait_until<T>(
    state: &mut AppState,
    what: &str,
    mut ready: impl FnMut(&mut AppState) -> Option<T>,
) -> T {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if let Some(value) = ready(state) {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn targeted(command: &str, arguments: Vec<String>, target: serde_json::Value) -> CommandInvocation {
    CommandInvocation {
        target: Some(serde_json::from_value(target).expect("command target")),
        ..CommandInvocation::new(command, arguments, Caller::Socket)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetSpace {
    /// The active Space, which has no session yet and so shows the new one at once.
    EmptyActive,
    /// The active Space, which keeps showing the session it already has.
    Active,
    /// A native Space that is not active.
    Inactive,
}

/// A native session's command runs from the moment the session is created, whether or not its
/// pane is shown, and showing the pane later presents that same process: in the active Space, in
/// an inactive one, and while another backend's Space is active and no native Space has been shown.
#[rstest]
#[case::in_an_empty_active_space(MultiplexerBackendConfig::Native, TargetSpace::EmptyActive)]
#[case::in_the_active_space(MultiplexerBackendConfig::Native, TargetSpace::Active)]
#[case::in_an_inactive_space(MultiplexerBackendConfig::Native, TargetSpace::Inactive)]
#[case::while_another_backend_is_active(MultiplexerBackendConfig::Tmux, TargetSpace::Inactive)]
fn a_native_session_command_starts_at_once_and_is_the_process_shown_later(
    directory: assert_fs::TempDir,
    #[case] home_backend: MultiplexerBackendConfig,
    #[case] created_in: TargetSpace,
) {
    let config_path = directory.path().join("config.toml");
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let native = SpaceMuxOverride {
        backend: Some(MultiplexerBackendConfig::Native),
        remote: SpaceRemoteOverride::Local,
    };
    let scripts = create_space(&mut repository, "Scripts", "2", [2; 3], native.clone()).id();
    let other = create_space(&mut repository, "Other", "3", [3; 3], native).id();
    drop(repository);
    let scripted = restore_provider(MuxBackendKind::Tmux, Arc::default(), Arc::default());
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [
                Arc::new(MixedProvider::Native(bootty_mux::native::NativeProvider)),
                Arc::new(MixedProvider::Scripted(scripted)),
            ],
            [MuxBackendKind::Native, MuxBackendKind::Tmux],
        )
        .expect("mixed backend registry"),
    );
    let mut state = app_state(test_config::config(config_path, home_backend), backends);
    let home = state.active_space_id();
    let space_target = |state: &mut AppState, name: &str| {
        let outcome = submit_command(
            state,
            CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
            Instant::now(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("spaces.list failed: {outcome:?}");
        };
        value
            .as_array()
            .expect("a list of Spaces")
            .iter()
            .find(|space| space["name"] == name || (name.is_empty() && space["active"] == true))
            .map(|space| space["target"].clone())
            .expect("the Space is listed")
    };
    let project = directory.path().join("project");
    std::fs::create_dir(&project).expect("project directory");
    let cwd = project.to_string_lossy().into_owned();
    let home_target = space_target(&mut state, "");
    if created_in != TargetSpace::EmptyActive {
        let outcome = submit_command(
            &mut state,
            targeted(
                "session.create",
                vec!["project".to_owned(), cwd.clone()],
                home_target.clone(),
            ),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    let selection = |state: &AppState| {
        (
            state.mux().selected_session().map(str::to_owned),
            state.mux().selected_window().map(str::to_owned),
        )
    };
    let before = selection(&state);

    let marker = directory.path().join("started");
    let received = directory.path().join("received");
    let argv = [
        "/bin/sh",
        "-c",
        "printf '%s|%s\\n' \"$BOOTTY_PANE\" \"$(pwd -P)\" >> \"$1\"; printf eager-token; exec cat >> \"$2\"",
        "agent",
        &marker.to_string_lossy(),
        &received.to_string_lossy(),
    ];
    let target = if created_in == TargetSpace::Inactive {
        space_target(&mut state, "Scripts")
    } else {
        home_target
    };
    let outcome = submit_command(
        &mut state,
        targeted(
            "session.create",
            vec![
                "agent".to_owned(),
                cwd,
                serde_json::to_string(&argv).expect("encode argv"),
            ],
            target,
        ),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("session.create failed: {outcome:?}");
    };
    let terminal = value["terminal"].clone();

    wait_until(
        &mut state,
        "the command starts when the session is created",
        |_| {
            std::fs::read_to_string(&marker)
                .ok()
                .filter(|text| text.ends_with('\n'))
        },
    );
    match created_in {
        // An empty Space shows the first session it gets, so this pane is on screen at once.
        TargetSpace::EmptyActive => {
            assert_eq!(selection(&state).0.as_deref(), Some("agent"));
        }
        TargetSpace::Active | TargetSpace::Inactive => {
            assert_eq!(selection(&state), before);
        }
    }
    assert_eq!(state.active_space_id(), home);

    let shows_token = |state: &mut AppState| {
        let outcome = submit_command(
            state,
            targeted("terminal.capture", Vec::new(), terminal.clone()),
            Instant::now(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            return None;
        };
        value["capture"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("eager-token"))
            .then_some(())
    };
    // Before anything shows it, even in an inactive Space, the pane is readable and takes input.
    wait_until(&mut state, "the hidden pane is readable", shows_token);
    for (command, arguments) in [
        ("terminal.paste", vec!["hidden-input".to_owned()]),
        ("terminal.submit", Vec::new()),
    ] {
        let outcome = submit_command(
            &mut state,
            targeted(command, arguments, terminal.clone()),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{command}: {outcome:?}"
        );
    }
    wait_until(
        &mut state,
        "input reaches the hidden pane's process",
        |_| {
            std::fs::read_to_string(&received)
                .ok()
                .filter(|text| text.contains("hidden-input"))
        },
    );
    assert_eq!(state.active_space_id(), home);

    if created_in == TargetSpace::Inactive {
        // Pass through another native Space first: the pane has to follow its owner each time.
        assert!(state.activate_space_from_ui(other));
        state.update_frame(frames::idle_frame(Instant::now()));
        assert!(state.activate_space_from_ui(scripts));
    }
    state.activate_session_from_ui("agent");
    wait_until(
        &mut state,
        "the shown pane is the process the create started",
        shows_token,
    );
    let pane = state
        .mux()
        .backend_session_by_id_or_name("agent")
        .and_then(|session| session.windows.first()?.panes.first()?.pane_id.clone())
        .expect("the session's pane");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker"),
        format!(
            "{pane}|{}\n",
            project.canonicalize().expect("canonical cwd").display()
        ),
        "the command ran once, in its cwd, as the pane Bootty shows"
    );
}

/// `pane.close` and `session.close` end a hidden native pane's process and drop the pane, leaving
/// selection alone. Closing a session's only pane ends the session, as in tmux and rmux.
#[rstest]
#[case::pane("pane.close")]
#[case::session("session.close")]
fn closing_ends_a_hidden_native_pane_without_moving_selection(
    directory: assert_fs::TempDir,
    #[case] close_command: &str,
) {
    let config_path = directory.path().join("config.toml");
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(MixedProvider::Native(
                bootty_mux::native::NativeProvider,
            ))],
            [MuxBackendKind::Native],
        )
        .expect("native backend registry"),
    );
    let mut state = app_state(
        test_config::config(config_path, MultiplexerBackendConfig::Native),
        backends,
    );
    let outcome = submit_command(
        &mut state,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("spaces.list failed: {outcome:?}");
    };
    let home = value[0]["target"].clone();
    let cwd = directory.path().to_string_lossy().into_owned();
    let pid_file = directory.path().join("pid");
    for (name, argv) in [
        ("shown", None),
        (
            "agent",
            Some(serde_json::json!([
                "/bin/sh",
                "-c",
                "echo $$ > \"$1\"; exec sleep 60",
                "agent",
                pid_file.to_string_lossy()
            ])),
        ),
    ] {
        let mut arguments = vec![name.to_owned(), cwd.clone()];
        arguments.extend(argv.map(|argv| argv.to_string()));
        let outcome = submit_command(
            &mut state,
            targeted("session.create", arguments, home.clone()),
            Instant::now(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("session.create {name} failed: {outcome:?}");
        };
        if name == "agent" {
            let terminal = value["terminal"].clone();
            let pid = wait_until(&mut state, "the hidden pane's process starts", |_| {
                std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok())
            });
            let selected = state.mux().selected_session().map(str::to_owned);

            let close_target = if close_command == "pane.close" {
                terminal.clone()
            } else {
                value["created"].clone()
            };
            let mut close = targeted(close_command, Vec::new(), close_target);
            let outcome = submit_command(&mut state, close.clone(), Instant::now());
            let CommandOutcome::ConfirmationRequired { confirmation } = outcome else {
                panic!("expected close confirmation: {outcome:?}");
            };
            close.confirmation = Some(*confirmation);
            let outcome = submit_command(&mut state, close, Instant::now());
            assert!(
                matches!(outcome, CommandOutcome::Success { .. }),
                "{outcome:?}"
            );
            wait_until(&mut state, "the closed pane's process exits", |_| {
                let alive = std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .status()
                    .is_ok_and(|status| status.success());
                (!alive).then_some(())
            });
            let pane = terminal["handle"]
                .as_str()
                .and_then(|handle| serde_json::from_str::<Vec<String>>(handle).ok())
                .and_then(|path| path.last().cloned())
                .expect("terminal target names its pane");
            assert!(
                !state
                    .mux()
                    .all_sessions()
                    .iter()
                    .flat_map(|session| &session.windows)
                    .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes))
                    .any(|anchor| anchor.pane_id.as_deref() == Some(pane.as_str())),
                "the closed pane is gone from the snapshot"
            );
            assert_eq!(state.mux().selected_session().map(str::to_owned), selected);
            // The session ended with its only pane, so its name is free again.
            assert!(
                !state
                    .mux()
                    .all_sessions()
                    .iter()
                    .any(|session| session.name == "agent"),
                "the session ends with its last pane"
            );
            let reused = submit_command(
                &mut state,
                targeted(
                    "session.create",
                    vec!["agent".to_owned(), cwd.clone()],
                    home.clone(),
                ),
                Instant::now(),
            );
            assert!(
                matches!(reused, CommandOutcome::Success { .. }),
                "{reused:?}"
            );
        }
    }
}

/// Every Space in `spaces.list` order: its name, scope, and Binding target.
fn listed_spaces(state: &mut AppState) -> Vec<(String, String, serde_json::Value)> {
    let outcome = submit_command(
        state,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("spaces.list failed: {outcome:?}");
    };
    value
        .as_array()
        .expect("a list of Spaces")
        .iter()
        .map(|space| {
            (
                space["name"].as_str().expect("Space name").to_owned(),
                space["scope"].as_str().expect("Space scope").to_owned(),
                space["target"].clone(),
            )
        })
        .collect()
}

/// Submit through the socket mailbox and run frames in real time until the command answers.
fn submit_and_wait(state: &mut AppState, invocation: CommandInvocation) -> CommandOutcome {
    let (response, outcomes) = mpsc::channel();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    state
        .app_command_sender(Caller::Socket)
        .try_send(AppCommandRequest {
            invocation,
            deadline,
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    wait_until(state, "the command answers", |_| outcomes.try_recv().ok())
}

/// An rmux-style pane Bootty has no runtime for, such as one in a session created behind the
/// selected one, takes targeted input and capture through its backend.
#[rstest]
fn a_backend_pane_without_a_runtime_takes_input_and_capture_through_its_backend(
    directory: assert_fs::TempDir,
) {
    let provider = RestoreProvider {
        topology: PaneTopology::BackendReconciled,
        created_panes: true,
        ..restore_provider(MuxBackendKind::Rmux, Arc::default(), Arc::default())
    };
    let inputs = Arc::clone(&provider.pane_inputs);
    let backends = registry([Arc::new(provider)], [MuxBackendKind::Rmux]);
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Rmux,
    );
    let mut state = app_state(config, backends);
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let cwd = directory.path().to_string_lossy().into_owned();
    let mut terminal = serde_json::Value::Null;
    for name in ["shown", "hidden"] {
        let outcome = submit_command(
            &mut state,
            targeted(
                "session.create",
                vec![name.to_owned(), cwd.clone()],
                home.clone(),
            ),
            Instant::now(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("session.create {name} failed: {outcome:?}");
        };
        terminal = value["terminal"].clone();
    }
    assert_eq!(state.mux().selected_session(), Some("shown"));

    for (command, arguments) in [
        ("terminal.paste", vec!["hidden input".to_owned()]),
        ("terminal.submit", Vec::new()),
    ] {
        let outcome = submit_command(
            &mut state,
            targeted(command, arguments, terminal.clone()),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{command}: {outcome:?}"
        );
    }
    assert_eq!(
        *inputs.lock().expect("pane inputs lock"),
        [
            ("%2".to_owned(), PaneInput::Paste("hidden input".to_owned())),
            ("%2".to_owned(), PaneInput::Submit),
        ]
    );
    let outcome = submit_command(
        &mut state,
        targeted("terminal.capture", Vec::new(), terminal),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("terminal.capture failed: {outcome:?}");
    };
    assert_eq!(
        (&value["capture"]["text"], &value["source"]["kind"]),
        (
            &serde_json::json!("%2 read by its backend"),
            &serde_json::json!("backend_pane")
        )
    );
    assert_eq!(state.mux().selected_session(), Some("shown"));
}

/// An agent record follows its pane. A hook that arrives before Bootty lists its pane shows once
/// the pane is discovered, and moving the session to another Space on the same server moves the
/// record with its final message once that Space lists it, leaving one entry.
#[cfg(unix)]
#[rstest]
fn an_agent_record_follows_its_pane_to_the_space_that_owns_it(directory: assert_fs::TempDir) {
    let socket = directory.path().join("tmux.sock");
    std::fs::write(&socket, "").expect("socket stand-in");
    let config_path = directory.path().join("config.toml");
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    let other = create_space(
        &mut repository,
        "Other",
        "2",
        [2; 3],
        SpaceMuxOverride::default(),
    )
    .id();
    drop(repository);
    let backends = registry(
        [Arc::new(RestoreProvider {
            created_panes: true,
            server_socket: Some(socket.clone()),
            ..restore_provider(MuxBackendKind::Tmux, Arc::default(), Arc::default())
        })],
        [MuxBackendKind::Tmux],
    );
    let (events, received) = bootty_control::event_queue();
    drop(received);
    let mut state = AppState::new_for_window_with_agents(
        test_config::config(config_path, MultiplexerBackendConfig::Tmux),
        "main".to_owned(),
        backends,
        Arc::new(|| {}),
        None,
        None,
        Some(events),
    )
    .expect("app state with agents");
    let (_, home_scope, home) = listed_spaces(&mut state).remove(0);
    let listed = |state: &mut AppState| {
        state.update_frame(frames::idle_frame(Instant::now()));
        state
            .agent_overview()
            .into_iter()
            .map(|entry| (entry.scope, entry.title, entry.last_message))
            .collect::<Vec<_>>()
    };
    let entry = |scope: String| vec![(scope, "agent".to_owned(), Some("Done.".to_owned()))];

    let reported = submit_and_wait(
        &mut state,
        CommandInvocation::new(
            "agents.claude.ingest",
            vec![
                r#"{"hook_event_name":"Stop","last_assistant_message":"Done."}"#.to_owned(),
                "%1".to_owned(),
                String::new(),
                format!("{},4242,0", socket.display()),
            ],
            Caller::Socket,
        ),
    );
    assert!(
        matches!(reported, CommandOutcome::Success { .. }),
        "{reported:?}"
    );
    assert_eq!(listed(&mut state), []);

    let created = submit_command(
        &mut state,
        targeted(
            "session.create",
            vec![
                "agent".to_owned(),
                directory.path().to_string_lossy().into_owned(),
            ],
            home,
        ),
        Instant::now(),
    );
    assert!(
        matches!(created, CommandOutcome::Success { .. }),
        "{created:?}"
    );
    assert_eq!(listed(&mut state), entry(home_scope.clone()));

    let session = state
        .mux()
        .backend_session_by_id_or_name("agent")
        .expect("created session")
        .id
        .clone();
    let home_id = SpaceId::from_persistence(home_scope.parse().expect("numeric scope"));
    assert!(state.move_scoped_session_to_space(&ScopedSessionTarget::new(home_id, session), other));
    // Until the other Space lists the session, the record waits rather than showing in the old one.
    assert_eq!(listed(&mut state), []);
    assert!(state.activate_space_from_ui(other));
    assert_eq!(
        listed(&mut state),
        entry(other.persistence_value().to_string())
    );
}

/// Pane ids repeat across servers. A hook that names its server lands on the Space bound to that
/// server, even when tmux reports the socket through a resolved path; a hook from an adapter that
/// names no server still lands nowhere while two Spaces list its pane. A server no Space names
/// lands nowhere either, even beside a Space whose server Bootty cannot name, such as a remote one.
#[cfg(unix)]
#[rstest]
#[case::both_local(true)]
#[case::one_unnamed(false)]
fn hooks_with_colliding_pane_ids_land_on_the_space_of_their_server(
    directory: assert_fs::TempDir,
    #[case] rmux_named: bool,
) {
    let sockets = directory.path().join("sockets");
    std::fs::create_dir(&sockets).expect("socket directory");
    for socket in ["tmux.sock", "rmux.sock"] {
        std::fs::write(sockets.join(socket), "").expect("socket stand-in");
    }
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&sockets, &link).expect("symlinked socket directory");
    let config_path = directory.path().join("config.toml");
    let (mut repository, _) = WorkspaceRepository::open(&config_path).expect("workspace");
    create_space(
        &mut repository,
        "Rmux",
        "2",
        [2; 3],
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Rmux),
            remote: SpaceRemoteOverride::Local,
        },
    );
    drop(repository);
    let provider = |kind, server_socket| {
        Arc::new(RestoreProvider {
            created_panes: true,
            server_socket,
            ..restore_provider(kind, Arc::default(), Arc::default())
        })
    };
    let backends = registry(
        [
            provider(MuxBackendKind::Tmux, Some(link.join("tmux.sock"))),
            provider(
                MuxBackendKind::Rmux,
                rmux_named.then(|| sockets.join("rmux.sock")),
            ),
        ],
        [MuxBackendKind::Tmux, MuxBackendKind::Rmux],
    );
    let (events, received) = bootty_control::event_queue();
    drop(received);
    let mut state = AppState::new_for_window_with_agents(
        test_config::config(config_path, MultiplexerBackendConfig::Tmux),
        "main".to_owned(),
        backends,
        Arc::new(|| {}),
        None,
        None,
        Some(events),
    )
    .expect("app state with agents");
    let spaces = listed_spaces(&mut state);
    let cwd = directory.path().to_string_lossy().into_owned();
    for ((_, _, target), session) in spaces.iter().zip(["tmux-agent", "rmux-agent"]) {
        let outcome = submit_command(
            &mut state,
            targeted(
                "session.create",
                vec![session.to_owned(), cwd.clone()],
                target.clone(),
            ),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    let resolved = sockets.canonicalize().expect("resolved socket directory");
    let report = |state: &mut AppState, server: String| {
        let outcome = submit_and_wait(
            state,
            CommandInvocation::new(
                "agents.claude.ingest",
                vec![
                    r#"{"hook_event_name":"SessionStart"}"#.to_owned(),
                    "%1".to_owned(),
                    String::new(),
                    server,
                ],
                Caller::Socket,
            ),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        state.update_frame(frames::idle_frame(Instant::now()));
        let mut listed = state
            .agent_overview()
            .into_iter()
            .map(|entry| (entry.scope, entry.title))
            .collect::<Vec<_>>();
        listed.sort();
        listed
    };
    assert_eq!(report(&mut state, String::new()), []);
    let (tmux_scope, rmux_scope) = (spaces[0].1.clone(), spaces[1].1.clone());
    let tmux = (tmux_scope, "tmux-agent".to_owned());
    let rmux = (rmux_scope, "rmux-agent".to_owned());
    assert_eq!(
        report(
            &mut state,
            format!("{},4141,0", resolved.join("other.sock").display())
        ),
        []
    );
    assert_eq!(
        report(
            &mut state,
            format!("{},4242,0", resolved.join("tmux.sock").display())
        ),
        std::slice::from_ref(&tmux)
    );
    let mut expected = vec![tmux];
    if rmux_named {
        expected.push(rmux);
        expected.sort();
    }
    assert_eq!(
        report(
            &mut state,
            format!("{},4343,1", resolved.join("rmux.sock").display())
        ),
        expected
    );
}

/// A native create whose first pane cannot start fails instead of reporting a dead pane as
/// created, and the session it made is closed so the name is free again. A cwd that is not a
/// directory fails before anything is created.
#[rstest]
fn a_native_session_that_cannot_start_fails_its_create_and_frees_its_name(
    directory: assert_fs::TempDir,
) {
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(MixedProvider::Native(
                bootty_mux::native::NativeProvider,
            ))],
            [MuxBackendKind::Native],
        )
        .expect("native backend registry"),
    );
    let mut state = app_state(
        test_config::config(
            directory.path().join("config.toml"),
            MultiplexerBackendConfig::Native,
        ),
        backends,
    );
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let cwd = directory.path().to_string_lossy().into_owned();
    let create = |cwd: &str, argv: serde_json::Value| {
        targeted(
            "session.create",
            vec!["agent".to_owned(), cwd.to_owned(), argv.to_string()],
            home.clone(),
        )
    };
    let has_agent = |state: &AppState| {
        state
            .mux()
            .all_sessions()
            .iter()
            .any(|session| session.name == "agent")
    };

    let missing = submit_and_wait(
        &mut state,
        create(
            &cwd,
            serde_json::json!(["bootty-no-such-program", "--flag"]),
        ),
    );
    assert!(
        matches!(&missing, CommandOutcome::Failed { code, message }
            if code == "session_start_failed" && message.contains("bootty-no-such-program")),
        "{missing:?}"
    );
    assert!(!has_agent(&state), "the failed session is closed");

    let not_a_directory = directory.path().join("file");
    std::fs::write(&not_a_directory, "").expect("plain file");
    let invalid = submit_and_wait(
        &mut state,
        create(
            &not_a_directory.to_string_lossy(),
            serde_json::json!(["/bin/sh", "-c", "exit 0"]),
        ),
    );
    assert!(
        matches!(&invalid, CommandOutcome::Failed { code, .. } if code == "invalid_arguments"),
        "{invalid:?}"
    );
    assert!(!has_agent(&state), "nothing was created");

    let started = submit_and_wait(
        &mut state,
        create(&cwd, serde_json::json!(["/bin/sh", "-c", "exec sleep 60"])),
    );
    assert!(
        matches!(started, CommandOutcome::Success { .. }),
        "{started:?}"
    );
    assert!(has_agent(&state));
}

/// A native create still starting when its session is closed and a new one takes the name
/// answers for its own session only: it fails, and the new session keeps running.
#[rstest]
fn a_starting_native_create_never_answers_for_or_closes_a_recreated_session(
    directory: assert_fs::TempDir,
) {
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(MixedProvider::Native(
                bootty_mux::native::NativeProvider,
            ))],
            [MuxBackendKind::Native],
        )
        .expect("native backend registry"),
    );
    let mut state = app_state(
        test_config::config(
            directory.path().join("config.toml"),
            MultiplexerBackendConfig::Native,
        ),
        backends,
    );
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let cwd = directory.path().to_string_lossy().into_owned();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    // Queued together, so the first create is still starting when the others run.
    let queue = |invocation| {
        let (response, outcome) = mpsc::channel();
        state
            .app_command_sender(Caller::Socket)
            .try_send(AppCommandRequest {
                invocation,
                deadline,
                cancellation: CommandCancellation::new(),
                response,
            })
            .expect("submit command");
        outcome
    };
    let create = |argv: serde_json::Value| {
        targeted(
            "session.create",
            vec!["agent".to_owned(), cwd.clone(), argv.to_string()],
            home.clone(),
        )
    };
    let session = serde_json::json!({
        "kind": "session",
        "handle": serde_json::to_string(&[home["handle"].as_str().expect("Space handle"), "agent"])
            .expect("session handle"),
        "generation": home["generation"],
    });
    let mut close = targeted("session.close", Vec::new(), session);
    close.confirmation = Some(close.confirmation());

    let first = queue(create(serde_json::json!([
        "bootty-no-such-program",
        "--flag"
    ])));
    let closed = queue(close);
    let second = queue(create(serde_json::json!([
        "/bin/sh",
        "-c",
        "exec sleep 60"
    ])));

    let first = wait_until(&mut state, "the first create answers", |_| {
        first.try_recv().ok()
    });
    // Normally the close finds the first create still starting. If its program fails first, the
    // create closes its own session and this close finds nothing; either way the rest holds.
    wait_until(&mut state, "the close answers", |_| closed.try_recv().ok());
    let second = wait_until(&mut state, "the second create answers", |_| {
        second.try_recv().ok()
    });
    assert!(
        matches!(&first, CommandOutcome::Failed { code, .. } if code == "session_start_failed"),
        "{first:?}"
    );
    assert!(
        matches!(second, CommandOutcome::Success { .. }),
        "{second:?}"
    );
    assert!(
        state
            .mux()
            .all_sessions()
            .iter()
            .any(|session| session.name == "agent"),
        "the recreated session keeps running"
    );
}

/// A mux command that fails on the backend's worker after its dispatch answers its socket caller
/// with the failure and leaves the window without an error notice nobody at the window asked for.
#[rstest]
fn an_asynchronous_mux_failure_answers_a_socket_caller_without_a_window_notice(
    directory: assert_fs::TempDir,
) {
    let (release_tx, release_rx) = mpsc::channel();
    let backends = registry(
        [Arc::new(RestoreProvider {
            dispatch: MuxCommandDispatch::WorkerThread,
            fail_commands: true,
            release: Some(Arc::new(Mutex::new(release_rx))),
            ..restore_provider(MuxBackendKind::Tmux, Arc::default(), Arc::default())
        })],
        [MuxBackendKind::Tmux],
    );
    let mut state = app_state(
        test_config::config(
            directory.path().join("config.toml"),
            MultiplexerBackendConfig::Tmux,
        ),
        backends,
    );
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let (response, outcomes) = mpsc::channel();
    state
        .app_command_sender(Caller::Socket)
        .try_send(AppCommandRequest {
            invocation: targeted(
                "session.create",
                vec![
                    "agent".to_owned(),
                    directory.path().to_string_lossy().into_owned(),
                ],
                home,
            ),
            deadline: Instant::now()
                .checked_add(Duration::from_secs(5))
                .expect("test deadline"),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit command");
    state.update_frame(frames::idle_frame(Instant::now()));
    assert!(
        outcomes.try_recv().is_err(),
        "the failure must land after dispatch"
    );
    release_tx.send(()).expect("release the backend");
    let outcome = wait_until(&mut state, "the failure answers", |_| {
        outcomes.try_recv().ok()
    });
    assert!(
        matches!(outcome, CommandOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(state.last_error(), None);
}
