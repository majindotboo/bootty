use std::{collections::HashMap, path::Path, sync::Arc};

use crate::{MuxBackendKind, MuxBindingConfig};
use anyhow::{Context as _, Result, bail};
#[cfg(feature = "terminal-runtime")]
use strum::IntoEnumIterator;

use crate::backend::MuxBackend;
#[cfg(feature = "terminal-runtime")]
use crate::command::MuxCommand;
#[cfg(feature = "terminal-runtime")]
use crate::{
    capability::{
        BindingCapabilityDescriptor, BindingOperationAvailability, BindingOperationOutcome,
    },
    controller::SpaceId,
    terminal::BackendPanePolicy,
};

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneTopology {
    ProcessLocal,
    BackendReconciled,
    Attach,
}

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneBehavior {
    pub topology: PaneTopology,
    pub cache_terminals: bool,
    pub resize_cached_terminals: bool,
}

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalProgressPolicy {
    TerminalOsc,
    BackendSnapshot,
}

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistedSessionPolicy {
    Immediate,
    AfterEmptyInitialSnapshot,
    Never,
}

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratedSessionNamePolicy {
    Reconcile,
    PreserveBackend,
}

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalResidency {
    WorkspaceShared,
    BindingScoped,
}

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionPublicationPolicy {
    Direct,
    PersistBeforePublish,
}

#[cfg(feature = "terminal-runtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MuxAppBackendPolicy {
    pub panes: PaneBehavior,
    pub progress: TerminalProgressPolicy,
    pub persisted_sessions: PersistedSessionPolicy,
    pub generated_session_names: GeneratedSessionNamePolicy,
    pub terminal_residency: TerminalResidency,
    pub selection_publication: SelectionPublicationPolicy,
}

/// Selects how the controller invokes a provider.
///
/// `CallerThread` providers own their command lifecycle in the controller thread.
/// `WorkerThread` providers run through the controller's worker path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MuxCommandDispatch {
    CallerThread,
    #[default]
    WorkerThread,
}

/// One complete backend implementation.
pub trait MuxBackendProvider: Send + Sync {
    fn kind(&self) -> MuxBackendKind;

    fn command_dispatch(&self) -> MuxCommandDispatch;

    fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend>;
}

#[cfg(feature = "terminal-runtime")]
pub trait MuxAppBackendProvider: MuxBackendProvider {
    fn build_pane_policy(&self, config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy>;

    fn app_policy(&self) -> MuxAppBackendPolicy;

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor;
}

#[derive(Clone)]
pub struct MuxBackendEntry {
    core: Arc<dyn MuxBackendProvider>,
    #[cfg(feature = "terminal-runtime")]
    app: Option<Arc<dyn MuxAppBackendProvider>>,
}

impl MuxBackendEntry {
    #[cfg(feature = "terminal-runtime")]
    pub fn from_app_provider<P>(provider: Arc<P>) -> Self
    where
        P: MuxAppBackendProvider + 'static,
    {
        let core: Arc<dyn MuxBackendProvider> = provider.clone();
        let app: Arc<dyn MuxAppBackendProvider> = provider;
        Self {
            core,
            app: Some(app),
        }
    }

    pub fn from_core_provider(provider: Arc<dyn MuxBackendProvider>) -> Self {
        Self {
            core: provider,
            #[cfg(feature = "terminal-runtime")]
            app: None,
        }
    }
}

pub struct MuxBackendRegistration {
    pub constructor: fn() -> MuxBackendEntry,
}

inventory::collect!(MuxBackendRegistration);

#[cfg(feature = "terminal-runtime")]
#[macro_export]
macro_rules! register_mux_backend {
    ($provider:expr) => {
        inventory::submit! {
            $crate::provider::MuxBackendRegistration {
                constructor: || {
                    $crate::provider::MuxBackendEntry::from_app_provider(
                        std::sync::Arc::new($provider),
                    )
                },
            }
        }
    };
}

#[cfg(not(feature = "terminal-runtime"))]
#[macro_export]
macro_rules! register_mux_backend {
    ($provider:expr) => {
        inventory::submit! {
            $crate::provider::MuxBackendRegistration {
                constructor: || {
                    $crate::provider::MuxBackendEntry::from_core_provider(
                        std::sync::Arc::new($provider),
                    )
                },
            }
        }
    };
}

#[derive(Clone)]
pub struct MuxBackendRegistry {
    providers: Arc<HashMap<MuxBackendKind, MuxBackendEntry>>,
}

impl MuxBackendRegistry {
    /// # Errors
    /// Returns an error for duplicate providers, missing required backends, or missing app policies.
    pub fn collect(required: impl IntoIterator<Item = MuxBackendKind>) -> Result<Self> {
        Self::from_entries(
            inventory::iter::<MuxBackendRegistration>
                .into_iter()
                .map(|registration| (registration.constructor)()),
            required,
            cfg!(feature = "terminal-runtime"),
        )
    }
    /// # Errors
    /// Returns an error for duplicate providers or missing required backends.
    pub fn from_core_providers(
        providers: impl IntoIterator<Item = Arc<dyn MuxBackendProvider>>,
        required: impl IntoIterator<Item = MuxBackendKind>,
    ) -> Result<Self> {
        Self::from_entries(
            providers
                .into_iter()
                .map(MuxBackendEntry::from_core_provider),
            required,
            false,
        )
    }

    #[cfg(feature = "terminal-runtime")]
    /// # Errors
    /// Returns an error for duplicate providers or missing required backends.
    pub fn from_app_providers<P>(
        providers: impl IntoIterator<Item = Arc<P>>,
        required: impl IntoIterator<Item = MuxBackendKind>,
    ) -> Result<Self>
    where
        P: MuxAppBackendProvider + 'static,
    {
        Self::from_entries(
            providers
                .into_iter()
                .map(MuxBackendEntry::from_app_provider),
            required,
            true,
        )
    }

    fn from_entries(
        entries: impl IntoIterator<Item = MuxBackendEntry>,
        required: impl IntoIterator<Item = MuxBackendKind>,
        require_app: bool,
    ) -> Result<Self> {
        #[cfg(not(feature = "terminal-runtime"))]
        let _ = require_app;
        let mut by_kind = HashMap::new();
        for entry in entries {
            let kind = entry.core.kind();
            #[cfg(feature = "terminal-runtime")]
            if require_app && entry.app.is_none() {
                bail!("missing app mux backend provider for {kind:?}")
            }
            if by_kind.insert(kind, entry).is_some() {
                bail!("duplicate mux backend provider for {kind:?}")
            }
        }
        for kind in required {
            if !by_kind.contains_key(&kind) {
                bail!("missing mux backend provider for {kind:?}")
            }
        }
        Ok(Self {
            providers: Arc::new(by_kind),
        })
    }

    #[must_use]
    pub fn selected_kind(&self, config: &MuxBindingConfig) -> MuxBackendKind {
        selected_backend(config)
    }

    /// # Errors
    /// Returns an error when the selected backend is not registered.
    pub fn build_backend(
        &self,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Result<Box<dyn MuxBackend>> {
        self.build_backend_for_kind(self.selected_kind(config), config, workspace)
    }

    /// # Errors
    /// Returns an error when the requested backend is not registered.
    pub fn build_backend_for_kind(
        &self,
        kind: MuxBackendKind,
        config: &MuxBindingConfig,
        workspace: Option<&Path>,
    ) -> Result<Box<dyn MuxBackend>> {
        let provider = self
            .providers
            .get(&kind)
            .with_context(|| format!("mux backend {kind:?} is not registered"))?;
        Ok(provider.core.build_backend(config, workspace))
    }

    #[must_use]
    pub fn command_dispatch(&self, config: &MuxBindingConfig) -> Option<MuxCommandDispatch> {
        self.providers
            .get(&self.selected_kind(config))
            .map(|provider| provider.core.command_dispatch())
    }

    #[cfg(feature = "terminal-runtime")]
    /// # Errors
    /// Returns an error if the registered providers do not cover every desktop backend.
    pub fn desktop() -> Result<Self> {
        Self::collect(MuxBackendKind::iter())
    }

    #[cfg(feature = "terminal-runtime")]
    /// # Errors
    /// Returns an error when the selected backend has no registered app provider.
    pub fn app_provider(
        &self,
        config: &MuxBindingConfig,
    ) -> Result<Arc<dyn MuxAppBackendProvider>> {
        let kind = self.selected_kind(config);
        self.providers
            .get(&kind)
            .and_then(|provider| provider.app.clone())
            .with_context(|| format!("app mux backend {kind:?} is not registered"))
    }

    #[cfg(feature = "terminal-runtime")]
    #[must_use]
    pub fn capabilities(
        &self,
        config: &MuxBindingConfig,
        scope: SpaceId,
    ) -> Option<BindingCapabilityDescriptor> {
        self.providers
            .get(&self.selected_kind(config))?
            .app
            .as_ref()
            .map(|provider| provider.capabilities(scope))
    }

    #[cfg(feature = "terminal-runtime")]
    pub fn execute_checked(
        &self,
        config: &MuxBindingConfig,
        scope: SpaceId,
        backend: &mut dyn MuxBackend,
        command: MuxCommand,
    ) -> BindingOperationOutcome<Result<()>> {
        let Some(descriptor) = self.capabilities(config, scope) else {
            return BindingOperationOutcome::Unavailable;
        };
        descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || backend.execute(command),
        )
    }
}

fn resolve_backend(backend: MuxBackendKind, remote: bool, windows: bool) -> MuxBackendKind {
    if windows && backend == MuxBackendKind::Tmux && !remote {
        return MuxBackendKind::Native;
    }
    backend
}

#[must_use]
pub fn selected_backend(config: &MuxBindingConfig) -> MuxBackendKind {
    resolve_backend(config.backend, config.remote.is_some(), cfg!(windows))
}
