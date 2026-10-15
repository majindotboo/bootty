use crate::{
    ComputerAccess, ComputerAction, ComputerError, ComputerResult, ComputerStatus, ComputerTarget,
    Permission, PermissionStatus, control, desktop::Desktop,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// Most app icons one call renders; callers batch larger sets.
pub const MAX_ICON_REQUEST: usize = 16;
const ICON_SIDE: u32 = 48;
// Accessibility and capture calls can stall inside other apps; no action waits longer than this.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(20);

/// Local desktop control. Native work runs on a blocking worker, never on the caller's thread.
#[derive(Clone)]
pub struct Computer {
    desktop: Arc<dyn Desktop>,
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
        Self::with_desktop(crate::platform::native())
    }

    #[must_use]
    pub fn with_desktop(desktop: Arc<dyn Desktop>) -> Self {
        Self { desktop }
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
        self.run(|desktop| Ok(control::targets(desktop))).await
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
        if !access.enabled() {
            return Err(ComputerError::Disabled);
        }
        target.validate_action(action)?;
        let status = self.status().await?;
        let (permission, granted) = if action.is_capture() {
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
        let (work_target, work_action) = (target.clone(), action.clone());
        let result = self
            .run(move |desktop| control::execute(desktop, &work_target, &work_action))
            .await?;
        result.validate(target, action)?;
        Ok(result)
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&dyn Desktop) -> Result<T, ComputerError> + Send + 'static,
    ) -> Result<T, ComputerError> {
        let desktop = self.desktop.clone();
        let task = tokio::task::spawn_blocking(move || work(desktop.as_ref()));
        tokio::time::timeout(OPERATION_TIMEOUT, task)
            .await
            .map_err(|_| ComputerError::Timeout)?
            .map_err(|_| ComputerError::Platform("computer use stopped unexpectedly".into()))?
    }
}
