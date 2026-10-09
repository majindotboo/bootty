use crate::{
    ComputerAccess, ComputerAction, ComputerError, ComputerResult, ComputerStatus, ComputerTarget,
    Permission, PermissionStatus,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const RESPONSE_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Computer {
    helper_path: PathBuf,
}

#[derive(Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum Request<'a> {
    Status,
    Targets,
    Execute {
        enabled: bool,
        target: &'a ComputerTarget,
        #[serde(flatten)]
        action: &'a ComputerAction,
    },
}

#[derive(Deserialize)]
struct Response<T> {
    value: Option<T>,
    error: Option<HelperError>,
}

#[derive(Deserialize)]
struct HelperError {
    code: String,
    message: String,
}

impl Computer {
    #[must_use]
    pub const fn new(helper_path: PathBuf) -> Self {
        Self { helper_path }
    }

    /// Checks current OS permission state without presenting prompts.
    ///
    /// # Errors
    /// Returns helper launch, protocol or timeout errors.
    pub async fn status(&self) -> Result<ComputerStatus, ComputerError> {
        if !cfg!(target_os = "macos") {
            return Ok(ComputerStatus {
                accessibility: PermissionStatus::Unsupported,
                screen_recording: PermissionStatus::Unsupported,
                secure_input: false,
            });
        }
        self.request(&Request::Status).await
    }

    /// Lists observable application windows without prompting for OS access.
    ///
    /// # Errors
    /// Returns unsupported-platform, helper launch, protocol or timeout errors.
    pub async fn targets(&self) -> Result<Vec<ComputerTarget>, ComputerError> {
        self.request(&Request::Targets).await
    }

    /// Posts one action; failures are never automatically retried.
    ///
    /// # Errors
    /// Refuses disabled access, invalid actions, denied permissions and secure input.
    /// Also returns unsupported-platform, helper launch, protocol and timeout errors.
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
        let result: ComputerResult = self
            .request(&Request::Execute {
                enabled: true,
                target,
                action,
            })
            .await?;
        result.validate(target, action)?;
        Ok(result)
    }

    async fn request<T: DeserializeOwned>(
        &self,
        request: &Request<'_>,
    ) -> Result<T, ComputerError> {
        if !cfg!(target_os = "macos") {
            return Err(ComputerError::Unsupported);
        }
        let mut child = tokio::process::Command::new(&self.helper_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let operation = async {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| ComputerError::Helper("helper stdin unavailable".into()))?;
            stdin.write_all(&serde_json::to_vec(request)?).await?;
            drop(stdin);
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| ComputerError::Helper("helper stdout unavailable".into()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| ComputerError::Helper("helper stderr unavailable".into()))?;
            let (output, _diagnostic, status) = tokio::try_join!(
                read_bounded(stdout, RESPONSE_LIMIT),
                read_bounded(stderr, 8192),
                child.wait()
            )?;
            if !status.success() {
                return Err(ComputerError::Helper("helper process failed".into()));
            }
            let response: Response<T> = serde_json::from_slice(&output)?;
            if let Some(error) = response.error {
                return Err(match error.code.as_str() {
                    "disabled" => ComputerError::Disabled,
                    "unsupported" => ComputerError::Unsupported,
                    "accessibility_denied" => {
                        ComputerError::PermissionDenied(Permission::Accessibility)
                    }
                    "screen_recording_denied" => {
                        ComputerError::PermissionDenied(Permission::ScreenRecording)
                    }
                    "invalid_target" => ComputerError::InvalidTarget,
                    "target_unavailable" => ComputerError::TargetUnavailable,
                    "stale_target" => ComputerError::StaleTarget,
                    "target_not_focused" => ComputerError::TargetNotFocused,
                    "secure_input" => ComputerError::SecureInput,
                    "invalid_action" => ComputerError::InvalidAction(error.message),
                    _ => ComputerError::Helper(error.message),
                });
            }
            response
                .value
                .ok_or_else(|| ComputerError::Helper("helper returned no value".into()))
        };
        tokio::time::timeout(Duration::from_secs(20), operation)
            .await
            .map_err(|_| ComputerError::Timeout)?
    }
}

async fn read_bounded(
    reader: impl AsyncRead + Unpin,
    limit: u64,
) -> Result<Vec<u8>, std::io::Error> {
    let mut data = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut data)
        .await?;
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > limit {
        return Err(std::io::Error::other("helper response exceeds size limit"));
    }
    Ok(data)
}

/// Packaging installs this inside the app before code signing, giving permission checks
/// and input actions the same stable identity. Runtime never writes an executable.
///
/// # Errors
/// Returns unsupported-platform or filesystem errors.
#[cfg(target_os = "macos")]
pub fn install_helper(destination: &Path) -> Result<(), ComputerError> {
    use std::os::unix::fs::PermissionsExt;
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/bootty-computer"));
    std::fs::write(destination, bytes)?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Packaging installs this inside the app before code signing, giving permission checks
/// and input actions the same stable identity. Runtime never writes an executable.
///
/// # Errors
/// Returns unsupported-platform or filesystem errors.
#[cfg(not(target_os = "macos"))]
pub const fn install_helper(_: &Path) -> Result<(), ComputerError> {
    Err(ComputerError::Unsupported)
}
