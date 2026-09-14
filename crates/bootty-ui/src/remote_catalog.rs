use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, TryRecvError},
};

use anyhow::Result;
use bootty_config::config::{MultiplexerBackendConfig, SshProfileConfig};
use bootty_host::{CancellableCommandRunner, CommandCancellation};
use bootty_mux::RemoteSpaceSummary;

use crate::error_catalog::ErrorNotice;

pub enum RemoteCatalogResult {
    Listed(Vec<RemoteSpaceSummary>),
    Created {
        selected: RemoteSpaceSummary,
        refreshed: Result<Vec<RemoteSpaceSummary>, String>,
    },
}

#[derive(Debug)]
pub struct RemoteCatalogTask {
    pub(crate) profile_id: String,
    receiver: mpsc::Receiver<Result<RemoteCatalogResult, String>>,
    cancellation: CommandCancellation,
}

impl RemoteCatalogTask {
    pub(crate) fn start(
        profile_id: String,
        profile: SshProfileConfig,
        create: Option<(String, MultiplexerBackendConfig)>,
    ) -> Result<Self, String> {
        let permit = RemoteWorkerPermit::acquire()
            .ok_or_else(|| ErrorNotice::RemoteSpaceOperationStopping.to_string())?;
        let (sender, receiver) = mpsc::channel();
        let cancellation = CommandCancellation::default();
        let runner = CancellableCommandRunner::new(cancellation.clone());
        std::thread::spawn(move || {
            let _permit = permit;
            let result = if let Some((name, backend)) = create {
                bootty_mux::remote_space::create_remote_with_runner(
                    &profile, &name, backend, &runner,
                )
                .map(|selected| RemoteCatalogResult::Created {
                    selected,
                    refreshed: bootty_mux::remote_space::list_remote_with_runner(&profile, &runner)
                        .map_err(|error| error.to_string()),
                })
            } else {
                bootty_mux::remote_space::list_remote_with_runner(&profile, &runner)
                    .map(RemoteCatalogResult::Listed)
            }
            .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        Ok(Self {
            profile_id,
            receiver,
            cancellation,
        })
    }

    pub(crate) fn try_recv(&self) -> Option<Result<RemoteCatalogResult, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err(ErrorNotice::RemoteSpaceTaskStopped.to_string()))
            }
        }
    }
}

impl Drop for RemoteCatalogTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

static REMOTE_CATALOG_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct RemoteWorkerPermit;

impl RemoteWorkerPermit {
    fn acquire() -> Option<Self> {
        REMOTE_CATALOG_WORKER_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for RemoteWorkerPermit {
    fn drop(&mut self) {
        REMOTE_CATALOG_WORKER_ACTIVE.store(false, Ordering::Release);
    }
}

/// Distro discovery is independent from the SSH Space catalog and never starts a distro.
pub struct WslDiscoveryTask {
    receiver: mpsc::Receiver<Result<Vec<bootty_config::config::WslDistribution>, String>>,
    cancellation: CommandCancellation,
}
impl WslDiscoveryTask {
    pub(crate) fn start(repaint: bootty_mux::RepaintHandle) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancellation = CommandCancellation::default();
        let runner = CancellableCommandRunner::with_deadline(cancellation.clone(), {
            let now = std::time::Instant::now();
            now.checked_add(std::time::Duration::from_secs(15))
                .unwrap_or(now)
        });
        std::thread::spawn(move || {
            let result =
                bootty_host::wsl::distributions(&runner).map_err(|error| error.to_string());
            let _ = sender.send(result);
            repaint();
        });
        Self {
            receiver,
            cancellation,
        }
    }
    pub(crate) fn try_recv(
        &self,
    ) -> Option<Result<Vec<bootty_config::config::WslDistribution>, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("WSL discovery stopped".to_owned())),
        }
    }
}
impl Drop for WslDiscoveryTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
