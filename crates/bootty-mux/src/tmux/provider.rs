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

use crate::tmux::TmuxBackend;
#[cfg(feature = "app")]
use crate::tmux::{TmuxControlRunner, TmuxPanePolicy, tmux_capabilities};

pub struct TmuxProvider;

impl MuxBackendProvider for TmuxProvider {
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Tmux
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
                MuxBackendKind::Tmux,
            ));
        }
        #[cfg(feature = "app")]
        {
            Box::new(match &config.remote {
                Some(remote) => TmuxBackend::with_runner(
                    "tmux",
                    TmuxControlRunner::for_remote(SshRemote::new(remote.clone())),
                ),
                None => {
                    TmuxBackend::for_identity(bootty_identity::ApplicationIdentity::for_process())
                }
            })
        }
        #[cfg(not(feature = "app"))]
        Box::new(TmuxBackend::new())
    }
}

#[cfg(feature = "app")]
impl MuxAppBackendProvider for TmuxProvider {
    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: true,
                resize_cached_terminals: true,
            },
            progress: TerminalProgressPolicy::BackendSnapshot,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(TmuxPanePolicy::new(
            config.remote.clone().map(SshRemote::new),
        ))
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        tmux_capabilities(scope)
    }
}

crate::register_mux_backend!(TmuxProvider);

pub fn link() {}
