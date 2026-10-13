use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_computer::{
    Computer, ComputerAccess, ComputerError, ComputerResult, ComputerResultSnapshot,
};
use bootty_control::{CommandCancellation, CommandOutcome, CommandWarning};
use bootty_write::{CommitError, CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};

use super::CommandDispatch;
use crate::{
    AppState,
    commands::{ApplicationCommand, ComputerCommand},
};

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
        policy: &bootty_config::config::ComputerConfig,
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
        let policy = self.config().computer.clone();
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
                let computer = match packaged_computer()? {
                    Ok(computer) => computer,
                    Err(unavailable) => return Ok(unavailable),
                };
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

/// The signed helper ships beside the packaged app's executable.
fn packaged_computer() -> Result<Result<Computer, CommandOutcome>, ComputerError> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| ComputerError::Helper("application directory unavailable".into()))?;
    let helper = directory.join("bootty-computer");
    if cfg!(target_os = "macos") && !helper.is_file() {
        return Ok(Err(CommandOutcome::Unavailable {
            message: "Computer use requires the signed helper in a packaged Bootty app".to_owned(),
        }));
    }
    Ok(Ok(Computer::new(helper)))
}

impl AppState {
    /// Agent app use: find the app's window, check or request the conversation grant, then act.
    /// A prompt mention, an approval card or the always-allowed list is the user's consent, so
    /// the `[computer]` switches that gate `computer.*` commands do not apply here.
    pub(super) fn dispatch_application_command(
        &self,
        command: ApplicationCommand,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = execution.clone().unwrap_or_else(|| {
            (
                Instant::now()
                    .checked_add(APPROVAL_WAIT.saturating_add(ACTION_TIME))
                    .unwrap_or_else(Instant::now),
                CommandCancellation::new(),
            )
        });
        let always_allowed = self.config().computer.always_allowed_apps.clone();
        self.dispatch_committed_command(execution, move || {
            run_application(&command, &always_allowed, deadline, cancellation)
                .unwrap_or_else(computer_error_outcome)
        })
    }
}

// The agent's tool call stays open while the user answers. Waiting ends early enough to finish
// the action inside the bridge's deadline; an unanswered card stays up for a retry.
const APPROVAL_WAIT: Duration = Duration::from_secs(40);
const ACTION_TIME: Duration = Duration::from_secs(8);

fn run_application(
    command: &ApplicationCommand,
    always_allowed: &[String],
    deadline: Instant,
    cancellation: CommandCancellation,
) -> Result<CommandOutcome, ComputerError> {
    let denied = |message: String| Ok(CommandOutcome::Denied { message });
    let computer = match packaged_computer()? {
        Ok(computer) => computer,
        Err(unavailable) => return Ok(unavailable),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let targets = runtime.block_on(computer.targets())?;
    let Some(window) = frontmost_window(&targets, &command.application) else {
        return Ok(CommandOutcome::Unavailable {
            message: format!(
                "No open window belongs to {}. Launch it or bring a window on screen, then try again.",
                command.application
            ),
        });
    };
    let application = bootty_agents::NativeApplication::from(window);
    if !agent_may_use(&application.bundle_id) {
        return denied(format!(
            "Bootty does not let agents use {}",
            application.name
        ));
    }
    let preapproved = always_allowed.contains(&application.bundle_id);
    let access = match grant(command, &application, preapproved, deadline) {
        Ok(access) => access,
        Err(message) => return denied(message),
    };
    let guard = access.begin(cancellation).map_err(ComputerError::Helper)?;
    // Captures always see the app's current front window; input targets the captured one so
    // coordinates keep meaning what the agent saw.
    let target = if command.action.is_capture() {
        window.clone()
    } else if let Some(observed) = access.observed() {
        observed
    } else {
        return denied(format!(
            "Take a computer_snapshot of {} before sending input",
            application.name
        ));
    };
    target.validate_action(&command.action)?;
    let result = runtime.block_on(computer.execute(
        &ComputerAccess::from_user_setting(true),
        &target,
        &command.action,
    ))?;
    if !access.current() {
        return Ok(CommandOutcome::StaleTarget {
            message: format!("Access to {} changed during the command", application.name),
        });
    }
    let outcome = match result {
        ComputerResult::Posted => {
            super::serialized_command_outcome(serde_json::json!({"result": "posted"}))
        }
        snapshot @ ComputerResult::Snapshot { .. } => {
            let snapshot = ComputerResultSnapshot::try_from(snapshot)?;
            access.observe(snapshot.geometry().0.clone());
            super::serialized_command_outcome(serde_json::to_value(snapshot)?)
        }
    };
    drop(guard);
    Ok(outcome)
}

/// The conversation's grant for the app, asking the user when it has none. Errors are the
/// denial the agent sees.
fn grant(
    command: &ApplicationCommand,
    application: &bootty_agents::NativeApplication,
    preapproved: bool,
    deadline: Instant,
) -> Result<bootty_agents::NativeApplicationAccess, String> {
    let lookup = |preapproved| {
        command.service.application_access(
            &command.conversation,
            application,
            preapproved,
            command.caller,
        )
    };
    if let Some(access) = lookup(preapproved)? {
        return Ok(access);
    }
    let answer = command.service.request_application_access(
        &command.conversation,
        application,
        command.caller,
    )?;
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(ACTION_TIME)
        .min(APPROVAL_WAIT);
    match answer.recv_timeout(wait) {
        Ok(true) => lookup(false)?.ok_or_else(|| format!("Access to {} changed", application.name)),
        Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("The user did not allow {}", application.name))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "Waiting for the user to allow {}. Try again after they answer.",
            application.name
        )),
    }
}

/// Window lists run front to back, so the first match is the app's front window.
fn frontmost_window<'a>(
    targets: &'a [bootty_computer::ComputerTarget],
    application: &str,
) -> Option<&'a bootty_computer::ComputerTarget> {
    targets.iter().find(|target| {
        target.bundle_id == application
            || target
                .application_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(application))
    })
}

/// Agents never drive Bootty, whose windows hold their own approval cards, nor apps that run
/// shell commands or change OS permissions outside the conversation's own approvals.
fn agent_may_use(bundle_id: &str) -> bool {
    // Deliberate limit: a fixed list of well-known terminals. Extend it when users report
    // another terminal, or replace it with an OS-level category check if macOS offers one.
    const BLOCKED: [&str; 9] = [
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        "com.mitchellh.ghostty",
        "dev.warp.Warp-Stable",
        "net.kovidgoyal.kitty",
        "org.alacritty",
        "com.github.wez.wezterm",
        "com.apple.systempreferences",
        "com.apple.SecurityAgent",
    ];
    !bootty_config::is_bootty_bundle_identifier(bundle_id) && !BLOCKED.contains(&bundle_id)
}
