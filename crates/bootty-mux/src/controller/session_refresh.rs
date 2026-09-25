use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::{
    MuxBackendKind, MuxBindingConfig, provider::MuxBackendRegistry, snapshot::MuxSnapshot,
};

use super::RepaintHandle;

type RefreshResult = Result<(MuxBackendKind, MuxSnapshot), String>;

struct RefreshRequest {
    config: MuxBindingConfig,
    result: mpsc::Sender<RefreshResult>,
}

struct PendingRefresh {
    config: MuxBindingConfig,
    valid: bool,
    result: mpsc::Receiver<RefreshResult>,
}

#[derive(Default)]
pub(super) struct SessionRefresh {
    worker: Option<mpsc::Sender<RefreshRequest>>,
    pending: Option<PendingRefresh>,
    last_started: Option<Instant>,
}

impl SessionRefresh {
    pub const fn request_soon(&mut self) {
        self.last_started = None;
    }

    pub const fn invalidate(&mut self) {
        self.request_soon();
        // A newer command supersedes the result, but the worker is still occupied.
        if let Some(pending) = &mut self.pending {
            pending.valid = false;
        }
    }

    pub fn is_due(&self, interval: Duration) -> bool {
        self.last_started
            .is_none_or(|last| last.elapsed() >= interval)
    }

    pub fn record_started(&mut self) {
        self.last_started = Some(Instant::now());
    }

    pub fn poll(&mut self, config: &MuxBindingConfig) -> Option<RefreshResult> {
        let pending = self.pending.as_ref()?;
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.worker = None;
                self.pending = None;
                self.request_soon();
                return Some(Err("mux session refresh worker stopped".to_owned()));
            }
        };
        let publish = pending.valid && pending.config == *config;
        self.pending = None;
        if !publish {
            self.request_soon();
        }
        publish.then_some(result)
    }

    pub fn request(
        &mut self,
        registry: &Arc<MuxBackendRegistry>,
        workspace: Option<&Path>,
        repaint: &RepaintHandle,
        config: &MuxBindingConfig,
    ) -> Result<(), String> {
        if self.pending.is_some() {
            return Ok(());
        }
        let worker = self.worker.get_or_insert_with(|| {
            Self::start_worker(
                Arc::clone(registry),
                workspace.map(Path::to_owned),
                repaint.clone(),
            )
        });
        let (result, receiver) = mpsc::channel();
        if worker
            .send(RefreshRequest {
                config: config.clone(),
                result,
            })
            .is_err()
        {
            self.worker = None;
            return Err("mux session refresh worker stopped".to_owned());
        }
        self.pending = Some(PendingRefresh {
            config: config.clone(),
            valid: true,
            result: receiver,
        });
        self.record_started();
        Ok(())
    }

    fn start_worker(
        registry: Arc<MuxBackendRegistry>,
        workspace: Option<PathBuf>,
        repaint: RepaintHandle,
    ) -> mpsc::Sender<RefreshRequest> {
        let (sender, receiver) = mpsc::channel::<RefreshRequest>();
        thread::spawn(move || {
            while let Ok(request) = receiver.recv() {
                let backend_kind = registry.selected_kind(&request.config);
                let result = registry
                    .build_backend(&request.config, workspace.as_deref())
                    .and_then(|backend| backend.snapshot())
                    .map(|snapshot| (backend_kind, snapshot))
                    .map_err(|error| error.to_string());
                if request.result.send(result).is_err() {
                    break;
                }
                repaint();
            }
        });
        sender
    }
}
