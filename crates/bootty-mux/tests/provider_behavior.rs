use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation, BindingOperationOutcome},
    command::{MuxCommand, MuxSplitDirection},
    controller::{MuxController, RepaintHandle, SpaceId},
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    snapshot::{MuxSessionTag, MuxSnapshot},
};
use pretty_assertions::assert_eq;
use static_assertions::assert_obj_safe;

assert_obj_safe!(MuxBackend);

#[derive(Default)]
struct Calls {
    snapshot: Mutex<MuxSnapshot>,
    snapshots: AtomicUsize,
    executes: AtomicUsize,
}

struct Backend {
    calls: Arc<Calls>,
    fail_snapshot_at: usize,
}

impl MuxBackend for Backend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        let call = self.calls.snapshots.fetch_add(1, Ordering::SeqCst);
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
    fn execute(&mut self, _command: MuxCommand) -> Result<()> {
        self.calls.executes.fetch_add(1, Ordering::SeqCst);
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

    fn build_backend(&self, _: &MuxBindingConfig, _: Option<&Path>) -> Box<dyn MuxBackend> {
        Box::new(Backend {
            calls: Arc::clone(&self.calls),
            fail_snapshot_at: self.fail_snapshot_at,
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
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
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
        BindingCapabilityDescriptor::new(scope, [BindingOperation::SplitPane])
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

#[rstest::rstest]
#[case(MuxBackendKind::Native)]
#[case(MuxBackendKind::Rmux)]
#[case(MuxBackendKind::Tmux)]
fn session_switch_selects_its_known_window_before_refresh(#[case] kind: MuxBackendKind) {
    use bootty_mux::snapshot::{MuxPaneAnchor, MuxSession, MuxWindow};
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
