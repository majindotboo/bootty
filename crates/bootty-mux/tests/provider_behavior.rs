#![cfg(test)]

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use bootty_control::ResourceKind;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation, BindingOperationOutcome},
    command::{MuxCommand, MuxSplitDirection},
    controller::{MuxCommandCompletion, MuxController, RepaintHandle, SpaceId},
    provider::{
        MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider, MuxBackendRegistry,
        MuxCommandDispatch, PaneBehavior, PaneTopology, SelectionPublicationPolicy,
        TerminalProgressPolicy, TerminalResidency,
    },
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, MuxWindow},
    target::{ExactMuxTarget, exact_mux_target},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use static_assertions::assert_obj_safe;

assert_obj_safe!(MuxBackend);

#[derive(Default)]
struct Calls {
    snapshot: Mutex<MuxSnapshot>,
    snapshots: AtomicUsize,
    executes: AtomicUsize,
    snapshot_queries: Option<mpsc::Sender<SnapshotQuery>>,
    command_queries: Option<mpsc::Sender<CommandQuery>>,
}

struct CommandQuery {
    command: MuxCommand,
    response: mpsc::Sender<()>,
}

struct SnapshotQuery {
    config: MuxBindingConfig,
    response: mpsc::Sender<Result<MuxSnapshot>>,
}

struct Backend {
    calls: Arc<Calls>,
    fail_snapshot_at: usize,
    config: MuxBindingConfig,
}

impl MuxBackend for Backend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let call = self.calls.snapshots.fetch_add(1, Ordering::SeqCst);
        if let Some(queries) = &self.calls.snapshot_queries {
            let (response, receiver) = mpsc::channel();
            queries
                .send(SnapshotQuery {
                    config: self.config.clone(),
                    response,
                })
                .expect("test receives snapshot request");
            return receiver
                .recv()
                .expect("snapshot worker deliberately stopped by test");
        }
        (call < self.fail_snapshot_at)
            .then(|| {
                self.calls
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .ok_or_else(|| anyhow!("dynamic refresh failure"))
    }
    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.calls.executes.fetch_add(1, Ordering::SeqCst);
        if let Some(queries) = &self.calls.command_queries {
            let (response, receiver) = mpsc::channel();
            queries.send(CommandQuery { command, response })?;
            receiver.recv()?;
        }
        Ok(())
    }
}

struct Provider {
    kind: MuxBackendKind,
    caller_thread: AtomicBool,
    calls: Arc<Calls>,
    fail_snapshot_at: usize,
}

impl Provider {
    fn new(kind: MuxBackendKind, fail_snapshot_at: usize) -> Arc<Self> {
        Arc::new(Self {
            kind,
            caller_thread: AtomicBool::new(false),
            calls: Arc::default(),
            fail_snapshot_at,
        })
    }
}

impl MuxBackendProvider for Provider {
    fn kind(&self) -> MuxBackendKind {
        self.kind
    }
    fn command_dispatch(&self) -> MuxCommandDispatch {
        if self.caller_thread.load(Ordering::SeqCst) {
            MuxCommandDispatch::CallerThread
        } else {
            MuxCommandDispatch::WorkerThread
        }
    }

    fn build_backend(&self, config: &MuxBindingConfig, _: Option<&Path>) -> Box<dyn MuxBackend> {
        Box::new(Backend {
            calls: Arc::clone(&self.calls),
            fail_snapshot_at: self.fail_snapshot_at,
            config: config.clone(),
        })
    }
}

impl MuxAppBackendProvider for Provider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: false,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,

            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(
        &self,
        _config: &MuxBindingConfig,
    ) -> Box<dyn bootty_mux::terminal::BackendPanePolicy> {
        Box::new(bootty_mux::tmux::TmuxPanePolicy::new(None))
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(
            scope,
            [
                BindingOperation::SplitPane,
                BindingOperation::NavigateWindow,
            ],
        )
    }
}

fn config() -> MuxBindingConfig {
    MuxBindingConfig {
        backend: MuxBackendKind::Tmux,
        ..Default::default()
    }
}

fn registry(provider: Arc<Provider>) -> Result<Arc<MuxBackendRegistry>> {
    Ok(Arc::new(MuxBackendRegistry::from_app_providers(
        [provider],
        [MuxBackendKind::Tmux],
    )?))
}

fn controller(provider: Arc<Provider>, scope: i64) -> Result<MuxController> {
    Ok(MuxController::new(
        SpaceId::from_persistence(scope),
        registry(provider)?,
        None,
    ))
}

#[rstest::rstest]
#[case::missing_backend(false)]
#[case::core_only(true)]
fn unavailable_provider_never_executes_a_command(#[case] core_only: bool) -> Result<()> {
    let provider = Provider::new(MuxBackendKind::Tmux, usize::MAX);
    let registry = if core_only {
        let core: Arc<dyn MuxBackendProvider> = provider.clone();
        MuxBackendRegistry::from_core_providers([core], [MuxBackendKind::Tmux])?
    } else {
        MuxBackendRegistry::from_app_providers::<Provider>([], [])?
    };
    let config = config();
    let scope = SpaceId::from_persistence(1);
    let mut backend = Backend {
        calls: Arc::clone(&provider.calls),
        fail_snapshot_at: usize::MAX,
        config: config.clone(),
    };
    anyhow::ensure!(registry.app_provider(&config).is_err());
    anyhow::ensure!(registry.capabilities(&config, scope).is_none());
    anyhow::ensure!(matches!(
        registry.execute_checked(&config, scope, &mut backend, split_command()),
        BindingOperationOutcome::Unavailable
    ));
    assert_eq!(provider.calls.executes.load(Ordering::SeqCst), 0);
    assert_eq!(registry.build_backend(&config, None).is_ok(), core_only);
    let controller = MuxController::new(scope, Arc::new(registry), None);
    anyhow::ensure!(matches!(
        controller.operation_outcome(&config, BindingOperation::SplitPane),
        BindingOperationOutcome::Unavailable
    ));
    Ok(())
}

#[test]
fn unsupported_command_does_not_reach_backend() {
    let provider = Provider::new(MuxBackendKind::Tmux, usize::MAX);
    let registry = registry(Arc::clone(&provider)).expect("provider registry");
    let mut backend = Backend {
        calls: Arc::clone(&provider.calls),
        fail_snapshot_at: usize::MAX,
        config: config(),
    };

    let unsupported = registry.execute_checked(
        &config(),
        SpaceId::from_persistence(1),
        &mut backend,
        MuxCommand::DitchSession {
            session_id: "session".into(),
        },
    );
    let supported = registry.execute_checked(
        &config(),
        SpaceId::from_persistence(1),
        &mut backend,
        split_command(),
    );

    assert!(matches!(unsupported, BindingOperationOutcome::Unsupported));
    assert!(matches!(
        supported,
        BindingOperationOutcome::Supported(Ok(()))
    ));
    assert_eq!(provider.calls.executes.load(Ordering::SeqCst), 1);
}

fn split_command() -> MuxCommand {
    MuxCommand::SplitPane {
        session_id: "session".into(),
        pane_id: None,
        direction: MuxSplitDirection::Right,
    }
}

#[test]
fn registry_rejects_missing_and_duplicate_providers() {
    let missing = MuxBackendRegistry::from_app_providers::<Provider>([], [MuxBackendKind::Tmux]);
    let duplicate = MuxBackendRegistry::from_app_providers(
        [
            Provider::new(MuxBackendKind::Tmux, usize::MAX),
            Provider::new(MuxBackendKind::Tmux, usize::MAX),
        ],
        [MuxBackendKind::Tmux],
    );

    assert_eq!(
        missing
            .err()
            .expect("missing provider must fail")
            .to_string(),
        "missing mux backend provider for Tmux"
    );
    assert_eq!(
        duplicate
            .err()
            .expect("duplicate provider must fail")
            .to_string(),
        "duplicate mux backend provider for Tmux"
    );
}

#[rstest::rstest]
fn empty_sessions_are_not_authoritative_until_a_provider_has_replied() {
    let provider = Provider::new(MuxBackendKind::Tmux, 1);
    provider.caller_thread.store(true, Ordering::SeqCst);
    let mut controller = controller(provider, 2).expect("provider controller");
    let repaint: RepaintHandle = Arc::new(|| {});
    assert_eq!(controller.sessions(), []);
    assert!(!controller.has_session_snapshot());
    assert!(
        controller
            .refresh_sessions(&repaint, &config(), Duration::ZERO)
            .applied
    );
    assert_eq!(controller.sessions(), []);
    assert!(controller.has_session_snapshot());
    assert!(
        controller
            .refresh_sessions(&repaint, &config(), Duration::ZERO)
            .error
            .is_some()
    );
    assert!(!controller.has_session_snapshot());
}

#[test]
fn refresh_outcome_fields_are_independent() {
    let provider = Provider::new(MuxBackendKind::Tmux, 1);
    let mut controller = controller(Arc::clone(&provider), 2).expect("provider controller");
    let repaint: RepaintHandle = Arc::new(|| {});

    let queued = controller.refresh_sessions(&repaint, &config(), Duration::ZERO);
    assert!(!queued.applied);
    provider.caller_thread.store(true, Ordering::SeqCst);
    let mut applied = false;
    let mut error = None;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(1));
        let outcome = controller.refresh_sessions(&repaint, &config(), Duration::ZERO);
        applied |= outcome.applied;
        error = error.or(outcome.error);
        if applied && error.is_some() {
            break;
        }
    }

    assert_eq!(
        (applied, error.as_deref()),
        (true, Some("dynamic refresh failure"))
    );
    assert_eq!(controller.last_error(), Some("dynamic refresh failure"));
    assert_eq!(controller.unavailable_reason(), controller.last_error());
    assert!(
        !controller
            .refresh_sessions(&repaint, &config(), Duration::ZERO)
            .applied
    );
}

#[test]
fn caller_thread_snapshot_failure_still_executes_the_command_once() {
    let provider = Provider::new(MuxBackendKind::Tmux, 0);
    provider.caller_thread.store(true, Ordering::SeqCst);
    let mut controller = controller(Arc::clone(&provider), 3).expect("provider controller");
    let repaint: RepaintHandle = Arc::new(|| {});

    controller.execute_command(&repaint, &config(), split_command());

    assert_eq!(controller.last_error(), Some("dynamic refresh failure"));
    assert_eq!(provider.calls.executes.load(Ordering::SeqCst), 1);
    assert_eq!(controller.poll_command(), None);
}

struct ControlledRefresh {
    controller: MuxController,
    requests: mpsc::Receiver<SnapshotQuery>,
    repaint: RepaintHandle,
    wakes: mpsc::Receiver<()>,
}

#[rstest::fixture]
fn controlled_refresh() -> ControlledRefresh {
    let (queries, requests) = mpsc::channel();
    let provider = Arc::new(Provider {
        kind: MuxBackendKind::Tmux,
        caller_thread: AtomicBool::new(false),
        calls: Arc::new(Calls {
            snapshot_queries: Some(queries),
            ..Calls::default()
        }),
        fail_snapshot_at: usize::MAX,
    });
    let (wake, wakes) = mpsc::channel();
    let repaint: RepaintHandle = Arc::new(move || {
        let _ = wake.send(());
    });
    ControlledRefresh {
        controller: controller(provider, 4).expect("provider controller"),
        requests,
        repaint,
        wakes,
    }
}

fn receive<T>(receiver: &mpsc::Receiver<T>) -> T {
    receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("worker must make progress")
}

enum RefreshSupersession {
    Configuration,
    Commands(usize),
}

#[rstest::rstest]
#[case::configuration(RefreshSupersession::Configuration)]
#[case::command_completion(RefreshSupersession::Commands(1))]
#[case::command_burst(RefreshSupersession::Commands(20))]
fn superseded_refreshes_neither_publish_nor_queue_a_backlog(
    controlled_refresh: ControlledRefresh,
    #[case] supersession: RefreshSupersession,
) {
    let ControlledRefresh {
        mut controller,
        requests,
        repaint,
        wakes,
    } = controlled_refresh;
    let mut config = config();
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    let old = receive(&requests);
    match supersession {
        RefreshSupersession::Configuration => {
            config.hide_tmux_status = !config.hide_tmux_status;
            controller.refresh_sessions(&repaint, &config, Duration::ZERO);
        }
        RefreshSupersession::Commands(count) => {
            for _ in 0..count {
                controller
                    .complete_authoritative_command(Ok(MuxCommandCompletion::default()), &config)
                    .expect("complete newer command");
                controller.refresh_sessions(&repaint, &config, Duration::ZERO);
            }
        }
    }
    old.response
        .send(Ok(MuxSnapshot::default()))
        .expect("finish old snapshot");
    receive(&wakes);
    assert!(
        !controller
            .refresh_sessions(&repaint, &config, Duration::MAX)
            .applied
    );

    let current = receive(&requests);
    assert_eq!(current.config, config);
    current
        .response
        .send(Ok(MuxSnapshot::default()))
        .expect("finish current snapshot");
    receive(&wakes);
    let outcome = controller.refresh_sessions(&repaint, &config, Duration::MAX);
    assert!(
        outcome.applied,
        "the next refresh must be current: {outcome:?}"
    );
    assert_eq!(outcome.error, None);
    assert!(controller.has_session_snapshot());
}

#[rstest::rstest]
fn a_stopped_snapshot_worker_reports_failure_once_and_can_recover(
    controlled_refresh: ControlledRefresh,
) {
    let ControlledRefresh {
        mut controller,
        requests,
        repaint,
        wakes,
    } = controlled_refresh;
    let config = config();
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    drop(receive(&requests).response);
    // No sleep: let the worker unwind, bounded only to diagnose a stuck worker.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("test deadline fits");
    let failure = loop {
        let outcome = controller.refresh_sessions(&repaint, &config, Duration::MAX);
        if outcome.error.is_some() {
            break outcome;
        }
        assert!(Instant::now() < deadline, "stopped worker must be detected");
        std::thread::yield_now();
    };
    assert_eq!(
        failure.error.as_deref(),
        Some("mux session refresh worker stopped")
    );
    assert!(!controller.has_session_snapshot());
    assert_eq!(
        controller
            .refresh_sessions(&repaint, &config, Duration::ZERO)
            .error,
        None
    );
    receive(&requests)
        .response
        .send(Ok(MuxSnapshot::default()))
        .expect("recovery snapshot");
    receive(&wakes);
    assert!(
        controller
            .refresh_sessions(&repaint, &config, Duration::MAX)
            .applied
    );
    assert!(controller.has_session_snapshot());
    assert_eq!(controller.last_error(), None);
}

#[derive(Arbitrary, Debug)]
struct ResourceIds {
    #[proptest(regex = ".{1,16}")]
    session: String,
    #[proptest(regex = ".{1,16}")]
    window: String,
    #[proptest(regex = ".{1,16}")]
    pane: String,
}

impl ResourceIds {
    fn snapshot(&self) -> MuxSnapshot {
        let anchor = MuxPaneAnchor {
            session_id: self.session.clone(),
            pane_id: Some(self.pane.clone()),
            ..Default::default()
        };
        MuxSnapshot {
            active_session_id: Some(self.session.clone()),
            sessions: vec![MuxSession {
                id: self.session.clone(),
                name: "display name".to_owned(),
                active: true,
                anchor: anchor.clone(),
                active_window_id: Some(self.window.clone()),
                tag: MuxSessionTag::default(),
                windows: vec![MuxWindow {
                    id: self.window.clone(),
                    index: 0,
                    name: "window display name".to_owned(),
                    active: true,
                    anchor,
                    panes: Vec::new(),
                    layout: None,
                    progress: None,
                }],
            }],
            ..Default::default()
        }
    }
}

proptest! {
    #[test]
    fn live_targets_round_trip_and_cannot_retarget_recreated_resources(ids in any::<ResourceIds>()) {
        let provider = Provider::new(MuxBackendKind::Tmux, usize::MAX);
        provider.caller_thread.store(true, Ordering::SeqCst);
        let mut controller = controller(Arc::clone(&provider), 5).unwrap();
        let scope = SpaceId::from_persistence(5);
        let repaint: RepaintHandle = Arc::new(|| {});
        let snapshot = ids.snapshot();
        *provider.calls.snapshot.lock().unwrap() = snapshot.clone();
        controller.refresh_sessions(&repaint, &config(), Duration::ZERO);
        let binding = "opaque host binding";
        let pane = ExactMuxTarget::Pane(scope, ids.session.clone(), ids.window.clone(), ids.pane.clone());
        let resources = [
            (ResourceKind::Binding, ExactMuxTarget::Binding(scope)),
            (ResourceKind::Terminal, ExactMuxTarget::Binding(scope)),
            (ResourceKind::Terminal, ExactMuxTarget::Session(scope, ids.session.clone())),
            (ResourceKind::Session, ExactMuxTarget::Session(scope, ids.session.clone())),
            (ResourceKind::MuxWindow, ExactMuxTarget::window(scope, &ids.session, &ids.window)),
            (ResourceKind::Pane, pane.clone()),
            (ResourceKind::Terminal, pane),
        ];
        let mut captured = Vec::new();
        for (kind, exact) in resources {
            let target = exact.command_target(kind, &controller, binding).unwrap();
            prop_assert_eq!(exact_mux_target(scope, &controller, &target, binding), Some(exact.clone()));
            let mut stale = target.clone();
            stale.generation = stale.generation.checked_add(1).unwrap();
            prop_assert_eq!(exact_mux_target(scope, &controller, &stale, binding), None);
            prop_assert_eq!(exact_mux_target(scope, &controller, &target, "another binding"), None);
            captured.push((exact, target));
        }
        *provider.calls.snapshot.lock().unwrap() = MuxSnapshot::default();
        controller.refresh_sessions(&repaint, &config(), Duration::ZERO);
        for (exact, target) in &captured {
            if !matches!(exact, ExactMuxTarget::Binding(_)) {
                prop_assert_eq!(exact.command_target(target.kind, &controller, binding), None);
                prop_assert_eq!(exact_mux_target(scope, &controller, target, binding), None);
            }
        }
        *provider.calls.snapshot.lock().unwrap() = snapshot;
        controller.refresh_sessions(&repaint, &config(), Duration::ZERO);
        for (exact, old) in captured {
            let current = exact.command_target(old.kind, &controller, binding).unwrap();
            if !matches!(exact, ExactMuxTarget::Binding(_)) {
                prop_assert!(current.generation > old.generation);
                prop_assert_eq!(exact_mux_target(scope, &controller, &old, binding), None);
            }
            prop_assert_eq!(exact_mux_target(scope, &controller, &current, binding), Some(exact));
        }
    }
}

#[rstest::rstest]
#[case(MuxBackendKind::Native)]
#[case(MuxBackendKind::Rmux)]
#[case(MuxBackendKind::Tmux)]
fn session_switch_selects_its_known_window_before_refresh(#[case] kind: MuxBackendKind) {
    let provider = Provider::new(kind, usize::MAX);
    provider.caller_thread.store(true, Ordering::SeqCst);
    let sessions = ["one", "two"].map(|id| {
        let anchor = MuxPaneAnchor {
            session_id: id.into(),
            ..Default::default()
        };
        MuxSession {
            id: id.into(),
            name: id.into(),
            active: id == "one",
            anchor: anchor.clone(),
            active_window_id: Some(format!("{id}-window")),
            tag: MuxSessionTag::default(),
            windows: vec![MuxWindow {
                id: format!("{id}-window"),
                index: 0,
                name: id.into(),
                active: id == "one",
                anchor,
                panes: vec![],
                layout: None,
                progress: None,
            }],
        }
    });
    *provider.calls.snapshot.lock().unwrap() = MuxSnapshot {
        sessions: sessions.into(),
        active_session_id: Some("one".into()),
        ..Default::default()
    };
    let registry =
        Arc::new(MuxBackendRegistry::from_app_providers([provider.clone()], [kind]).unwrap());
    let mut controller = MuxController::new(SpaceId::from_persistence(42), registry, None);
    let config = MuxBindingConfig {
        backend: kind,
        ..Default::default()
    };
    let repaint: RepaintHandle = Arc::new(|| {});
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    let snapshots = provider.calls.snapshots.load(Ordering::SeqCst);
    controller.activate_session("two");
    assert_eq!(controller.selected_window(), Some("two-window"));
    assert_eq!(
        controller.selected_session_anchor().unwrap().session_id,
        "two"
    );
    assert_eq!(provider.calls.snapshots.load(Ordering::SeqCst), snapshots);
}

#[rstest::rstest]
#[case(MuxBackendKind::Native)]
#[case(MuxBackendKind::Rmux)]
#[case(MuxBackendKind::Tmux)]
fn rapid_tab_selection_survives_older_commands_and_refreshes(#[case] kind: MuxBackendKind) {
    let (command_tx, commands) = mpsc::channel();
    let (snapshot_tx, snapshots) = mpsc::channel();
    let provider = Arc::new(Provider {
        kind,
        caller_thread: AtomicBool::new(false),
        calls: Arc::new(Calls {
            command_queries: Some(command_tx),
            snapshot_queries: Some(snapshot_tx),
            ..Calls::default()
        }),
        fail_snapshot_at: usize::MAX,
    });
    let registry = Arc::new(MuxBackendRegistry::from_app_providers([provider], [kind]).unwrap());
    let mut controller = MuxController::new(SpaceId::from_persistence(42), registry, None);
    let config = MuxBindingConfig {
        backend: kind,
        ..Default::default()
    };
    let (wake, wakes) = mpsc::channel();
    let repaint: RepaintHandle = Arc::new(move || {
        let _ = wake.send(());
    });
    let mut snapshot = ResourceIds {
        session: "session".into(),
        window: "one".into(),
        pane: "pane".into(),
    }
    .snapshot();
    snapshot.sessions[0].windows[0].index = 1;
    let mut other = snapshot.sessions[0].windows[0].clone();
    other.id = "five".into();
    other.index = 5;
    other.active = false;
    snapshot.sessions[0].windows.push(other);

    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    receive(&snapshots)
        .response
        .send(Ok(snapshot.clone()))
        .unwrap();
    receive(&wakes);
    controller.refresh_sessions(&repaint, &config, Duration::MAX);
    // This poll starts before navigation but returns the first command's intermediate window.
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    let older_snapshot = receive(&snapshots);
    for index in [5, 1] {
        controller.execute_command(
            &repaint,
            &config,
            MuxCommand::ActivateWindowIndex {
                session_id: "session".into(),
                index,
            },
        );
    }
    assert_eq!(controller.selected_window(), Some("one"));
    snapshot.sessions[0].active_window_id = Some("five".into());
    older_snapshot.response.send(Ok(snapshot.clone())).unwrap();
    receive(&wakes);
    assert!(
        !controller
            .refresh_sessions(&repaint, &config, Duration::ZERO)
            .applied
    );
    assert_eq!(controller.selected_window(), Some("one"));

    for index in [5, 1] {
        let command = receive(&commands);
        assert_eq!(
            command.command,
            MuxCommand::ActivateWindowIndex {
                session_id: "session".into(),
                index,
            }
        );
        command.response.send(()).unwrap();
        receive(&wakes);
        assert_eq!(controller.poll_command(), Some(Ok(())));
        assert_eq!(controller.selected_window(), Some("one"));
        if index == 5 {
            controller.refresh_sessions(&repaint, &config, Duration::ZERO);
            assert!(matches!(
                snapshots.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
        }
    }
    snapshot.sessions[0].active_window_id = Some("one".into());
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    receive(&snapshots)
        .response
        .send(Ok(snapshot.clone()))
        .unwrap();
    receive(&wakes);
    assert!(
        controller
            .refresh_sessions(&repaint, &config, Duration::MAX)
            .applied
    );
    assert_eq!(controller.selected_window(), Some("one"));

    // Backend navigation after the burst still follows the external selection.
    snapshot.sessions[0].active_window_id = Some("five".into());
    controller.refresh_sessions(&repaint, &config, Duration::ZERO);
    receive(&snapshots).response.send(Ok(snapshot)).unwrap();
    receive(&wakes);
    assert!(
        controller
            .refresh_sessions(&repaint, &config, Duration::MAX)
            .applied
    );
    assert_eq!(controller.selected_window(), Some("five"));
}
