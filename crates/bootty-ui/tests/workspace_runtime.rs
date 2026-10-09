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
        MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider, MuxBackendRegistry,
        MuxCommandDispatch, PaneBehavior, PaneTopology, SelectionPublicationPolicy,
        TerminalProgressPolicy, TerminalResidency,
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
    fail_commands: bool,
    created_panes: bool,
    pane_inputs: PaneInputs,
}

impl RestoreBackend {
    fn activate_window(
        &self,
        session_id: &str,
        selected: impl FnOnce(&MuxSession) -> Option<String>,
    ) {
        let mut sessions = self.sessions.lock().expect("backend sessions");
        let session = sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("session");
        session.active_window_id = selected(session);
        drop(sessions);
    }
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
                self.create_calls.fetch_add(1, Ordering::SeqCst);
                if let Some(release) = &self.release {
                    release
                        .lock()
                        .expect("restore backend release lock")
                        .recv_timeout(Duration::from_secs(2))
                        .expect("release delayed create");
                }
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
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                self.activate_window(&session_id, |session| {
                    session
                        .windows
                        .iter()
                        .find_map(|window| (window.index == index).then(|| window.id.clone()))
                });
            }
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => {
                self.activate_window(&session_id, |_| Some(window_id));
            }
            MuxCommand::CreatePane {
                session_id,
                pane_id: Some(parent),
                ..
            } => {
                let mut sessions = self.sessions.lock().expect("backend sessions");
                let session = sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
                    .expect("session");
                let window = session
                    .windows
                    .iter_mut()
                    .find(|window| {
                        window
                            .panes
                            .iter()
                            .any(|pane| pane.pane_id.as_deref() == Some(&parent))
                    })
                    .expect("parent window");
                let mut pane = window.anchor.clone();
                pane.pane_id = Some(format!("{parent}-child"));
                window.panes.push(pane.clone());
                window.anchor = pane;
                drop(sessions);
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

            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: self.selection_publication,
        }
    }

    fn local_server_socket(&self, _config: &MuxBindingConfig) -> Option<std::path::PathBuf> {
        self.server_socket.clone()
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        let mut operations = vec![
            BindingOperation::ActivateWindow,
            BindingOperation::NavigateWindow,
            BindingOperation::CreateProjectSession,
            BindingOperation::CreateWindow,
            BindingOperation::SplitPane,
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
        display_name: backend_name.to_owned(),
        explicit: false,
        cwd: cwd.to_owned(),
        state: bootty_mux::session_membership::SessionState::default(),
        terminal_snapshot: None,
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

#[rstest]
fn creating_a_pane_in_a_background_window_returns_that_pane(directory: assert_fs::TempDir) {
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Tmux,
    );
    let cwd = directory.path().to_string_lossy().into_owned();
    let (repository, space) = claim_first_space(&config.config_path, "task", "first", &cwd);
    drop(repository);
    let mut session = session_on_pane("first", "%1", Some(cwd));
    session.tag = MuxSessionTag {
        identity: Some("task".to_owned()),
        space: Some(space.remote_id().to_owned()),
    };
    let mut other = session.windows[0].clone();
    other.id = "other-window".to_owned();
    other.index = 1;
    other.anchor.pane_id = Some("%2".to_owned());
    other.panes = vec![other.anchor.clone()];
    session.windows.push(other);
    let sessions = Arc::new(Mutex::new(vec![session]));
    let backends = registry(
        [Arc::new(restore_provider(
            MuxBackendKind::Tmux,
            sessions,
            Arc::default(),
        ))],
        [MuxBackendKind::Tmux],
    );
    let mut state = app_state(config, backends);
    state.update_frame(frames::idle_frame(Instant::now()));
    let CommandOutcome::Success { value, .. } = submit_command(
        &mut state,
        CommandInvocation::new(
            "resource.current",
            vec!["terminal".to_owned()],
            Caller::Socket,
        ),
        Instant::now(),
    ) else {
        panic!("issued source terminal");
    };
    let parent = value["target"].clone();
    assert!(matches!(
        submit_command(
            &mut state,
            CommandInvocation::from_action("next_tab", Caller::Socket),
            Instant::now()
        ),
        CommandOutcome::Success { .. }
    ));
    assert_eq!(state.mux().selected_window(), Some("other-window"));
    let outcome = submit_command(
        &mut state,
        targeted(
            "terminal.create_pane",
            vec![
                "right".to_owned(),
                "[]".to_owned(),
                directory.path().to_string_lossy().into_owned(),
            ],
            parent,
        ),
        Instant::now(),
    );
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("pane creation: {outcome:?}");
    };
    let created: bootty_control::CommandTarget =
        serde_json::from_value(value["created"].clone()).expect("issued created pane");
    let path: Vec<String> = serde_json::from_str(&created.handle).unwrap();
    assert_eq!(&path[1..], ["first", "first-window", "%1-child"]);
    assert_eq!(state.mux().selected_window(), Some("other-window"));
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
            creation_receipt: None,
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
            .is_some_and(|error| error.contains("save selected identity"))
    );
}

#[rstest]
fn native_pane_publication_error_is_preserved_on_successful_command(directory: assert_fs::TempDir) {
    let config_path = directory.path().join("config.toml");
    let cwd = directory.path().to_string_lossy().into_owned();
    let (repository, space) = claim_first_space(&config_path, "first-id", "first", &cwd);
    drop(repository);
    let config = test_config::config(config_path, MultiplexerBackendConfig::Tmux);
    let mut first = session_on_pane("first", "first-pane", Some(cwd));
    first.tag = MuxSessionTag {
        identity: Some("first-id".to_owned()),
        space: Some(space.remote_id().to_owned()),
    };
    let backends = registry(
        [Arc::new(RestoreProvider {
            topology: PaneTopology::ProcessLocal,
            ..restore_provider(
                MuxBackendKind::Tmux,
                Arc::new(Mutex::new(vec![first])),
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
        CommandInvocation::from_action("next_tab", Caller::Socket),
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
    let (_, saved) = WorkspaceRepository::open(&config_path).expect("saved adoption");
    assert_eq!(
        saved.spaces()[0].binding().sessions().sessions()[0].display_name,
        "kept"
    );
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
    assert_eq!(
        first.binding().sessions().sessions().len(),
        1,
        "the closed attachment retains its saved identity in its original Space"
    );
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
    let binding = listed_spaces(&mut state)[0].2.clone();
    let commands = state.app_command_sender(Caller::Socket);
    let (response, outcomes) = mpsc::channel();
    commands
        .try_send(AppCommandRequest {
            creation_receipt: None,
            invocation: targeted(
                "session.create",
                vec![
                    "retained".to_owned(),
                    directory.path().to_string_lossy().into_owned(),
                ],
                binding,
            ),
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
    assert_eq!(
        create_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "journal recovery never respawns saved terminals"
    );
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
    let (release_tx, release_rx) = mpsc::channel();
    let backends = registry(
        [Arc::new(RestoreProvider {
            dispatch: MuxCommandDispatch::WorkerThread,
            release: Some(Arc::new(Mutex::new(release_rx))),
            created_panes: true,
            ..restore_provider(MuxBackendKind::Native, Arc::default(), Arc::default())
        })],
        [MuxBackendKind::Native],
    );
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new(
        config,
        backends,
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("app state");
    release_tx.send(()).expect("release first create");
    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: first_cwd.to_string_lossy().into_owned(),
    });
    observe_project_creation(&mut state, &wakes, "project").unwrap();
    assert!(state.modal_dialog().is_none(), "first creation completed");

    state.apply_picker_event(NewSessionPickerEvent::CreateSession {
        cwd: second_cwd.to_string_lossy().into_owned(),
    });
    // The form submits through the mailbox; admit creation before testing reload deferral.
    state.update_frame(frames::idle_frame(Instant::now()));
    assert!(
        !repository
            .pending_binding_membership_mutations(scope)
            .expect("pending before reload")
            .is_empty(),
        "the held worker create must retain its journal"
    );
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

    release_tx
        .send(())
        .expect("release second create after reload");
    observe_project_creation(&mut state, &wakes, "project-2").unwrap();
    assert_eq!(
        repository
            .pending_binding_membership_mutations(scope)
            .expect("read completed operation"),
        []
    );
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

fn observe_project_creation(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    backend_name: &str,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .ok_or_else(|| anyhow::anyhow!("creation deadline does not fit"))?;
    let mut frame_at = Instant::now();
    loop {
        let effects = state.update_frame(frames::idle_frame(frame_at));
        let projection = state.dialog_projection();
        if state.modal_dialog().is_none()
            && state
                .binding_session_groups()
                .iter()
                .flat_map(|group| &group.sessions)
                .any(|session| session.name == backend_name)
        {
            return Ok(());
        }
        // The real host schedules this frame after a deferred controller rebuild.
        // Advance the injected frame clock instead of sleeping in the fixture.
        if state.modal_dialog().is_none()
            && Instant::now() < deadline
            && let Some(after) = effects
                .iter()
                .filter_map(|effect| match effect {
                    bootty_ui::AppEffect::RepaintAfter(after) => Some(*after),
                    _ => None,
                })
                .min()
        {
            frame_at = frame_at
                .checked_add(after)
                .ok_or_else(|| anyhow::anyhow!("scheduled creation frame does not fit"))?;
            continue;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || wakes.recv_timeout(remaining).is_err() {
            let observed = state
                .binding_session_groups()
                .iter()
                .flat_map(|group| &group.sessions)
                .map(|session| session.name.clone())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "project {backend_name} creation did not complete; last error: {:?}; observed sessions: {observed:?}; dialog: {projection:?}",
                state.last_error()
            );
        }
        frame_at = frame_at.max(Instant::now());
    }
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
    let scripted = RestoreProvider {
        created_panes: true,
        ..restore_provider(MuxBackendKind::Tmux, Arc::default(), Arc::default())
    };
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
            creation_receipt: None,
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
                creation_receipt: None,
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
            creation_receipt: None,
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

#[rstest]
fn backend_rename_and_shell_directory_changes_preserve_saved_title_and_project(
    directory: assert_fs::TempDir,
) {
    let config_path = directory.path().join("config.toml");
    let project = directory.path().join("project");
    let elsewhere = directory.path().join("elsewhere");
    std::fs::create_dir(&project).expect("project");
    std::fs::create_dir(&elsewhere).expect("elsewhere");
    let cwd = project.to_string_lossy().into_owned();
    let (mut repository, space) =
        claim_first_space(&config_path, "saved-id", "Original title", &cwd);
    let other = create_space(
        &mut repository,
        "Other",
        "2",
        [1, 2, 3],
        SpaceMuxOverride::default(),
    );
    drop(repository);
    let sessions = Arc::new(Mutex::new(vec![mux_session(
        "Original title",
        cwd.clone(),
        MuxSessionTag {
            identity: Some("saved-id".to_owned()),
            space: Some(space.remote_id().to_owned()),
        },
        true,
    )]));
    let backends = registry(
        [Arc::new(restore_provider(
            MuxBackendKind::Tmux,
            Arc::clone(&sessions),
            Arc::default(),
        ))],
        [MuxBackendKind::Tmux],
    );
    let config = test_config::config(config_path.clone(), MultiplexerBackendConfig::Tmux);
    let mut state = app_state(config, backends);
    state.update_frame(frames::idle_frame(Instant::now()));
    {
        let mut live = sessions.lock().expect("backend session");
        live[0].name = "backend-renamed".to_owned();
        live[0].anchor.cwd = Some(elsewhere.to_string_lossy().into_owned());
    }
    // Switching back requests a fresh snapshot through the public application flow.
    assert!(state.activate_space_from_ui(other.id()));
    assert!(state.activate_space_from_ui(space.id()));
    state.update_frame(frames::idle_frame(Instant::now()));
    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("saved workspace");
    let saved = &snapshot.spaces()[0].binding().sessions().sessions()[0];
    assert_eq!(saved.identity, "saved-id");
    assert_eq!(saved.backend_name, "backend-renamed");
    assert_eq!(saved.label(), "Original title");
    assert_eq!(saved.cwd, cwd);
}

/// Saved metadata commands complete synchronously through the same socket mailbox.
fn saved_metadata_command(
    state: &mut AppState,
    command: &str,
    arguments: Vec<String>,
    binding: serde_json::Value,
) -> CommandOutcome {
    let mut invocation = targeted(command, arguments, binding);
    if command == "session.delete" {
        invocation.confirmation = Some(invocation.confirmation());
    }
    let (response, outcomes) = mpsc::channel();
    state
        .app_command_sender(Caller::Socket)
        .try_send(AppCommandRequest {
            creation_receipt: None,
            invocation,
            deadline: Instant::now()
                .checked_add(Duration::from_secs(1))
                .expect("deadline"),
            cancellation: CommandCancellation::new(),
            response,
        })
        .expect("submit metadata command");
    state.update_frame(frames::idle_frame(Instant::now()));
    outcomes
        .try_recv()
        .expect("metadata command completes in one frame")
}

fn saved_metadata_listing(
    state: &mut AppState,
    binding: serde_json::Value,
) -> Vec<serde_json::Value> {
    let CommandOutcome::Success { value, .. } =
        saved_metadata_command(state, "session.saved", Vec::new(), binding)
    else {
        panic!("saved metadata listing failed");
    };
    value.as_array().expect("saved sessions").clone()
}

#[rstest]
fn current_session_checkpoint_preserves_complete_backend_history_and_saved_work(
    directory: assert_fs::TempDir,
    #[values(false, true)] historical_archives: bool,
) {
    let SavedLifecycleWorkspace {
        mut config,
        backends,
        sessions,
        state,
        ..
    } = saved_lifecycle_workspace(directory.path(), true);
    drop(state);
    config.session.output_archives = historical_archives;
    let cwd = directory.path().to_string_lossy().into_owned();
    {
        let mut reported = sessions.lock().expect("backend");
        let original = reported.first_mut().expect("original session");
        let mut window = session_on_pane("backend-id", "%first", Some(cwd.clone()))
            .windows
            .remove(0);
        window.name = "Retained window".to_owned();
        window.panes.push(MuxPaneAnchor {
            session_id: original.id.clone(),
            pane_id: Some("%second".to_owned()),
            cwd: Some(cwd.clone()),
            ..MuxPaneAnchor::default()
        });
        window.layout = Some(bootty_mux::snapshot::MuxPaneLayout::Split {
            direction: bootty_mux::snapshot::MuxPaneSplitDirection::Right,
            ratio_millis: 650,
            first: Box::new(bootty_mux::snapshot::MuxPaneLayout::Pane(
                "%first".to_owned(),
            )),
            second: Box::new(bootty_mux::snapshot::MuxPaneLayout::Pane(
                "%second".to_owned(),
            )),
        });
        original.active_window_id = Some(window.id.clone());
        original.windows = vec![window];
        drop(reported);
    }
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new_for_window(
        config.clone(),
        format!("checkpoint:{}", directory.path().display()),
        backends,
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("checkpoint host");
    let binding = listed_spaces(&mut state)[0].2.clone();
    let before = saved_metadata_listing(&mut state, binding.clone());
    state.checkpoint_sessions(123);
    while state.session_checkpoint_pending() {
        wakes
            .recv_timeout(Duration::from_secs(2))
            .expect("checkpoint worker wakes host");
        state.update_frame(frames::idle_frame(Instant::now()));
    }
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
    let (_, persisted) =
        WorkspaceRepository::open(&config.config_path).expect("persisted workspace");
    let members = persisted.spaces()[0].binding().sessions();
    let saved = members.get("saved").expect("same saved identity");
    let checkpoint = saved
        .terminal_snapshot
        .as_ref()
        .expect("current checkpoint is durable");
    assert_eq!(checkpoint.captured_at, 123);
    assert_eq!(checkpoint.session_id, "saved");
    assert_eq!(checkpoint.backend_id, "backend-id");
    assert_eq!(checkpoint.windows.len(), 1);
    let window = &checkpoint.windows[0];
    assert_eq!(window.title, "Retained window");
    assert_eq!(
        window.layout,
        sessions.lock().expect("backend")[0].windows[0].layout
    );
    assert_eq!(
        window
            .panes
            .iter()
            .map(|pane| (pane.id.as_str(), pane.cwd.as_str(), pane.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("%first", cwd.as_str(), "%first read by its backend"),
            ("%second", cwd.as_str(), "%second read by its backend")
        ]
    );
    assert!(
        members
            .get("sibling")
            .expect("other saved work")
            .terminal_snapshot
            .is_none()
    );
}

/// Commands and process output wake the host; this waiter never sleeps to poll the backend.
fn checkpoint_host_command(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    invocation: CommandInvocation,
) -> serde_json::Value {
    let outcome = checkpoint_host_outcome(state, wakes, invocation);
    let CommandOutcome::Success { value, .. } = outcome else {
        panic!("checkpoint host command failed: {outcome:?}");
    };
    value
}

fn checkpoint_host_outcome(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    invocation: CommandInvocation,
) -> CommandOutcome {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("command deadline");
    let outcomes = state
        .app_command_sender(Caller::Socket)
        .submit(invocation, deadline, CommandCancellation::new())
        .expect("submit host command");
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if let Ok(outcome) = outcomes.try_recv() {
            return outcome;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("command completion wakes host");
    }
}

fn wait_for_checkpoint_output(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    target: &serde_json::Value,
    marker: &str,
) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("output deadline");
    loop {
        let captured = checkpoint_host_command(
            state,
            wakes,
            targeted(
                "terminal.capture",
                vec!["plain".to_owned(), "history".to_owned()],
                target.clone(),
            ),
        );
        if captured["capture"]["text"]
            .as_str()
            .is_some_and(|text| text.contains(marker))
        {
            return;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("original PTY output wakes host");
    }
}

fn wait_for_restored_pane_history(
    state: &mut AppState,
    wakes: &mpsc::Receiver<()>,
    identity: &str,
    pane_count: usize,
) -> std::collections::BTreeMap<String, bootty_terminal::terminal_capture::TerminalCapture> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("restore deadline");
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        let panes = state
            .mux()
            .all_sessions()
            .iter()
            .filter(|session| session.tag.identity.as_deref() == Some(identity))
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .filter_map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        let mut captured = std::collections::BTreeMap::new();
        for pane in panes.iter().filter(|_| panes.len() == pane_count) {
            let runtime: Option<&mut dyn TerminalRuntime> =
                if state.focused_pane().as_deref() == Some(pane.as_str()) {
                    Some(state.terminal_mut())
                } else {
                    state.terminal_runtime_for_pane(pane)
                };
            let Some(runtime) = runtime else {
                break;
            };
            let response =
                match runtime.capture(bootty_terminal::terminal_capture::CaptureOptions {
                    scope: bootty_terminal::terminal_capture::CaptureScope::History,
                    format: bootty_terminal::terminal_capture::CaptureFormat::Plain,
                    ..Default::default()
                }) {
                    Ok(response) => response,
                    Err(error) if error.to_string() == "Terminal is still starting" => break,
                    Err(error) => panic!("restored pane {pane} capture failed: {error:#}"),
                };
            captured.insert(
                pane.clone(),
                response
                    .receive("restored pane history")
                    .expect("history response")
                    .expect("history captured"),
            );
        }
        if captured.len() == pane_count {
            return captured;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("every restored renderer becoming capturable wakes host");
    }
}

/// A new host receives the genuine process-local native catalogue, as after an app process restart.
struct RestartedNativeProvider(&'static str);
impl MuxBackendProvider for RestartedNativeProvider {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Native
    }
    fn command_dispatch(&self) -> MuxCommandDispatch {
        bootty_mux::native::NativeProvider.command_dispatch()
    }
    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        let key = workspace.expect("fixture workspace").join(self.0);
        bootty_mux::native::NativeProvider.build_backend(config, Some(&key))
    }
}
impl MuxAppBackendProvider for RestartedNativeProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        bootty_mux::native::NativeProvider.app_policy()
    }
    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        bootty_mux::native::NativeProvider.build_pane_policy(config)
    }
    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        bootty_mux::native::NativeProvider.capabilities(scope)
    }
}

fn saved_terminal_checkpoint(
    config: &BoottyConfig,
    identity: &str,
) -> Arc<bootty_mux::session_snapshot::SavedTerminalSession> {
    let (_, saved) =
        WorkspaceRepository::open(&config.config_path).expect("durable creation receipt");
    Arc::clone(
        saved.spaces()[0]
            .binding()
            .sessions()
            .get(identity)
            .expect("logical session")
            .terminal_snapshot
            .as_ref()
            .expect("reported creation success includes a committed checkpoint"),
    )
}

#[rstest]
fn a_window_without_native_pane_runtimes_preserves_the_prior_checkpoint(
    directory: assert_fs::TempDir,
) {
    let SavedLifecycleWorkspace {
        config,
        sessions,
        backends,
        state,
        ..
    } = saved_lifecycle_workspace(directory.path(), true);
    drop(state);
    {
        let mut reported = sessions.lock().expect("backend");
        let session = reported.first_mut().expect("saved session");
        session.windows = session_on_pane("backend-id", "%saved", None).windows;
        session.active_window_id = Some(session.windows[0].id.clone());
        drop(reported);
    }
    let mut state = app_state(config.clone(), backends);
    wait_until(&mut state, "saved pane topology arrives", |state| {
        state
            .mux()
            .all_sessions()
            .iter()
            .any(|session| !session.windows.is_empty())
            .then_some(())
    });
    state.checkpoint_sessions(123);
    wait_until(&mut state, "initial checkpoint commits", |state| {
        (!state.session_checkpoint_pending()).then_some(())
    });
    let saved = saved_terminal_checkpoint(&config, "saved");
    drop(state);
    let backends = registry(
        [Arc::new(RestoreProvider {
            topology: PaneTopology::ProcessLocal,
            ..restore_provider(
                MuxBackendKind::Tmux,
                sessions,
                Arc::new(AtomicUsize::new(0)),
            )
        })],
        [MuxBackendKind::Tmux],
    );
    let mut other = app_state(config.clone(), backends);
    other.update_frame(frames::idle_frame(Instant::now()));
    other.clear_last_error();
    other.checkpoint_sessions(456);
    assert_eq!(other.last_error(), None);
    assert!(!other.session_checkpoint_pending());
    assert_eq!(saved_terminal_checkpoint(&config, "saved"), saved);
}

#[rstest]
#[case::active(false)]
#[case::archived(true)]
fn normal_native_start_restores_only_selected_saved_session_after_its_process_ended(
    directory: assert_fs::TempDir,
    #[case] archived: bool,
) {
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RestartedNativeProvider("fixture-first-process"))],
            [MuxBackendKind::Native],
        )
        .expect("native registry"),
    );
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.output_archives = false;
    let window_key = format!("cold-checkpoint:{}", directory.path().display());
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new_for_window(
        config.clone(),
        window_key.clone(),
        Arc::clone(&backends),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("first host");
    assert!(state.open_new_session_dialog_from_ui());
    let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("new session draft");
    };
    let draft = dialog.draft().expect("new terminal draft");
    assert!(draft.command.is_empty() && draft.prompt.is_empty());
    assert!(
        state.mux().all_sessions().is_empty(),
        "opening a new draft does not spawn a shell"
    );
    state.apply_picker_event(NewSessionPickerEvent::Close);
    let spaces = checkpoint_host_command(
        &mut state,
        &wakes,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
    );
    let binding = spaces[0]["target"].clone();
    let cwd = directory.path().to_string_lossy().into_owned();
    let marker = directory.path().join("original-command-count");
    let changed_cwd = directory.child("changed cwd % # ?");
    changed_cwd
        .create_dir_all()
        .expect("current working directory");
    let changed_uri =
        url::Url::from_file_path(changed_cwd.path()).expect("encoded OSC 7 directory");
    let original = checkpoint_host_command(&mut state, &wakes, targeted("session.create", vec![
        "selected".to_owned(), cwd.clone(), serde_json::json!([
            "/bin/sh", "-c", "printf x >> \"$1\"; cd \"$2\"; printf '\\033]7;%s\\007' \"$3\"; printf 'retained cold history\\n'; exec sleep 60",
            "original", marker.to_string_lossy(), changed_cwd.path().to_string_lossy(), changed_uri.as_str(),
        ]).to_string(), "selected-task".to_owned(), "Retained task title".to_owned(),
    ], binding.clone()));
    let initial = saved_terminal_checkpoint(&config, "selected-task");
    assert_eq!(initial.windows.len(), 1);
    assert_eq!(
        initial.windows[0].panes.len(),
        1,
        "first creation receipt is already durable"
    );
    // The original command's real PTY output must be in the frame before it is checkpointed.
    wait_for_checkpoint_output(
        &mut state,
        &wakes,
        &original["terminal"],
        "retained cold history",
    );
    let idle_argv = serde_json::json!(["/bin/sh", "-c", "exec sleep 60"]).to_string();
    checkpoint_host_command(
        &mut state,
        &wakes,
        targeted(
            "terminal.create_pane",
            vec!["right".to_owned(), idle_argv.clone(), cwd.clone()],
            original["terminal"].clone(),
        ),
    );
    assert_eq!(
        saved_terminal_checkpoint(&config, "selected-task").windows[0]
            .panes
            .len(),
        2,
        "pane success waits for its complete session checkpoint"
    );
    checkpoint_host_command(
        &mut state,
        &wakes,
        targeted(
            "terminal.create_tab",
            vec![idle_argv.clone(), cwd.clone()],
            original["created"].clone(),
        ),
    );
    checkpoint_host_command(
        &mut state,
        &wakes,
        targeted(
            "session.create",
            vec![
                "other".to_owned(),
                cwd.clone(),
                idle_argv,
                "other-task".to_owned(),
                "Other retained task".to_owned(),
            ],
            binding.clone(),
        ),
    );
    state.activate_session_from_ui("selected");
    let (_, saved) = WorkspaceRepository::open(&config.config_path).expect("durable checkpoint");
    let chosen = saved.spaces()[0]
        .binding()
        .sessions()
        .get("selected-task")
        .expect("chosen logical task");
    let retained = Arc::clone(
        chosen
            .terminal_snapshot
            .as_ref()
            .expect("checkpoint committed by tab creation"),
    );
    assert_eq!(retained.windows.len(), 2);
    assert_eq!(
        retained
            .windows
            .iter()
            .map(|window| window.panes.len())
            .sum::<usize>(),
        3
    );
    assert!(
        retained
            .windows
            .iter()
            .any(|window| window.layout.is_some())
    );
    let changed_pane = retained
        .windows
        .iter()
        .flat_map(|window| &window.panes)
        .find(|pane| pane.text.contains("retained cold history"))
        .expect("original output captured");
    assert_eq!(changed_pane.cwd, changed_cwd.path().to_string_lossy());
    assert_eq!(
        chosen.cwd, cwd,
        "logical task project directory remains independently owned"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("original execution marker"),
        "x"
    );
    if archived {
        for command in ["session.settle", "session.archive"] {
            checkpoint_host_command(
                &mut state,
                &wakes,
                targeted(command, vec!["selected-task".to_owned()], binding.clone()),
            );
        }
    }
    drop(state);

    let (wake, wakes) = mpsc::channel();
    let cold_backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RestartedNativeProvider(
                "fixture-restored-process",
            ))],
            [MuxBackendKind::Native],
        )
        .expect("cold native registry"),
    );
    let mut restored = AppState::new_for_window(
        config.clone(),
        window_key,
        cold_backends,
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("cold host");
    if archived {
        restored.update_frame(frames::idle_frame(Instant::now()));
        assert!(
            restored.mux().all_sessions().is_empty(),
            "Archived tasks stay dormant on launch"
        );
        let spaces = checkpoint_host_command(
            &mut restored,
            &wakes,
            CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
        );
        checkpoint_host_command(
            &mut restored,
            &wakes,
            targeted(
                "session.unarchive",
                vec!["selected-task".to_owned()],
                spaces[0]["target"].clone(),
            ),
        );
        checkpoint_host_command(
            &mut restored,
            &wakes,
            CommandInvocation::new(
                "ui.sidebar.activate_session",
                Vec::new(),
                Caller::Keybinding,
            ),
        );
    }
    let captured = wait_for_restored_pane_history(&mut restored, &wakes, "selected-task", 3);
    assert!(
        !restored
            .mux()
            .all_sessions()
            .iter()
            .any(|session| session.tag.identity.as_deref() == Some("other-task")),
        "normal launch does not eagerly spawn other retained tasks"
    );
    let actual = restored
        .mux()
        .all_sessions()
        .iter()
        .find(|session| session.tag.identity.as_deref() == Some("selected-task"))
        .expect("same logical session restored");
    assert_eq!(actual.windows.len(), retained.windows.len());
    for (window, expected) in actual.windows.iter().zip(&retained.windows) {
        assert_eq!(window.name, expected.title);
        assert_eq!(
            window
                .panes
                .iter()
                .filter_map(|pane| pane.cwd.as_deref())
                .collect::<Vec<_>>(),
            expected
                .panes
                .iter()
                .map(|pane| pane.cwd.as_str())
                .collect::<Vec<_>>()
        );
    }
    let output_pane = actual
        .windows
        .iter()
        .zip(&retained.windows)
        .flat_map(|(window, saved)| window.panes.iter().zip(&saved.panes))
        .find(|(_, saved)| saved.text.contains("retained cold history"))
        .and_then(|(pane, _)| pane.pane_id.clone())
        .expect("exact restored output pane");
    let capture = captured
        .get(&output_pane)
        .expect("exact restored hidden pane was captured");
    assert!(
        capture.text.contains("retained cold history"),
        "{}",
        capture.text
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("original command was not replayed"),
        "x"
    );
    while restored.session_checkpoint_pending() {
        wakes
            .recv_timeout(Duration::from_secs(5))
            .expect("restored creation receipt wakes host");
        restored.update_frame(frames::idle_frame(Instant::now()));
    }
    let (_, current_state) =
        WorkspaceRepository::open(&config.config_path).expect("restored current state");
    let current_checkpoint = current_state.spaces()[0]
        .binding()
        .sessions()
        .get("selected-task")
        .and_then(|saved| saved.terminal_snapshot.as_ref())
        .expect("restored checkpoint");
    assert_eq!(
        current_checkpoint
            .windows
            .iter()
            .map(|window| (&window.id, &window.layout, &window.focused_pane_id))
            .collect::<Vec<_>>(),
        retained
            .windows
            .iter()
            .map(|window| (&window.id, &window.layout, &window.focused_pane_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        current_state.spaces()[0]
            .binding()
            .sessions()
            .get("selected-task")
            .expect("retained title")
            .label(),
        "Retained task title"
    );
    assert!(
        current_state.spaces()[0]
            .binding()
            .sessions()
            .get("other-task")
            .is_some(),
        "other saved work remains retained"
    );
    let current = checkpoint_host_command(
        &mut restored,
        &wakes,
        CommandInvocation::new(
            "resource.current",
            vec!["session".to_owned()],
            Caller::Socket,
        ),
    );
    let mut close = targeted("session.close", Vec::new(), current["target"].clone());
    close.confirmation = Some(close.confirmation());
    checkpoint_host_command(&mut restored, &wakes, close);
}

#[rstest]
#[case("https://localhost/tmp", "not a file URI")]
#[case("file://foreign.invalid/tmp", "does not name this host")]
fn unsupported_osc7_directory_preserves_the_checkpoint_and_live_process(
    directory: assert_fs::TempDir,
    #[case] reported_uri: &str,
    #[case] expected_error: &str,
) {
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(RestartedNativeProvider("fixture-first-process"))],
            [MuxBackendKind::Native],
        )
        .expect("native registry"),
    );
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.session.output_archives = false;
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new_for_window(
        config.clone(),
        format!("unsupported-cwd:{}", directory.path().display()),
        backends,
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("native host");
    let spaces = checkpoint_host_command(
        &mut state,
        &wakes,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
    );
    let cwd = directory.path().to_string_lossy().into_owned();
    let valid_uri = url::Url::from_file_path(directory.path()).expect("local directory URI");
    let created = checkpoint_host_command(&mut state, &wakes, targeted("session.create", vec![
        "retained".to_owned(), cwd.clone(), serde_json::json!([
            "/bin/sh", "-c",
            "read -r trigger; printf '\\033]7;%s\\007' \"$1\"; printf 'unsupported directory received\\n'; read -r trigger; printf '\\033]7;%s\\007' \"$2\"; printf 'local directory restored\\n'; exec sleep 60",
            "original", reported_uri, valid_uri.as_str(),
        ]).to_string(), "retained-task".to_owned(), "Retained".to_owned(),
    ], spaces[0]["target"].clone()));
    let before = saved_terminal_checkpoint(&config, "retained-task");
    checkpoint_host_command(
        &mut state,
        &wakes,
        targeted(
            "terminal.write",
            vec!["go\n".to_owned()],
            created["terminal"].clone(),
        ),
    );
    wait_for_checkpoint_output(
        &mut state,
        &wakes,
        &created["terminal"],
        "unsupported directory received",
    );
    let mut close = targeted("session.close", Vec::new(), created["created"].clone());
    close.confirmation = Some(close.confirmation());
    let outcome = checkpoint_host_outcome(&mut state, &wakes, close.clone());
    assert!(
        matches!(outcome, CommandOutcome::Failed { ref code, ref message }
        if code == "session_checkpoint_failed" && message.contains(expected_error)),
        "{outcome:?}"
    );
    assert_eq!(
        saved_terminal_checkpoint(&config, "retained-task"),
        before,
        "an unsupported cwd never replaces the complete prior checkpoint"
    );
    assert_eq!(
        state.mux().all_sessions().len(),
        1,
        "failed close retains the actual process"
    );
    checkpoint_host_command(
        &mut state,
        &wakes,
        targeted(
            "terminal.write",
            vec!["repair\n".to_owned()],
            created["terminal"].clone(),
        ),
    );
    wait_for_checkpoint_output(
        &mut state,
        &wakes,
        &created["terminal"],
        "local directory restored",
    );
    checkpoint_host_command(&mut state, &wakes, close);
    assert_eq!(state.mux().all_sessions(), Vec::<MuxSession>::new());
    assert_eq!(
        saved_terminal_checkpoint(&config, "retained-task").windows[0].panes[0].cwd,
        cwd
    );
}

struct SavedLifecycleWorkspace {
    config: BoottyConfig,
    backends: Arc<MuxBackendRegistry>,
    sessions: Arc<Mutex<Vec<MuxSession>>>,
    creates: Arc<AtomicUsize>,
    state: AppState,
}

#[rstest]
fn closing_saved_work_checkpoints_before_ending_its_process_and_honors_cancellation(
    directory: assert_fs::TempDir,
    #[values(false, true)] cancel_close: bool,
) {
    let SavedLifecycleWorkspace {
        mut config,
        backends,
        sessions,
        state,
        ..
    } = saved_lifecycle_workspace(directory.path(), true);
    drop(state);
    config.session.output_archives = false;
    let cwd = directory.path().to_string_lossy().into_owned();
    {
        let mut reported = sessions.lock().expect("backend");
        let original = reported.first_mut().expect("original");
        original.windows = session_on_pane("backend-id", "%retained", Some(cwd)).windows;
        original.active_window_id = Some("backend-id-window".to_owned());
        drop(reported);
    }
    let (wake, wakes) = mpsc::channel();
    let mut state = AppState::new_for_window(
        config.clone(),
        format!("checkpoint-close:{}", directory.path().display()),
        backends,
        Arc::new(move || {
            let _ = wake.send(());
        }),
        None,
        None,
    )
    .expect("checkpoint host");
    let (_, saved) = WorkspaceRepository::open(&config.config_path).expect("logical owner");
    let owner_scope = saved
        .spaces()
        .iter()
        .find(|space| space.binding().sessions().get("saved").is_some())
        .expect("saved task owner")
        .id();
    state.activate_space_from_ui(owner_scope);
    assert_eq!(state.active_space_id(), owner_scope);
    let observed_deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("backend deadline");
    loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if state
            .mux()
            .sessions()
            .iter()
            .any(|session| session.id == "backend-id")
        {
            break;
        }
        wakes
            .recv_timeout(observed_deadline.saturating_duration_since(Instant::now()))
            .expect("exact backend session observation wakes host");
    }
    state.activate_session_from_ui("backend-id");
    assert_eq!(state.mux().selected_session(), Some("backend-id"));
    let value = checkpoint_host_command(
        &mut state,
        &wakes,
        CommandInvocation::new(
            "resource.current",
            vec!["session".to_owned()],
            Caller::Socket,
        ),
    );
    let mut close = targeted("session.close", Vec::new(), value["target"].clone());
    close.confirmation = Some(close.confirmation());
    let cancellation = CommandCancellation::new();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("deadline");
    let response = state
        .app_command_sender(Caller::Socket)
        .submit(close.clone(), deadline, cancellation.clone())
        .expect("close submission");
    let duplicate = state
        .app_command_sender(Caller::Socket)
        .submit(close, deadline, CommandCancellation::new())
        .expect("duplicate close submission");
    state.update_frame(frames::idle_frame(Instant::now()));
    assert!(
        matches!(duplicate.try_recv().expect("duplicate close is rejected immediately"),
        CommandOutcome::Unavailable { message } if message == "This session is already being closed")
    );
    assert_eq!(
        sessions.lock().expect("backend").len(),
        1,
        "capture must precede process close"
    );
    if cancel_close {
        assert!(
            cancellation.cancel(),
            "close remains cancellable while capturing"
        );
    }
    let outcome = loop {
        state.update_frame(frames::idle_frame(Instant::now()));
        if let Ok(outcome) = response.try_recv() {
            break outcome;
        }
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("checkpoint completion wakes host");
    };
    if cancel_close {
        assert_eq!(outcome, CommandOutcome::cancelled());
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    assert_eq!(
        sessions.lock().expect("backend").len(),
        usize::from(cancel_close)
    );
    while state.session_checkpoint_pending() {
        wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("checkpoint wake");
        state.update_frame(frames::idle_frame(Instant::now()));
    }
    let (_, persisted) =
        WorkspaceRepository::open(&config.config_path).expect("persisted workspace");
    let saved = persisted.spaces()[0]
        .binding()
        .sessions()
        .get("saved")
        .expect("same logical work remains saved");
    let checkpoint = saved
        .terminal_snapshot
        .as_ref()
        .expect("accepted current checkpoint");
    assert_eq!(
        checkpoint.windows[0].panes[0].text,
        "%retained read by its backend"
    );
    assert_eq!(saved.label(), "shared-title");
    // A deliberate close remains detached in this run instead of triggering startup restoration.
    state.update_frame(frames::idle_frame(Instant::now()));
    assert_eq!(
        sessions.lock().expect("backend").len(),
        usize::from(cancel_close)
    );
}

fn saved_lifecycle_workspace(directory: &Path, attached: bool) -> SavedLifecycleWorkspace {
    let config_path = directory.join("config.toml");
    let cwd = directory.to_string_lossy().into_owned();
    let (mut repository, space) = claim_first_space(&config_path, "saved", "shared-title", &cwd);
    let mut membership = space.binding().sessions().clone();
    // The returned Space predates the claim; persist both exact identities explicitly.
    membership.claim(claimed_session("saved", "shared-title", &cwd));
    membership.claim(claimed_session("sibling", "shared-title", &cwd));
    repository
        .commit_binding_state(space.id(), &membership)
        .expect("persist sibling");
    create_space(
        &mut repository,
        "Other",
        "2",
        [1, 2, 3],
        SpaceMuxOverride::default(),
    );
    drop(repository);
    let sessions = Arc::new(Mutex::new(if attached {
        vec![mux_session(
            "backend-id",
            cwd,
            MuxSessionTag {
                identity: Some("saved".to_owned()),
                space: Some(space.remote_id().to_owned()),
            },
            true,
        )]
    } else {
        Vec::new()
    }));
    let creates = Arc::<AtomicUsize>::default();
    let backends = registry(
        [Arc::new(restore_provider(
            MuxBackendKind::Tmux,
            Arc::clone(&sessions),
            Arc::clone(&creates),
        ))],
        [MuxBackendKind::Tmux],
    );
    let config = test_config::config(config_path, MultiplexerBackendConfig::Tmux);
    let state = app_state(config.clone(), Arc::clone(&backends));
    SavedLifecycleWorkspace {
        config,
        backends,
        sessions,
        creates,
        state,
    }
}

fn change_saved_state(
    state: &mut AppState,
    binding: &serde_json::Value,
    changes: &[(&str, Option<&str>)],
) {
    for (command, extra) in changes {
        let mut arguments = vec!["saved".to_owned()];
        arguments.extend(extra.map(str::to_owned));
        let outcome = saved_metadata_command(state, command, arguments, binding.clone());
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{command}: {outcome:?}"
        );
    }
}

#[rstest]
#[case::surviving_original(true, true)]
#[case::missing_original_window(true, false)]
fn shared_reopen_uses_only_the_original_and_never_replaces_saved_work(
    directory: assert_fs::TempDir,
    #[case] attached: bool,
    #[case] has_window: bool,
) {
    let SavedLifecycleWorkspace {
        config,
        backends,
        sessions,
        creates,
        state,
    } = saved_lifecycle_workspace(directory.path(), attached);
    if has_window {
        let mut reported = sessions.lock().expect("backend");
        let original = reported.first_mut().expect("original session");
        original.windows = session_with_pane("backend-id").windows;
        original.active_window_id = Some("backend-id-window".to_owned());
        drop(reported);
    }
    // Same name and directory cannot substitute for the missing exact identity.
    let mut foreign = session_with_pane("foreign-id");
    foreign.name = "shared-title".to_owned();
    foreign.anchor.cwd = Some(directory.path().to_string_lossy().into_owned());
    sessions.lock().expect("backend").push(foreign);
    drop(state);
    let mut state = app_state(config.clone(), Arc::clone(&backends));
    let spaces = listed_spaces(&mut state);
    let binding = spaces[0].2.clone();
    let original_scope = state.mux_scope();
    let before = saved_metadata_listing(&mut state, binding.clone());
    let backend_before = sessions.lock().expect("backend").clone();
    let other = SpaceId::from_persistence(spaces[1].1.parse().expect("Other scope"));
    assert!(state.activate_space_from_ui(other));
    let outcome = saved_metadata_command(
        &mut state,
        "session.reopen",
        vec!["saved".to_owned()],
        binding,
    );
    if has_window {
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(state.mux_scope(), original_scope);
        assert_eq!(state.mux().selected_session(), Some("backend-id"));
        assert_eq!(state.mux().selected_window(), Some("backend-id-window"));
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Unavailable { .. }),
            "{outcome:?}"
        );
        assert_eq!(state.mux_scope(), other);
    }
    let binding = listed_spaces(&mut state)[0].2.clone();
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
    assert_eq!(*sessions.lock().expect("backend"), backend_before);
    assert_eq!(creates.load(Ordering::SeqCst), 0);
    drop(state);
    let mut state = app_state(config, backends);
    let binding = listed_spaces(&mut state)[0].2.clone();
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
}

#[rstest]
#[case::attached(true)]
#[case::detached(false)]
fn saved_lifecycle_is_durable_and_independent_of_backend_attachments(
    directory: assert_fs::TempDir,
    #[case] attached: bool,
    #[values(false, true)] inactive: bool,
) {
    let SavedLifecycleWorkspace {
        config,
        backends,
        sessions,
        creates,
        mut state,
    } = saved_lifecycle_workspace(directory.path(), attached);
    let spaces = listed_spaces(&mut state);
    let binding = spaces[0].2.clone();
    let sibling_before = saved_metadata_listing(&mut state, binding.clone())[1].clone();
    let original_scope = state.mux_scope();
    if inactive {
        let other = SpaceId::from_persistence(spaces[1].1.parse().expect("Other scope"));
        assert!(state.activate_space_from_ui(other));
    }
    let selected_scope = state.mux_scope();
    change_saved_state(
        &mut state,
        &binding,
        &[
            ("session.settle", None),
            ("session.snooze", Some("100")),
            ("session.hide", None),
            ("session.archive", None),
        ],
    );
    assert_eq!(state.mux_scope(), selected_scope);
    let deleted = saved_metadata_listing(&mut state, binding.clone());
    assert_eq!(deleted[1], sibling_before);
    assert_eq!(
        deleted[0]["state"],
        serde_json::json!({"lifecycle":"settled", "pinned":false, "archived":true, "deleted":false, "hidden":true, "snoozed_until":100, "last_activity_at":null})
    );
    assert_eq!(deleted[0]["attachment"].is_string(), attached);
    // A Binding for another Space cannot mutate the identity despite sharing a backend.
    let foreign = saved_metadata_command(
        &mut state,
        "session.restore",
        vec!["saved".to_owned()],
        spaces[1].2.clone(),
    );
    assert!(
        matches!(foreign, CommandOutcome::Failed { .. }),
        "{foreign:?}"
    );
    assert_eq!(state.mux_scope(), selected_scope);
    if inactive {
        assert!(state.activate_space_from_ui(original_scope));
    }
    drop(state);
    let mut state = app_state(config, backends);
    let binding = listed_spaces(&mut state)[0].2.clone();
    assert_eq!(saved_metadata_listing(&mut state, binding.clone()), deleted);
    change_saved_state(&mut state, &binding, &[("session.unarchive", None)]);
    let restored = saved_metadata_listing(&mut state, binding);
    assert_eq!(
        restored[0]["state"],
        serde_json::json!({"lifecycle":"settled", "pinned":false, "archived":false, "deleted":false, "hidden":true, "snoozed_until":100, "last_activity_at":null})
    );
    assert_eq!(restored[1], sibling_before);
    assert_eq!(creates.load(Ordering::SeqCst), 0);
    assert_eq!(
        sessions.lock().expect("backend").len(),
        usize::from(attached)
    );
}

#[rstest]
#[case("session.archive")]
#[case("session.pin")]
#[case("session.settle")]
#[case("session.activate")]
#[case("session.snooze")]
#[case("session.unsnooze")]
#[case("session.hide")]
#[case("session.show")]
#[case("session.delete")]
fn failed_lifecycle_write_keeps_live_and_reloaded_state(
    directory: assert_fs::TempDir,
    #[case] command: &str,
) {
    let config_path = directory.path().join("config.toml");
    claim_first_space(
        &config_path,
        "saved",
        "purpose",
        &directory.path().to_string_lossy(),
    );
    let (backends, creates) = backends_after_empty_restore();
    let config = test_config::config(config_path, MultiplexerBackendConfig::Tmux);
    let mut state = app_state(config.clone(), Arc::clone(&backends));
    let binding = listed_spaces(&mut state)[0].2.clone();
    if command == "session.activate" {
        change_saved_state(&mut state, &binding, &[("session.settle", None)]);
    } else if command == "session.unsnooze" {
        change_saved_state(&mut state, &binding, &[("session.snooze", Some("100"))]);
    } else if command == "session.show" {
        change_saved_state(&mut state, &binding, &[("session.hide", None)]);
    }
    let before = saved_metadata_listing(&mut state, binding.clone());
    let database =
        Connection::open(directory.path().join("session-order.sqlite3")).expect("database");
    database.execute_batch("CREATE TRIGGER reject_lifecycle BEFORE DELETE ON workspace_sessions BEGIN SELECT RAISE(ABORT, 'injected state write failure'); END;").expect("failure boundary");
    let mut arguments = vec!["saved".to_owned()];
    if command == "session.snooze" {
        arguments.push("100".to_owned());
    }
    let outcome = saved_metadata_command(&mut state, command, arguments, binding.clone());
    assert!(
        matches!(&outcome, CommandOutcome::Failed { code, .. } if code == "session_state_failed"),
        "{outcome:?}"
    );
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
    database
        .execute_batch("DROP TRIGGER reject_lifecycle;")
        .expect("clear failure");
    drop(state);
    let mut restarted = app_state(config, backends);
    let binding = listed_spaces(&mut restarted)[0].2.clone();
    assert_eq!(saved_metadata_listing(&mut restarted, binding), before);
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}

#[rstest]
#[case::attached(true)]
#[case::detached(false)]
fn hide_and_show_preserve_saved_work_and_snooze_across_restart(
    directory: assert_fs::TempDir,
    #[case] attached: bool,
) {
    let SavedLifecycleWorkspace {
        config,
        backends,
        sessions,
        creates,
        mut state,
    } = saved_lifecycle_workspace(directory.path(), attached);
    let binding = listed_spaces(&mut state)[0].2.clone();
    change_saved_state(
        &mut state,
        &binding,
        &[("session.settle", None), ("session.snooze", Some("100"))],
    );
    let before = saved_metadata_listing(&mut state, binding.clone());
    let backend_before = sessions.lock().expect("backend").clone();
    change_saved_state(&mut state, &binding, &[("session.hide", None)]);
    let mut hidden = before.clone();
    hidden[0]["state"]["hidden"] = true.into();
    assert_eq!(saved_metadata_listing(&mut state, binding), hidden);
    drop(state);
    let mut state = app_state(config.clone(), Arc::clone(&backends));
    let binding = listed_spaces(&mut state)[0].2.clone();
    assert_eq!(saved_metadata_listing(&mut state, binding.clone()), hidden);
    change_saved_state(&mut state, &binding, &[("session.show", None)]);
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
    drop(state);
    let mut state = app_state(config, backends);
    let binding = listed_spaces(&mut state)[0].2.clone();
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
    assert_eq!(*sessions.lock().expect("backend"), backend_before);
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}

#[rstest]
#[case::attached(true)]
#[case::detached(false)]
fn shared_pin_commands_preserve_exact_saved_work_across_restore(
    directory: assert_fs::TempDir,
    #[case] attached: bool,
) {
    let SavedLifecycleWorkspace {
        config,
        backends,
        creates,
        mut state,
        ..
    } = saved_lifecycle_workspace(directory.path(), attached);
    let spaces = listed_spaces(&mut state);
    let binding = spaces[0].2.clone();
    let original_scope = state.mux_scope();
    let before = saved_metadata_listing(&mut state, binding.clone());
    change_saved_state(
        &mut state,
        &binding,
        &[("session.settle", None), ("session.snooze", Some("100"))],
    );
    assert!(state.activate_space_from_ui(SpaceId::from_persistence(spaces[1].1.parse().unwrap())));
    let selected_scope = state.mux_scope();
    change_saved_state(&mut state, &binding, &[("session.pin", None)]);
    let pinned = saved_metadata_listing(&mut state, binding.clone());
    assert_eq!(pinned[0]["state"]["pinned"], true);
    assert_eq!(pinned[0]["state"]["lifecycle"], "active");
    assert_eq!(pinned[0]["state"]["snoozed_until"], serde_json::Value::Null);
    for field in ["identity", "title", "cwd", "attachment"] {
        assert_eq!(pinned[0][field], before[0][field], "{field}");
    }
    assert_eq!(pinned[1], before[1]);
    assert_eq!(state.mux_scope(), selected_scope);
    let repeated = saved_metadata_command(
        &mut state,
        "session.pin",
        vec!["saved".to_owned()],
        binding.clone(),
    );
    assert!(matches!(repeated, CommandOutcome::Success { value, .. } if value["changed"] == false));
    let foreign = saved_metadata_command(
        &mut state,
        "session.pin",
        vec!["saved".to_owned()],
        spaces[1].2.clone(),
    );
    assert!(matches!(foreign, CommandOutcome::Failed { .. }));
    let mut stale: bootty_control::CommandTarget = serde_json::from_value(binding.clone()).unwrap();
    stale.generation = stale.generation.checked_add(1).unwrap();
    let rejected = saved_metadata_command(
        &mut state,
        "session.unpin",
        vec!["saved".to_owned()],
        serde_json::to_value(stale).unwrap(),
    );
    assert!(matches!(rejected, CommandOutcome::StaleTarget { .. }));
    assert_eq!(saved_metadata_listing(&mut state, binding.clone()), pinned);
    change_saved_state(&mut state, &binding, &[("session.archive", None)]);
    assert_eq!(
        saved_metadata_listing(&mut state, binding.clone())[0]["state"]["pinned"],
        true
    );
    change_saved_state(&mut state, &binding, &[("session.unarchive", None)]);
    assert_eq!(saved_metadata_listing(&mut state, binding.clone()), pinned);
    change_saved_state(&mut state, &binding, &[("session.unpin", None)]);
    assert_eq!(
        saved_metadata_listing(&mut state, binding.clone())[0]["state"]["pinned"],
        false
    );
    change_saved_state(&mut state, &binding, &[("session.pin", None)]);
    assert!(state.activate_space_from_ui(original_scope));
    drop(state);
    let mut restarted = app_state(config, backends);
    let binding = listed_spaces(&mut restarted)[0].2.clone();
    assert_eq!(
        saved_metadata_listing(&mut restarted, binding.clone()),
        pinned
    );
    change_saved_state(&mut restarted, &binding, &[("session.settle", None)]);
    let settled = saved_metadata_listing(&mut restarted, binding);
    assert_eq!(settled[0]["state"]["pinned"], false);
    assert_eq!(settled[0]["state"]["lifecycle"], "settled");
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}

#[rstest]
#[case("-1")]
#[case("tomorrow")]
#[case("9223372036854775808")]
fn invalid_snooze_deadlines_leave_saved_state_unchanged(
    directory: assert_fs::TempDir,
    #[case] until: &str,
) {
    let config_path = directory.path().join("config.toml");
    claim_first_space(
        &config_path,
        "saved",
        "purpose",
        &directory.path().to_string_lossy(),
    );
    let (backends, creates) = backends_after_empty_restore();
    let mut state = app_state(
        test_config::config(config_path, MultiplexerBackendConfig::Tmux),
        backends,
    );
    let binding = listed_spaces(&mut state)[0].2.clone();
    let before = saved_metadata_listing(&mut state, binding.clone());
    let outcome = saved_metadata_command(
        &mut state,
        "session.snooze",
        vec!["saved".to_owned(), until.to_owned()],
        binding.clone(),
    );
    assert!(matches!(outcome, CommandOutcome::Failed { code, .. } if code == "invalid_arguments"));
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}

#[rstest]
#[case("session.pin")]
#[case("session.unpin")]
fn pending_terminal_operation_blocks_pin_metadata_changes(
    directory: assert_fs::TempDir,
    #[case] command: &str,
) {
    let mut state = saved_lifecycle_workspace(directory.path(), false).state;
    let binding = listed_spaces(&mut state)[0].2.clone();
    let before = saved_metadata_listing(&mut state, binding.clone());
    let (mut repository, _) =
        WorkspaceRepository::open(&directory.path().join("config.toml")).expect("saved repository");
    repository
        .begin_binding_membership_mutation(
            state.mux_scope(),
            &BindingMembershipMutation::Rename {
                identity: "saved".to_owned(),
                old_name: "shared-title".to_owned(),
                new_name: "pending-title".to_owned(),
                display_name: "Purpose".to_owned(),
                explicit: true,
            },
        )
        .expect("persist real pending operation");
    let rejected = saved_metadata_command(
        &mut state,
        command,
        vec!["saved".to_owned()],
        binding.clone(),
    );
    assert!(
        matches!(rejected, CommandOutcome::Failed { code, .. } if code == "session_state_failed")
    );
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
}

fn native_saved_retry_state(directory: &Path) -> AppState {
    let backends = Arc::new(
        MuxBackendRegistry::from_app_providers(
            [Arc::new(MixedProvider::Native(
                bootty_mux::native::NativeProvider,
            ))],
            [MuxBackendKind::Native],
        )
        .expect("native backend registry"),
    );
    app_state(
        test_config::config(
            directory.join("config.toml"),
            MultiplexerBackendConfig::Native,
        ),
        backends,
    )
}

fn retained_create(
    binding: &serde_json::Value,
    identity: &str,
    cwd: &str,
    argv: &serde_json::Value,
    title: &str,
) -> CommandInvocation {
    targeted(
        "session.create",
        vec![
            "retained-task".to_owned(),
            cwd.to_owned(),
            argv.to_string(),
            identity.to_owned(),
            title.to_owned(),
        ],
        binding.clone(),
    )
}

#[rstest]
fn failed_native_launch_retries_one_saved_identity_and_preserves_its_content(
    directory: assert_fs::TempDir,
) {
    let mut state = native_saved_retry_state(directory.path());
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let cwd = directory.path().to_string_lossy().into_owned();
    let identity = bootty_mux::snapshot::new_session_identity();
    let count = saved_metadata_listing(&mut state, home.clone()).len();
    let missing = retained_create(
        &home,
        &identity,
        &cwd,
        &serde_json::json!(["bootty-no-such-program"]),
        "Fix the checkout",
    );
    for _ in 0..2 {
        let failed = submit_and_wait(&mut state, missing.clone());
        assert!(
            matches!(&failed, CommandOutcome::Failed {code, ..} if code == "session_start_failed"),
            "{failed:?}"
        );
        let saved = saved_metadata_listing(&mut state, home.clone());
        assert_eq!(saved.len(), count.saturating_add(1));
        let saved = saved
            .iter()
            .find(|saved| saved["identity"] == identity)
            .expect("retained identity");
        assert_eq!(saved["title"], "Fix the checkout");
        assert_eq!(saved["cwd"], cwd);
        assert!(saved["attachment"].is_null());
    }
    for (command, extra) in [
        ("session.settle", None),
        ("session.hide", None),
        ("session.snooze", Some("9000")),
    ] {
        let outcome = saved_metadata_command(
            &mut state,
            command,
            std::iter::once(identity.clone())
                .chain(extra.map(str::to_owned))
                .collect(),
            home.clone(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    }
    let before = saved_metadata_listing(&mut state, home.clone())
        .into_iter()
        .find(|saved| saved["identity"] == identity)
        .unwrap();
    let success = submit_and_wait(
        &mut state,
        retained_create(
            &home,
            &identity,
            &cwd,
            &serde_json::json!(["/bin/sh", "-c", "exec sleep 60"]),
            "Ignored replacement title",
        ),
    );
    assert!(
        matches!(success, CommandOutcome::Success { .. }),
        "{success:?}"
    );
    let saved = saved_metadata_listing(&mut state, home.clone());
    assert_eq!(saved.len(), count.saturating_add(1));
    let saved = saved
        .iter()
        .find(|saved| saved["identity"] == identity)
        .unwrap();
    for field in ["identity", "title", "cwd", "state"] {
        assert_eq!(saved[field], before[field], "{field}");
    }
    assert!(saved["attachment"].is_string());
    let repeated = submit_and_wait(
        &mut state,
        retained_create(
            &home,
            &identity,
            &cwd,
            &serde_json::json!([]),
            "Fix the checkout",
        ),
    );
    assert!(
        matches!(repeated, CommandOutcome::Unavailable { .. }),
        "{repeated:?}"
    );
    assert_eq!(
        saved_metadata_listing(&mut state, home).len(),
        count.saturating_add(1)
    );
}

#[rstest]
#[case::other_directory("session.settle", true)]
fn saved_retry_rejects_changed_directory_without_replacing_work(
    directory: assert_fs::TempDir,
    #[case] command: &str,
    #[case] change_directory: bool,
) {
    let mut state = saved_lifecycle_workspace(directory.path(), false).state;
    let (_, _, home) = listed_spaces(&mut state).remove(0);
    let outcome =
        saved_metadata_command(&mut state, command, vec!["saved".to_owned()], home.clone());
    assert!(matches!(outcome, CommandOutcome::Success { .. }));
    let before = saved_metadata_listing(&mut state, home.clone());
    let cwd = if change_directory {
        "/another/project".to_owned()
    } else {
        directory.path().to_string_lossy().into_owned()
    };
    let rejected = submit_and_wait(
        &mut state,
        retained_create(&home, "saved", &cwd, &serde_json::json!([]), "Replacement"),
    );
    assert!(
        matches!(
            rejected,
            CommandOutcome::Unavailable { .. } | CommandOutcome::Failed { .. }
        ),
        "{rejected:?}"
    );
    assert_eq!(saved_metadata_listing(&mut state, home), before);
}

#[rstest]
fn saved_retry_cannot_claim_an_identity_owned_by_another_space(directory: assert_fs::TempDir) {
    let mut state = saved_lifecycle_workspace(directory.path(), false).state;
    let mut spaces = listed_spaces(&mut state);
    let (_, _, home) = spaces.remove(0);
    let (_, _, other) = spaces.remove(0);
    let before = saved_metadata_listing(&mut state, home.clone());
    let rejected = submit_and_wait(
        &mut state,
        retained_create(
            &other,
            "saved",
            &directory.path().to_string_lossy(),
            &serde_json::json!([]),
            "Replacement",
        ),
    );
    assert!(
        matches!(rejected, CommandOutcome::Unavailable { .. }),
        "{rejected:?}"
    );
    assert_eq!(saved_metadata_listing(&mut state, home), before);
    assert_eq!(
        saved_metadata_listing(&mut state, other),
        Vec::<serde_json::Value>::new()
    );
}

#[rstest]
fn contextual_settle_uses_the_issued_saved_session_and_preserves_other_space_focus(
    directory: assert_fs::TempDir,
    #[values(false, true)] inactive: bool,
    #[values(false, true)] stale: bool,
) {
    use bootty_mux::session_membership::SessionLifecycle;
    let SavedLifecycleWorkspace {
        mut state,
        sessions,
        creates,
        ..
    } = saved_lifecycle_workspace(directory.path(), true);
    let spaces = listed_spaces(&mut state);
    let binding = spaces[0].2.clone();
    let before = saved_metadata_listing(&mut state, binding.clone());
    let CommandOutcome::Success { value, .. } = submit_command(
        &mut state,
        CommandInvocation::new(
            "resource.current",
            vec!["session".to_owned()],
            Caller::Socket,
        ),
        Instant::now(),
    ) else {
        panic!("issued attached session");
    };
    let mut target: bootty_control::CommandTarget =
        serde_json::from_value(value["target"].clone()).unwrap();
    if stale {
        target.generation = target.generation.checked_add(1).unwrap();
    }
    if inactive {
        assert!(
            state.activate_space_from_ui(SpaceId::from_persistence(spaces[1].1.parse().unwrap()))
        );
    }
    let focus = state.mux_scope();
    let processes = sessions.lock().unwrap().clone();
    let mut invocation = CommandInvocation::from_action("settle_session", Caller::Socket);
    invocation.target = Some(target);
    let outcome = submit_command(&mut state, invocation, Instant::now());
    assert_eq!(state.mux_scope(), focus);
    assert_eq!(*sessions.lock().unwrap(), processes);
    assert_eq!(creates.load(Ordering::SeqCst), 0);
    let after = saved_metadata_listing(&mut state, binding);
    if stale {
        assert!(
            matches!(outcome, CommandOutcome::StaleTarget { .. }),
            "{outcome:?}"
        );
        assert_eq!(after, before);
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            after[0]["state"]["lifecycle"],
            serde_json::to_value(SessionLifecycle::Settled).unwrap()
        );
        assert_eq!(after[1], before[1]);
    }
}

#[rstest]
fn contextual_settle_without_an_attached_session_is_unavailable_and_keeps_saved_work(
    directory: assert_fs::TempDir,
) {
    let mut state = saved_lifecycle_workspace(directory.path(), false).state;
    let binding = listed_spaces(&mut state)[0].2.clone();
    let before = saved_metadata_listing(&mut state, binding.clone());
    let outcome = submit_command(
        &mut state,
        CommandInvocation::from_action("settle_session", Caller::Socket),
        Instant::now(),
    );
    assert!(
        matches!(outcome, CommandOutcome::Unavailable { .. }),
        "{outcome:?}"
    );
    assert_eq!(saved_metadata_listing(&mut state, binding), before);
}

#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(false, true)]
fn selecting_existing_content_dismisses_creation_and_preserves_the_draft(
    directory: assert_fs::TempDir,
    #[case] select_tab: bool,
    #[case] current_tab_binding: bool,
) {
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Tmux,
    );
    config.input.keybind.push("alt+1=select_tab:1".into());
    let cwd = directory.path().to_string_lossy().into_owned();
    let (repository, space) = claim_first_space(&config.config_path, "task", "first", &cwd);
    drop(repository);
    let mut session = session_on_pane("first", "%1", Some(cwd));
    session.tag = MuxSessionTag {
        identity: Some("task".to_owned()),
        space: Some(space.remote_id().to_owned()),
    };
    session.windows[0].index = 1;
    let mut other = session.windows[0].clone();
    other.id = "other-window".to_owned();
    other.index = 2;
    other.anchor.pane_id = Some("%2".to_owned());
    other.panes = vec![other.anchor.clone()];
    session.windows.push(other);
    let backends = registry(
        [Arc::new(restore_provider(
            MuxBackendKind::Tmux,
            Arc::new(Mutex::new(vec![session])),
            Arc::default(),
        ))],
        [MuxBackendKind::Tmux],
    );
    let mut state = app_state(config, backends);
    state.update_frame(frames::idle_frame(Instant::now()));
    let open = |state: &mut AppState| {
        let outcome = submit_command(
            state,
            CommandInvocation::new("new_mux_session", Vec::new(), Caller::Socket),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
    };
    open(&mut state);
    assert_eq!(
        state.keymap_focus(),
        bootty_ui::keymap_runtime::KeymapFocus::Other
    );
    state.apply_dialog_intent(
        &bootty_ui::gpui::DialogIntent::TextChanged {
            dialog: bootty_ui::gpui::DialogId::new(
                bootty_ui::presentation::dialogs::NEW_SESSION_ID,
            ),
            value: "Keep the creation draft".to_owned(),
        },
        &mut Vec::new(),
    );
    if current_tab_binding {
        let mut frame = frames::idle_frame(Instant::now());
        frame.input.events.push(bootty_ui::gpui::InputEvent::Key {
            key: bootty_ui::gpui::Key::Digit(1),
            pressed: true,
            repeat: false,
            modifiers: bootty_ui::gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
        });
        state.update_frame(frame);
        for _ in 0..250 {
            state.update_frame(frames::idle_frame(Instant::now()));
            if state.modal_dialog().is_none() {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(state.mux().selected_window(), Some("first-window"));
    } else if select_tab {
        let outcome = submit_command(
            &mut state,
            CommandInvocation::new("next_tab", Vec::new(), Caller::Socket),
            Instant::now(),
        );
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert_eq!(state.mux().selected_window(), Some("other-window"));
    } else {
        assert!(
            state.activate_scoped_session_from_ui(&ScopedSessionTarget::new(
                state.mux_scope(),
                "first",
            ))
        );
    }
    assert!(state.modal_dialog().is_none(), "{:?}", state.last_error());
    open(&mut state);
    let Some(ModalDialog::NewSession(dialog)) = state.modal_dialog() else {
        panic!("creation form reopened");
    };
    assert_eq!(
        dialog.draft().expect("draft").prompt,
        "Keep the creation draft"
    );
}

#[rstest]
#[case::closed(false)]
#[case::live(true)]
fn deletion_removes_closed_history_and_rejects_live_sessions(
    directory: assert_fs::TempDir,
    #[case] attached: bool,
) {
    let SavedLifecycleWorkspace {
        config,
        backends,
        mut state,
        ..
    } = saved_lifecycle_workspace(directory.path(), attached);
    let binding = listed_spaces(&mut state)[0].2.clone();
    let before = saved_metadata_listing(&mut state, binding.clone());
    let outcome = saved_metadata_command(
        &mut state,
        "session.delete",
        vec!["saved".to_owned()],
        binding.clone(),
    );
    let expected = if attached {
        assert!(
            matches!(outcome, CommandOutcome::Failed { .. }),
            "{outcome:?}"
        );
        before
    } else {
        assert!(
            matches!(outcome, CommandOutcome::Success { .. }),
            "{outcome:?}"
        );
        vec![before[1].clone()]
    };
    assert_eq!(saved_metadata_listing(&mut state, binding), expected);
    drop(state);
    let mut reopened = app_state(config, backends);
    let binding = listed_spaces(&mut reopened)[0].2.clone();
    assert_eq!(saved_metadata_listing(&mut reopened, binding), expected);
}
