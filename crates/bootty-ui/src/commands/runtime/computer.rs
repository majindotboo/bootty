use std::{path::Path, time::Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_computer::{
    Computer, ComputerAccess, ComputerError, ComputerResult, ComputerResultSnapshot,
};
use bootty_control::{CommandCancellation, CommandOutcome, CommandWarning};
use bootty_write::{CommitError, CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};

use super::CommandDispatch;
use crate::{AppState, commands::ComputerCommand};

mod host_capture;

/// The shared browser request has already checked feature policy. OS grants are checked here.
#[cfg(target_os = "macos")]
pub fn browser_capture_candidate(
    window: std::num::NonZeroU32,
    region: &bootty_computer::HostCaptureRegion,
    access: ComputerAccess,
) -> Result<ComputerResult, ComputerError> {
    let executable = std::env::current_exe()?;
    let helper = executable
        .parent()
        .ok_or_else(|| ComputerError::Helper("application directory unavailable".into()))?
        .join("bootty-computer");
    if !helper.is_file() {
        return Err(ComputerError::Helper(
            "Computer use requires the signed helper in a packaged Bootty app".into(),
        ));
    }
    let computer = Computer::new(helper);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(host_capture::capture_host_candidate(
        &computer,
        window,
        &access,
        Some(region),
    ))
}

/// Called only after the UI owner admits publication of its still-current page candidate.
#[cfg(target_os = "macos")]
pub fn publish_browser_capture(result: ComputerResult, directory: &Path) -> CommandOutcome {
    let run = || {
        let destination = host_capture::host_capture_destination(directory)?;
        captured_result(result, Some(destination))
    };
    run().unwrap_or_else(computer_error_outcome)
}

impl super::CommandRuntime {
    /// Accepted capture revocation cancels only candidates that have not admitted publication.
    pub(crate) fn sync_browser_capture_policy(
        &self,
        policy: bootty_config::config::ComputerConfig,
    ) {
        if !policy.enabled || !policy.capture_enabled {
            let Some(descriptor) = self.catalog.describe("browser.capture") else {
                return;
            };
            for pending in &self.pending {
                if pending.label == descriptor.title && !pending.user_initiated_annotation_capture {
                    _ = pending.cancellation.cancel();
                }
            }
        }
    }
}

impl AppState {
    pub(super) fn dispatch_computer_command(
        &self,
        command: ComputerCommand,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let application_cancellation = execution
            .as_ref()
            .map_or_else(CommandCancellation::new, |(_, token)| token.clone());
        let policy = self.config().computer;
        let capture = matches!(
            command,
            ComputerCommand::HostCapture
                | ComputerCommand::HostSnapshot
                | ComputerCommand::HostSnapshotRegion(_)
        ) || matches!(&command, ComputerCommand::Execute { action, .. } if action.is_capture());
        if matches!(
            command,
            ComputerCommand::Execute { .. }
                | ComputerCommand::HostCapture
                | ComputerCommand::HostSnapshot
                | ComputerCommand::HostSnapshotRegion(_)
        ) && (!policy.enabled
            || !(if capture {
                policy.capture_enabled
            } else {
                policy.input_enabled
            }))
        {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: if capture {
                    "Computer capture is disabled in Bootty settings"
                } else {
                    "Computer input is disabled in Bootty settings"
                }
                .to_owned(),
            });
        }
        let native_window = self.native_computer_window;
        let capture_directory =
            crate::gpui_workspace::browser_profile_directory().with_file_name("computer-captures");
        self.dispatch_committed_command(execution, move || {
            let run = || -> Result<CommandOutcome, ComputerError> {
                let executable = std::env::current_exe()?;
                let directory = executable.parent().ok_or_else(|| ComputerError::Helper("application directory unavailable".into()))?;
                let helper = directory.join("bootty-computer");
                if cfg!(target_os = "macos") && !helper.is_file() {
                    return Ok(CommandOutcome::Unavailable {
                        message: "Computer use requires the signed helper in a packaged Bootty app".to_owned(),
                    });
                }
                let computer = Computer::new(helper);
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                match command {
                    ComputerCommand::Status => Ok(super::serialized_command_outcome(serde_json::json!({
                        "enabled": policy.enabled,
                        "capture_enabled": policy.capture_enabled,
                        "input_enabled": policy.input_enabled,
                        "permissions": runtime.block_on(computer.status())?,
                    }))),
                    ComputerCommand::Targets => {
                        let targets = runtime.block_on(computer.targets())?;
                        let total = targets.len();
                        let mut accepted = Vec::new();
                        let mut bytes = 0_usize;
                        // The control reply has a 128 KiB bound; every returned token remains complete.
                        for target in targets {
                            target.validate()?;
                            bytes = bytes.saturating_add(serde_json::to_vec(&target)?.len());
                            if bytes > 96 * 1024 { break; }
                            accepted.push(target);
                        }
                        Ok(super::serialized_command_outcome(serde_json::json!({
                            "omitted_targets": total.saturating_sub(accepted.len()), "targets": accepted,
                        })))
                    }
                    ComputerCommand::HostCapture => {
                        let window = native_window.ok_or(ComputerError::TargetUnavailable)?;
                        let result = runtime.block_on(host_capture::capture_host_candidate(
                            &computer, window, &ComputerAccess::from_user_setting(policy.enabled), None,
                        ))?;
                        let image = ComputerResultSnapshot::try_from(result)?;
                        Ok(super::serialized_command_outcome(serde_json::to_value(image)?))
                    }
                    host @ (ComputerCommand::HostSnapshot | ComputerCommand::HostSnapshotRegion(_)) => {
                        let region = match &host {
                            ComputerCommand::HostSnapshotRegion(region) => Some(region),
                            _ => None,
                        };
                        let window = native_window.ok_or(ComputerError::TargetUnavailable)?;
                        let (result, path) = runtime.block_on(host_capture::capture_host_window(
                            &computer, window, &capture_directory,
                            &ComputerAccess::from_user_setting(policy.enabled), region,
                        ))?;
                        captured_result(result, Some(path))
                    }
                    ComputerCommand::Application {access,action}=>execute_application(&computer,&runtime,&access,&action,application_cancellation),
                    ComputerCommand::Execute { target, action, destination } => {
                        let result = runtime.block_on(computer.execute(
                            &ComputerAccess::from_user_setting(policy.enabled), &target, &action,
                        ))?;
                        captured_result(result, destination)
                    }
                }
            };
            run().unwrap_or_else(computer_error_outcome)
        })
    }
}

fn captured_result(
    result: ComputerResult,
    destination: Option<std::path::PathBuf>,
) -> Result<CommandOutcome, ComputerError> {
    match result {
        ComputerResult::Posted => Ok(super::serialized_command_outcome(
            serde_json::json!({"result": "posted"}),
        )),
        ComputerResult::Snapshot {
            png_base64,
            region,
            requested_region,
            pixel_width,
            pixel_height,
            target,
        } => {
            let path = destination.ok_or_else(|| {
                ComputerError::InvalidAction("capture destination is required".into())
            })?;
            let bytes = STANDARD.decode(png_base64).map_err(|_| {
                ComputerError::Helper("helper returned invalid PNG encoding".into())
            })?;
            if bytes.len() > 8 * 1024 * 1024
                || !bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10])
            {
                return Err(ComputerError::Helper(
                    "helper returned an invalid or oversized PNG".into(),
                ));
            }
            let warning = write_snapshot(&path, &bytes)?;
            let mut value = serde_json::json!({"result": "snapshot", "path": path,
                "pixel_width": pixel_width, "pixel_height": pixel_height, "target": target});
            if let Some(region) = region
                && let serde_json::Value::Object(fields) = &mut value
            {
                fields.insert("region".into(), serde_json::to_value(region)?);
            }
            if let Some(requested_region) = requested_region
                && let serde_json::Value::Object(fields) = &mut value
            {
                fields.insert(
                    "requested_region".into(),
                    serde_json::to_value(requested_region)?,
                );
            }
            Ok(CommandOutcome::Success {
                value,
                warnings: warning.into_iter().collect(),
            })
        }
    }
}

fn write_snapshot(path: &Path, bytes: &[u8]) -> Result<Option<CommandWarning>, ComputerError> {
    if !path.is_absolute() {
        return Err(ComputerError::InvalidAction(
            "capture destination must be an absolute path".into(),
        ));
    }
    // Reject existing aliases before resolution; publication also refuses a concurrent winner.
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "capture destination already exists",
            )
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let target = WriteTarget::resolve(path)
        .map_err(ResolveTargetError::into_io)?
        .lock()?;
    match target
        .create(bytes, NewFileMode::Private)
        .map_err(CommitError::into_io)?
    {
        CommitOutcome::Confirmed => Ok(None),
        CommitOutcome::CommittedWithDurabilityWarning(error) => Ok(Some(CommandWarning {
            code: "durability_warning".into(),
            message: error.to_string(),
        })),
    }
}

pub fn computer_error_outcome(error: ComputerError) -> CommandOutcome {
    match error {
        ComputerError::Unsupported => CommandOutcome::Unsupported {
            message:
                "Computer capture requires macOS 14 or later; input requires macOS 13 or later"
                    .into(),
        },
        error @ (ComputerError::Disabled
        | ComputerError::PermissionDenied(_)
        | ComputerError::SecureInput) => CommandOutcome::Denied {
            message: error.to_string(),
        },
        error @ (ComputerError::StaleTarget | ComputerError::TargetNotFocused) => {
            CommandOutcome::StaleTarget {
                message: error.to_string(),
            }
        }
        ComputerError::TargetUnavailable => CommandOutcome::Unavailable {
            message: "Selected computer window is unavailable".into(),
        },
        error => CommandOutcome::Failed {
            code: "computer_failed".into(),
            message: error.to_string(),
        },
    }
}

fn execute_application(
    computer: &Computer,
    runtime: &tokio::runtime::Runtime,
    access: &bootty_agents::NativeApplicationAccess,
    action: &bootty_computer::ComputerAction,
    cancellation: CommandCancellation,
) -> Result<CommandOutcome, ComputerError> {
    // Explicit prompt mentions own this grant; the helper checks OS grants and exact identity.
    let guard = access.begin(cancellation).map_err(ComputerError::Helper)?;
    let result = runtime.block_on(computer.execute(
        &ComputerAccess::from_user_setting(true),
        access.target(),
        action,
    ))?;
    if !access.current() {
        return Ok(CommandOutcome::StaleTarget {
            message: "Application access changed".into(),
        });
    }
    let result = captured_result(result, None);
    drop(guard);
    result
}
