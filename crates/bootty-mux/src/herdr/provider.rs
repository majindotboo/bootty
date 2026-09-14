use std::path::Path;

use crate::{MuxBackendKind, MuxBindingConfig};
use crate::{
    backend::MuxBackend,
    provider::{MuxBackendProvider, MuxCommandDispatch},
};
#[cfg(feature = "terminal-runtime")]
use crate::{
    capability::BindingCapabilityDescriptor,
    controller::SpaceId,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, PaneBehavior,
        PaneTopology, PersistedSessionPolicy, SelectionPublicationPolicy, TerminalProgressPolicy,
        TerminalResidency,
    },
    terminal::BackendPanePolicy,
};
use bootty_host::remote::RemoteHost;

use super::HerdrBackend;
#[cfg(feature = "terminal-runtime")]
use super::{HerdrPanePolicy, herdr_capabilities};

pub struct HerdrProvider;

impl MuxBackendProvider for HerdrProvider {
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Herdr
    }

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        match &config.remote {
            Some(remote) => Box::new(HerdrBackend::for_remote(RemoteHost::new(remote.clone()))),
            None => Box::new(HerdrBackend::new()),
        }
    }
}

#[cfg(feature = "terminal-runtime")]
impl MuxAppBackendProvider for HerdrProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: true,
                resize_cached_terminals: true,
            },
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(HerdrPanePolicy::new(
            config.remote.clone().map(RemoteHost::new),
        ))
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        herdr_capabilities(scope)
    }
}

crate::register_mux_backend!(HerdrProvider);
