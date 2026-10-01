#![cfg(test)]

//! Pane-addressed input through `WorkspaceRuntime`: where it goes, and that it never outlives the
//! binding that dispatched it.

use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_config::config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::{MuxBackend, PaneCapture, PaneInput},
    capability::BindingCapabilityDescriptor,
    command::MuxCommand,
    controller::{MuxCommandError, SpaceId},
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    repository::{SpaceMuxOverride, SpaceRemoteOverride},
    snapshot::MuxSnapshot,
    terminal::{
        BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget,
        TerminalRuntime,
    },
    workspace::WorkspaceRuntime,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

type Delivered = Arc<Mutex<Vec<(String, PaneInput)>>>;

/// Holds the next backend a worker builds until the test lets it go, so the test can act between
/// dispatch and claim.
#[derive(Default)]
struct Gate(Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>);

impl Gate {
    /// Returns the sender that opens the gate and the receiver that hears a worker arrive.
    fn close(&self) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
        let (open, wait) = mpsc::channel();
        let (arrive, arrived) = mpsc::channel();
        *self.0.lock().expect("gate lock") = Some((arrive, wait));
        (open, arrived)
    }

    fn pass(&self) {
        let gate = self.0.lock().expect("gate lock").take();
        if let Some((arrive, wait)) = gate {
            let _ = arrive.send(());
            let _ = wait.recv();
        }
    }
}

struct ScriptedBackend {
    delivered: Delivered,
}

impl MuxBackend for ScriptedBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot::default())
    }

    fn execute(&mut self, _command: MuxCommand) -> Result<()> {
        Ok(())
    }

    fn send_pane_input(&self, pane_id: &str, input: &PaneInput) -> Result<()> {
        self.delivered
            .lock()
            .expect("delivered lock")
            .push((pane_id.to_owned(), input.clone()));
        Ok(())
    }
}

struct ScriptedProvider {
    kind: MuxBackendKind,
    topology: PaneTopology,
    gate: Arc<Gate>,
    delivered: Delivered,
}

impl MuxBackendProvider for ScriptedProvider {
    fn kind(&self) -> MuxBackendKind {
        self.kind
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn build_backend(&self, _: &MuxBindingConfig, _: Option<&Path>) -> Box<dyn MuxBackend> {
        self.gate.pass();
        Box::new(ScriptedBackend {
            delivered: Arc::clone(&self.delivered),
        })
    }
}

struct NoPanes;

impl BackendPanePolicy for NoPanes {
    fn remote_target(&self) -> Option<bootty_mux::RemoteTarget> {
        None
    }
    fn start_terminal(
        &mut self,
        _: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        Ok(None)
    }
    fn sync_target(&mut self, _: Option<&ScopedMuxPaneTarget>, _: bool) {}
    fn set_layout_window(&mut self, _: Option<&str>) {}
    fn resize_layout_window(&mut self, _: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }
    fn deactivate(&mut self) {}
}

impl MuxAppBackendProvider for ScriptedProvider {
    fn build_pane_policy(&self, _: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(NoPanes)
    }

    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: self.topology,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(scope, [])
    }
}

struct Fixture {
    _directory: assert_fs::TempDir,
    config: BoottyConfig,
    workspace: WorkspaceRuntime,
    /// A second Space the input targets, on the tmux provider, and its binding's scope.
    space: SpaceId,
    target: SpaceId,
    gate: Arc<Gate>,
    delivered: Delivered,
}

/// A home Space on the native provider and a target Space on a scripted tmux provider with
/// `topology`. An rmux provider is registered too, so the target can be moved to it.
fn fixture(topology: PaneTopology) -> Result<Fixture> {
    let directory = assert_fs::TempDir::new()?;
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let gate = Arc::new(Gate::default());
    let delivered = Delivered::default();
    let providers = [
        (MuxBackendKind::Native, PaneTopology::ProcessLocal),
        (MuxBackendKind::Tmux, topology),
        (MuxBackendKind::Rmux, topology),
    ]
    .map(|(kind, topology)| {
        Arc::new(ScriptedProvider {
            kind,
            topology,
            gate: Arc::clone(&gate),
            delivered: Arc::clone(&delivered),
        })
    });
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(providers, [])?);
    let mut workspace = WorkspaceRuntime::open(
        &config,
        "main",
        registry,
        AppearanceVariant::Light,
        Arc::new(|| {}),
    )?;
    let space = workspace
        .create_space(
            "Target",
            "folder",
            [0, 0, 0],
            false,
            placement(MultiplexerBackendConfig::Tmux),
            &config,
            AppearanceVariant::Light,
        )?
        .expect("valid Space");
    let target = workspace
        .spaces()
        .find(|candidate| candidate.id == space)
        .map(|candidate| candidate.binding.scope())
        .expect("created Space is live");
    Ok(Fixture {
        _directory: directory,
        config,
        workspace,
        space,
        target,
        gate,
        delivered,
    })
}

const fn placement(backend: MultiplexerBackendConfig) -> SpaceMuxOverride {
    SpaceMuxOverride {
        backend: Some(backend),
        remote: SpaceRemoteOverride::Local,
    }
}

fn send(
    workspace: &mut WorkspaceRuntime,
    scope: SpaceId,
    input: PaneInput,
) -> mpsc::Receiver<Result<(), MuxCommandError>> {
    let (done, result) = mpsc::channel();
    workspace.send_pane_input(scope, "%7", input, None, move |outcome| {
        let _ = done.send(outcome);
    });
    result
}

fn wait(result: &mpsc::Receiver<Result<(), MuxCommandError>>) -> Result<(), MuxCommandError> {
    result
        .recv_timeout(Duration::from_secs(5))
        .expect("pane input completes")
}

/// A pane whose backend owns its process takes input through the backend when Bootty has no
/// runtime for it: every tmux pane, and an rmux session nothing has shown yet. A process-local
/// pane exists only as its runtime.
#[rstest]
#[case::attach(PaneTopology::Attach, true)]
#[case::backend_reconciled(PaneTopology::BackendReconciled, true)]
#[case::process_local(PaneTopology::ProcessLocal, false)]
fn a_pane_without_a_runtime_takes_input_through_its_backend(
    #[case] topology: PaneTopology,
    #[case] delivered: bool,
) -> Result<()> {
    let mut fixture = fixture(topology)?;
    let result = wait(&send(
        &mut fixture.workspace,
        fixture.target,
        PaneInput::Paste("hidden".to_owned()),
    ));
    assert_eq!(result.is_ok(), delivered, "{result:?}");
    let expected = if delivered {
        vec![("%7".to_owned(), PaneInput::Paste("hidden".to_owned()))]
    } else {
        Vec::new()
    };
    assert_eq!(*fixture.delivered.lock().expect("delivered lock"), expected);
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum Retirement {
    Closed,
    MovedToAnotherBackend,
}

/// Input dispatched just before its Space is closed or moved to another backend is claimed after
/// the binding is gone, so it fails as stale and never reaches the old backend.
#[rstest]
#[case::closed(Retirement::Closed)]
#[case::reconfigured(Retirement::MovedToAnotherBackend)]
fn input_for_a_retired_binding_fails_as_stale(#[case] retirement: Retirement) -> Result<()> {
    let mut fixture = fixture(PaneTopology::Attach)?;
    let (open, arrived) = fixture.gate.close();
    let result = send(
        &mut fixture.workspace,
        fixture.target,
        PaneInput::Write(b"late".to_vec()),
    );
    arrived.recv_timeout(Duration::from_secs(5))?;
    match retirement {
        Retirement::Closed => {
            let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
            fixture.workspace.close_space(
                fixture.space,
                "main",
                &fixture.config,
                AppearanceVariant::Light,
                &repaint,
                Instant::now(),
            )?;
        }
        Retirement::MovedToAnotherBackend => {
            let summary = fixture
                .workspace
                .space_summaries()
                .into_iter()
                .find(|summary| summary.id == fixture.space)
                .expect("target Space summary");
            fixture.workspace.update_space(
                &summary,
                placement(MultiplexerBackendConfig::Rmux),
                &fixture.config,
                AppearanceVariant::Light,
            )?;
        }
    }
    open.send(())?;
    let claimed = wait(&result);
    anyhow::ensure!(
        matches!(claimed, Err(MuxCommandError::Stale)),
        "input for a retired binding must be stale: {claimed:?}"
    );
    assert_eq!(
        *fixture.delivered.lock().expect("delivered lock"),
        Vec::new()
    );
    Ok(())
}

/// A pane that shrinks or trims history between measuring and reading returns fewer rows than
/// were measured; the capture reports the rows it read.
#[rstest]
#[case::as_measured("a\nb\nc\n", 3)]
#[case::shrunk("a\n", 1)]
fn a_capture_counts_the_rows_it_read(#[case] text: &str, #[case] captured: u64) {
    let rows = PaneCapture {
        history: false,
        max_lines: 3,
        ansi: false,
    }
    .rows(0, 5);
    let capture = rows.text(text.to_owned());
    assert_eq!(
        (capture.captured_lines, capture.omitted_lines),
        (captured, 2)
    );
}
