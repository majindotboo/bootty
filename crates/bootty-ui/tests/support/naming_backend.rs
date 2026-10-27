//! Exercise the native session metadata contract without starting interactive shells.
use std::{path::Path, sync::Arc};
#[path = "placeholder_terminal.rs"]
pub mod placeholder_terminal;

use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::{MuxBackend, PaneCapture, PaneText},
    capability::BindingCapabilityDescriptor,
    command::MuxCommand,
    controller::SpaceId,
    native::NativeProvider,
    provider::{
        MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider, MuxBackendRegistry,
        MuxCommandDispatch, PaneTopology,
    },
    snapshot::MuxSnapshot,
    terminal::{
        BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget,
        TerminalRuntime,
    },
};

/// Retain a supplied backend's host policies and capabilities while keeping topology in process.
/// Without a provider, use the process-free native fixture for session metadata tests.
pub fn backends(
    provider: Option<Arc<dyn MuxAppBackendProvider>>,
    terminal_commands: bool,
) -> anyhow::Result<Arc<MuxBackendRegistry>> {
    let providers: Vec<Arc<dyn MuxAppBackendProvider>> = provider.map_or_else(
        || -> Vec<Arc<dyn MuxAppBackendProvider>> {
            vec![
                Arc::new(NativeProvider),
                Arc::new(bootty_mux::rmux::RmuxProvider),
                Arc::new(bootty_mux::tmux::TmuxProvider),
            ]
        },
        |provider| vec![provider],
    );
    let kinds = providers
        .iter()
        .map(|provider| provider.kind())
        .collect::<Vec<_>>();
    let providers = providers.into_iter().map(|provider| {
        let topology = (provider.kind() == MuxBackendKind::Native && !terminal_commands)
            .then_some(PaneTopology::BackendReconciled);
        Arc::new(NamingProvider {
            provider,
            topology,
            terminal_commands,
        })
    });
    Ok(Arc::new(MuxBackendRegistry::from_app_providers(
        providers, kinds,
    )?))
}

struct NamingProvider {
    provider: Arc<dyn MuxAppBackendProvider>,
    topology: Option<PaneTopology>,
    terminal_commands: bool,
}

impl MuxBackendProvider for NamingProvider {
    fn kind(&self) -> MuxBackendKind {
        self.provider.kind()
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        self.provider.command_dispatch()
    }

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(NamingBackend(
            NativeProvider.build_backend(config, workspace),
        ))
    }
}

impl MuxAppBackendProvider for NamingProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        let mut policy = self.provider.app_policy();
        if let Some(topology) = self.topology {
            policy.panes.topology = topology;
        }
        policy
    }

    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(NamingPanePolicy {
            terminal_commands: self.terminal_commands,
        })
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        self.provider.capabilities(scope)
    }
}

struct NamingPanePolicy {
    terminal_commands: bool,
}

struct NamingBackend(Box<dyn MuxBackend>);

impl MuxBackend for NamingBackend {
    fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
        self.0.snapshot()
    }

    fn execute(&mut self, command: MuxCommand) -> anyhow::Result<()> {
        let cwd = match &command {
            MuxCommand::CreateProjectSession { cwd, .. }
            | MuxCommand::CreateWorktreeSession { cwd, .. } => Some(cwd.as_str()),
            MuxCommand::NewWindow { cwd, .. } | MuxCommand::CreatePane { cwd, .. } => {
                cwd.as_deref()
            }
            _ => None,
        };
        if let Some(cwd) = cwd {
            anyhow::ensure!(
                Path::new(cwd).is_dir(),
                "working directory {cwd} is unavailable"
            );
        }
        self.0.execute(command)
    }

    fn capture_pane(&self, _pane_id: &str, _capture: PaneCapture) -> anyhow::Result<PaneText> {
        // This fixture has no process, so its saved terminal contents are empty.
        Ok(PaneText {
            text: String::new(),
            captured_lines: 0,
            omitted_lines: 0,
        })
    }
}

impl BackendPanePolicy for NamingPanePolicy {
    fn remote_target(&self) -> Option<bootty_mux::RemoteTarget> {
        None
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> anyhow::Result<Option<Box<dyn TerminalRuntime>>> {
        if self.terminal_commands {
            if !request.terminal_config.launch.command.is_empty() {
                return bootty_mux::native::NativePanePolicy.start_terminal(request);
            }
            return Ok(Some(Box::new(
                placeholder_terminal::PlaceholderTerminal::new(
                    request.spawn_geometry,
                    request.target.cwd().map(str::to_owned),
                ),
            )));
        }
        Ok(None)
    }

    fn sync_target(&mut self, _target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {}

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(
        &mut self,
        _request: PaneLayoutResizeRequest<'_>,
    ) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn deactivate(&mut self) {}
}
