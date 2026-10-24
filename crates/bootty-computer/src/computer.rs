use crate::{
    ComputerAccess, ComputerAction, ComputerError, ComputerResult, ComputerStatus, ComputerTarget,
    Permission, PermissionStatus, control, desktop::Desktop,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Most app icons one call renders; callers batch larger sets.
pub const MAX_ICON_REQUEST: usize = 16;
const ICON_SIDE: u32 = 48;
// Accessibility and capture calls can stall inside other apps; no action waits longer than this.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(20);

/// Local desktop control. Native work runs on a blocking worker, never on the caller's thread.
#[derive(Clone)]
pub struct Computer {
    owner: Arc<Owner>,
}

#[derive(Default)]
struct Owner {
    retired: AtomicBool,
    backends: Mutex<BTreeMap<Option<String>, Arc<Backend>>>,
    supplied: Option<Arc<dyn Desktop>>,
}
struct Backend {
    desktop: Arc<dyn Desktop>,
    gate: Mutex<()>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        for backend in self
            .backends
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
        {
            std::thread::spawn(move || {
                let _gate = backend.gate.lock().unwrap_or_else(PoisonError::into_inner);
                _ = backend.desktop.release_inputs();
            });
        }
    }
}

// A dropped/expired future cannot leave input down, including when its blocking worker is late.
struct PendingInput {
    backend: Arc<Backend>,
    cancelled: Arc<AtomicBool>,
    armed: bool,
}
impl Drop for PendingInput {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Release);
            let backend = self.backend.clone();
            std::thread::spawn(move || {
                let _gate = backend.gate.lock().unwrap_or_else(PoisonError::into_inner);
                _ = backend.desktop.release_inputs();
            });
        }
    }
}

impl std::fmt::Debug for Computer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Computer").finish_non_exhaustive()
    }
}

impl Default for Computer {
    fn default() -> Self {
        Self::new()
    }
}

impl Computer {
    /// Control this machine's desktop.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner: Arc::new(Owner::default()),
        }
    }

    #[must_use]
    pub fn with_desktop(desktop: Arc<dyn Desktop>) -> Self {
        Self {
            owner: Arc::new(Owner {
                supplied: Some(desktop),
                backends: Mutex::new(BTreeMap::new()),
                retired: AtomicBool::new(false),
            }),
        }
    }

    /// Checks current OS permission state without presenting prompts.
    ///
    /// # Errors
    /// Returns timeout errors.
    pub async fn status(&self) -> Result<ComputerStatus, ComputerError> {
        self.run(|desktop| Ok(desktop.status())).await
    }

    /// Lists observable application windows without prompting for OS access.
    ///
    /// # Errors
    /// Returns timeout errors.
    pub async fn targets(&self) -> Result<Vec<ComputerTarget>, ComputerError> {
        let _default = self.backend(None)?;
        let backends: Vec<_> = self
            .owner
            .backends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        let mut targets = Vec::new();
        for backend in backends {
            targets.extend(
                self.run_backend(backend, |desktop| Ok(control::targets(desktop)))
                    .await?,
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        targets.retain(|target| {
            seen.insert((
                target.window_id,
                target.native_id.clone(),
                target.backend_id.clone(),
            ))
        });
        Ok(targets)
    }

    /// Discover a local X11 display without changing the host process's environment or focus.
    /// # Errors
    /// Returns invalid-display, unsupported-platform and native errors.
    pub async fn targets_on_display(
        &self,
        display: &str,
    ) -> Result<Vec<ComputerTarget>, ComputerError> {
        let backend = self.backend(Some(display))?;
        self.run_backend(backend, |desktop| Ok(control::targets(desktop)))
            .await
    }

    /// Revoke held input without blocking a UI owner. Only this owner's input is released.
    pub fn release_inputs(&self) {
        let backends: Vec<_> = self
            .owner
            .backends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for backend in backends {
            std::thread::spawn(move || {
                let _gate = backend.gate.lock().unwrap_or_else(PoisonError::into_inner);
                _ = backend.desktop.release_inputs();
            });
        }
    }

    /// Wait for every owned backend to release its held input. Use during bounded host shutdown.
    /// # Errors
    /// Returns native release and timeout errors; failed releases retain their ownership.
    pub async fn release_all_inputs(&self) -> Result<(), ComputerError> {
        let backends: Vec<_> = self
            .owner
            .backends
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        let mut error = None;
        for backend in backends {
            if let Err(failed) = self
                .run_backend(backend, |desktop| desktop.release_inputs())
                .await
            {
                error = Some(failed);
            }
        }
        error.map_or(Ok(()), Err)
    }

    /// Retire this owner and schedule cleanup without blocking its UI owner.
    pub fn retire(&self) {
        self.owner.retired.store(true, Ordering::Release);
        self.release_inputs();
    }

    /// Retire this owner before awaiting release, so queued work cannot start another hold.
    /// # Errors
    /// Returns native cleanup and timeout errors.
    pub async fn shutdown(&self) -> Result<(), ComputerError> {
        self.owner.retired.store(true, Ordering::Release);
        self.release_all_inputs().await
    }

    fn backend(&self, display: Option<&str>) -> Result<Arc<Backend>, ComputerError> {
        if self.owner.retired.load(Ordering::Acquire) {
            return Err(ComputerError::TargetUnavailable);
        }
        #[cfg(target_os = "linux")]
        let default_display = std::env::var("DISPLAY")
            .ok()
            .filter(|display| crate::model::valid_display(display));
        #[cfg(target_os = "linux")]
        let display = display.or(default_display.as_deref());
        if display.is_some_and(|display| !crate::model::valid_display(display)) {
            return Err(ComputerError::InvalidTarget);
        }
        let mut backends = self
            .owner
            .backends
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(backend) = backends.get(&display.map(str::to_owned)) {
            return Ok(backend.clone());
        }
        // A bounded set of local displays; grow this only when a real multi-display host needs it.
        if backends.len() >= 8 {
            return Err(ComputerError::InvalidAction(
                "too many display connections".into(),
            ));
        }
        let desktop = if let Some(desktop) = &self.owner.supplied {
            desktop.clone()
        } else if let Some(display) = display {
            crate::platform::on_display(display)?
        } else {
            crate::platform::native()
        };
        let backend = Arc::new(Backend {
            desktop,
            gate: Mutex::new(()),
        });
        backends.insert(display.map(str::to_owned), backend.clone());
        drop(backends);
        Ok(backend)
    }

    /// Small PNG icons for apps, keyed by bundle id; unknown apps are omitted. Asks for at most
    /// [`MAX_ICON_REQUEST`] apps and needs no OS permission.
    ///
    /// # Errors
    /// Returns oversized-request and timeout errors.
    pub async fn icons(
        &self,
        bundle_ids: &[String],
    ) -> Result<BTreeMap<String, Vec<u8>>, ComputerError> {
        if bundle_ids.len() > MAX_ICON_REQUEST {
            return Err(ComputerError::InvalidAction(
                "too many app icons requested".into(),
            ));
        }
        let bundle_ids = bundle_ids.to_vec();
        self.run(move |desktop| {
            Ok(bundle_ids
                .into_iter()
                .filter(|bundle_id| !bundle_id.is_empty() && bundle_id.len() <= 256)
                .filter_map(|bundle_id| {
                    let png = control::icon_png(desktop, &bundle_id, ICON_SIDE)?;
                    Some((bundle_id, png))
                })
                .collect())
        })
        .await
    }

    /// Posts one action; failures are never automatically retried.
    ///
    /// # Errors
    /// Refuses disabled access, invalid actions, denied permissions and secure input.
    /// Also returns unsupported-platform, platform and timeout errors.
    pub async fn execute(
        &self,
        access: &ComputerAccess,
        target: &ComputerTarget,
        action: &ComputerAction,
    ) -> Result<ComputerResult, ComputerError> {
        let backend = match self.backend(target.display.as_deref()) {
            Ok(backend) => backend,
            Err(error) => {
                self.release_all_inputs().await?;
                return Err(error);
            }
        };
        let (access, work_target, work_action) = (*access, target.clone(), action.clone());
        let owner = self.owner.clone();
        let result = self
            .run_backend(backend, move |desktop| {
                if owner.retired.load(Ordering::Acquire) {
                    return Err(ComputerError::TargetUnavailable);
                }
                if !access.enabled() {
                    return Err(ComputerError::Disabled);
                }
                work_target.validate_action(&work_action)?;
                let status = desktop.status();
                let (permission, granted) = if work_action.is_capture() {
                    (Permission::ScreenRecording, status.screen_recording)
                } else {
                    (Permission::Accessibility, status.accessibility)
                };
                match granted {
                    PermissionStatus::Granted => {}
                    PermissionStatus::NotGranted => {
                        return Err(ComputerError::PermissionDenied(permission));
                    }
                    PermissionStatus::Unsupported => return Err(ComputerError::Unsupported),
                }
                if status.secure_input {
                    return Err(ComputerError::SecureInput);
                }
                let result = control::execute(desktop, &work_target, &work_action)?;
                result.validate(&work_target, &work_action)?;
                Ok(result)
            })
            .await;
        if result.is_err() {
            self.release_all_inputs().await?;
        }
        result
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&dyn Desktop) -> Result<T, ComputerError> + Send + 'static,
    ) -> Result<T, ComputerError> {
        self.run_backend(self.backend(None)?, work).await
    }

    async fn run_backend<T: Send + 'static>(
        &self,
        backend: Arc<Backend>,
        work: impl FnOnce(&dyn Desktop) -> Result<T, ComputerError> + Send + 'static,
    ) -> Result<T, ComputerError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut pending = PendingInput {
            backend: backend.clone(),
            cancelled: cancelled.clone(),
            armed: true,
        };
        let task = tokio::task::spawn_blocking(move || {
            let _gate = backend.gate.lock().unwrap_or_else(PoisonError::into_inner);
            if cancelled.load(Ordering::Acquire) {
                return Err(ComputerError::Timeout);
            }
            let result = work(backend.desktop.as_ref());
            if result.is_err() || cancelled.load(Ordering::Acquire) {
                backend.desktop.release_inputs()?;
            }
            result
        });
        let result = tokio::time::timeout(OPERATION_TIMEOUT, task)
            .await
            .map_err(|_| ComputerError::Timeout)?
            .map_err(|_| ComputerError::Platform("computer use stopped unexpectedly".into()))?;
        pending.armed = false;
        result
    }
}
