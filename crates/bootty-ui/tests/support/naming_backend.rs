//! Exercise the native session metadata contract without starting interactive shells.
use std::{path::Path, sync::Arc};

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

pub fn backends() -> anyhow::Result<Arc<MuxBackendRegistry>> {
    Ok(Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(NamingProvider)],
        [MuxBackendKind::Native],
    )?))
}

struct NamingProvider;

impl MuxBackendProvider for NamingProvider {
    fn kind(&self) -> MuxBackendKind {
        NativeProvider.kind()
    }

    fn command_dispatch(&self) -> MuxCommandDispatch {
        NativeProvider.command_dispatch()
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
        let mut policy = NativeProvider.app_policy();
        policy.panes.topology = PaneTopology::BackendReconciled;
        policy
    }

    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(NamingPanePolicy)
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        NativeProvider.capabilities(scope)
    }
}

struct NamingPanePolicy;

struct NamingBackend(Box<dyn MuxBackend>);

impl MuxBackend for NamingBackend {
    fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
        self.0.snapshot()
    }

    fn execute(&mut self, command: MuxCommand) -> anyhow::Result<()> {
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
        _request: PaneStartRequest<'_>,
    ) -> anyhow::Result<Option<Box<dyn TerminalRuntime>>> {
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
