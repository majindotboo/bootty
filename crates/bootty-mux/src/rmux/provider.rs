use std::path::Path;

use crate::remote_space::RemoteSpaceBackend;
use crate::{MuxBackendKind, MuxBindingConfig};
use crate::{
    backend::MuxBackend,
    provider::{MuxBackendProvider, MuxCommandDispatch},
};
#[cfg(feature = "app")]
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
use bootty_host::ssh::SshRemote;

use crate::rmux::RmuxBackend;
#[cfg(feature = "app")]
use crate::rmux::{RmuxPanePolicy, remote::RemoteRmuxBackend, rmux_capabilities};

pub struct RmuxProvider;

impl MuxBackendProvider for RmuxProvider {
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Rmux
    }

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        if let (Some(remote), Some(space_id)) = (&config.remote, &config.remote_space_id) {
            return Box::new(RemoteSpaceBackend::new(
                SshRemote::new(remote.clone()),
                space_id.clone(),
                MuxBackendKind::Rmux,
            ));
        }
        #[cfg(feature = "app")]
        if let Some(remote) = &config.remote {
            return Box::new(RemoteRmuxBackend::new(SshRemote::new(remote.clone())));
        }
        Box::new(RmuxBackend::new())
    }
}

#[cfg(feature = "app")]
impl MuxAppBackendProvider for RmuxProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::BackendReconciled,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::AfterEmptyInitialSnapshot,
            generated_session_names: GeneratedSessionNamePolicy::PreserveBackend,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::PersistBeforePublish,
        }
    }

    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(RmuxPanePolicy::new(
            config.remote.clone().map(SshRemote::new),
        ))
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        rmux_capabilities(scope)
    }
}

crate::register_mux_backend!(RmuxProvider);

pub fn link() {}
