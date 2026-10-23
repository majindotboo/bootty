use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerError, ComputerResult,
    ComputerResultSnapshot, Permission,
};
use bootty_control::{CommandCancellation, CommandOutcome, CommandWarning};
use bootty_mux::controller::RepaintHandle;
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
    let computer = Computer::new();
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
        ) || matches!(&command, ComputerCommand::Execute { action, .. } if action.observes());
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
        let repaint = self.repaint.clone();
        self.dispatch_committed_command(execution, move || {
            let run = || -> Result<CommandOutcome, ComputerError> {
                let computer = Computer::new();
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
                    ComputerCommand::Icons(bundle_ids) => {
                        let icons = runtime.block_on(computer.icons(&bundle_ids))?;
                        Ok(super::serialized_command_outcome(serde_json::json!({
                            "icons": icons
                                .into_iter()
                                .map(|(bundle_id, png)| (bundle_id, STANDARD.encode(png)))
                                .collect::<std::collections::BTreeMap<_, _>>(),
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
                        let result = crate::agent_cursor::pointing(&action, repaint.as_ref(), || {
                            runtime.block_on(computer.execute(
                                &ComputerAccess::from_user_setting(policy.enabled), &target, &action,
                            ))
                        })?;
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
        ComputerResult::Pressed { method } => Ok(super::serialized_command_outcome(
            serde_json::json!({"result": "pressed", "method": method}),
        )),
        ComputerResult::Elements {
            elements,
            mut omitted,
            target,
        } => {
            // The control reply has a 128 KiB bound; every returned element remains complete.
            let mut accepted = Vec::new();
            let mut bytes = 0_usize;
            for element in elements {
                bytes = bytes.saturating_add(serde_json::to_vec(&element)?.len());
                if bytes > 96 * 1024 {
                    omitted = true;
                    break;
                }
                accepted.push(element);
            }
            Ok(super::serialized_command_outcome(serde_json::json!({
                "result": "elements", "elements": accepted, "omitted": omitted, "target": target,
            })))
        }
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
                ComputerError::Platform("computer use returned invalid PNG encoding".into())
            })?;
            if bytes.len() > 8 * 1024 * 1024
                || !bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10])
            {
                return Err(ComputerError::Platform(
                    "computer use returned an invalid or oversized PNG".into(),
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
            message: "The desktop does not support this operation for the selected window".into(),
        },
        error @ (ComputerError::Disabled
        | ComputerError::PermissionDenied(_)
        | ComputerError::SecureInput) => CommandOutcome::Denied {
            message: error.to_string(),
        },
        error @ (ComputerError::StaleTarget
        | ComputerError::StaleElement
        | ComputerError::TargetNotFocused) => CommandOutcome::StaleTarget {
            message: error.to_string(),
        },
        ComputerError::TargetUnavailable => CommandOutcome::Unavailable {
            message: "Selected computer window is unavailable".into(),
        },
        error => CommandOutcome::Failed {
            code: "computer_failed".into(),
            message: error.to_string(),
        },
    }
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
        let repaint = self.repaint.clone();
        self.dispatch_committed_command(execution, move || {
            run_application(&command, &always_allowed, deadline, cancellation, &repaint)
                .unwrap_or_else(computer_error_outcome)
        })
    }
}

// The agent's tool call stays open while the user answers. Waiting ends early enough to finish
// the action inside the shared tool call timeout; an unanswered card stays up for a retry.
const ACTION_TIME: Duration = Duration::from_secs(8);
const APPROVAL_WAIT: Duration = bootty_host::private_stdio::TOOL_CALL_TIMEOUT
    .saturating_sub(ACTION_TIME)
    .saturating_sub(Duration::from_secs(5));

fn run_application(
    command: &ApplicationCommand,
    always_allowed: &[String],
    deadline: Instant,
    cancellation: CommandCancellation,
    repaint: &RepaintHandle,
) -> Result<CommandOutcome, ComputerError> {
    let denied = |message: String| Ok(CommandOutcome::Denied { message });
    let computer = Computer::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut targets = runtime.block_on(computer.targets())?;
    if matches!(&command.action, bootty_agents::NativeApplicationAction::Computer(action) if action.is_capture())
    {
        targets.retain(|target| !target.is_semantic_only());
    } else {
        targets.sort_by_key(|target| !target.is_semantic_only());
    }
    let Some(window) = frontmost_window(&targets, &command.application) else {
        return Ok(CommandOutcome::Unavailable {
            message: format!(
                "No open window belongs to {}. Launch it or bring a window on screen, then try again.",
                command.application
            ),
        });
    };
    let application = bootty_agents::NativeApplication::from(window);
    if !crate::commands::agent_may_use(window) {
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
    let guard = access
        .begin(cancellation)
        .map_err(ComputerError::Platform)?;
    let action = match agent_action(&command.action, &access, &application) {
        Ok(action) => action,
        Err(outcome) => return Ok(outcome),
    };
    // Observations see the app's current front window; input targets the observed one so
    // coordinates and element numbers keep meaning what the agent saw.
    let target = if action.observes() {
        window.clone()
    } else if let Some(observed) = access.observed() {
        observed
    } else {
        return denied(format!(
            "Use computer_elements or computer_snapshot on {} before sending input",
            application.name
        ));
    };
    target.validate_action(&action)?;
    let access_setting = ComputerAccess::from_user_setting(true);
    let result = match crate::agent_cursor::pointing(&action, repaint.as_ref(), || {
        runtime.block_on(computer.execute(&access_setting, &target, &action))
    }) {
        Err(ComputerError::StaleTarget) => {
            return Ok(CommandOutcome::StaleTarget {
                message: format!(
                    "{}'s window moved, resized or closed. Refresh with computer_elements or computer_snapshot.",
                    application.name
                ),
            });
        }
        Err(ComputerError::StaleElement) => {
            return Ok(CommandOutcome::StaleTarget {
                message: format!(
                    "That element of {} moved, changed or closed. Refresh with computer_elements or computer_snapshot.",
                    application.name
                ),
            });
        }
        result => result?,
    };
    if !access.current() {
        return Ok(CommandOutcome::StaleTarget {
            message: format!("Access to {} changed during the command", application.name),
        });
    }
    let outcome = match result {
        result @ ComputerResult::Elements { .. } => {
            if let ComputerResult::Elements {
                target, elements, ..
            } = &result
            {
                access.observe((**target).clone(), elements.clone());
            }
            super::serialized_command_outcome(serde_json::to_value(result)?)
        }
        snapshot @ ComputerResult::Snapshot { .. } => {
            snapshot_with_elements(snapshot, &access, |target| {
                runtime.block_on(computer.execute(
                    &access_setting,
                    target,
                    &ComputerAction::Elements,
                ))
            })?
        }
        result => captured_result(result, None)?,
    };
    drop(guard);
    Ok(outcome)
}

/// The platform action for an agent's request. A press names an element by its number in the
/// app's latest observation; agents never supply their own element identities.
fn agent_action(
    action: &bootty_agents::NativeApplicationAction,
    access: &bootty_agents::NativeApplicationAccess,
    application: &bootty_agents::NativeApplication,
) -> Result<ComputerAction, CommandOutcome> {
    match action {
        bootty_agents::NativeApplicationAction::SetValue(set) => access
            .element(set.element)
            .map(|element| ComputerAction::SetValue {
                element: Box::new(element),
                value: set.value.clone(),
            })
            .ok_or_else(|| CommandOutcome::StaleTarget {
                message: format!("{}'s latest observation has no element {}. Refresh with computer_elements.", application.name, set.element),
            }),
        bootty_agents::NativeApplicationAction::Press(press) => access
            .element(press.element)
            .map(|element| ComputerAction::Press {
                element: Box::new(element),
                allow_click: press.allow_click,
            })
            .ok_or_else(|| CommandOutcome::StaleTarget {
                message: format!(
                    "{}'s latest observation has no element {}. Refresh with computer_elements or computer_snapshot.",
                    application.name, press.element
                ),
            }),
        bootty_agents::NativeApplicationAction::Computer(
            ComputerAction::Press { .. } | ComputerAction::SetValue { .. },
        ) => Err(CommandOutcome::Denied {
            message: "computer_elements and computer_snapshot list elements; press one by its number".to_owned(),
        }),
        bootty_agents::NativeApplicationAction::Computer(action) => Ok(action.clone()),
    }
}

/// The agent's snapshot with the window's elements, which later presses refer to by number. A
/// snapshot without elements still works with coordinates.
fn snapshot_with_elements(
    snapshot: ComputerResult,
    access: &bootty_agents::NativeApplicationAccess,
    list: impl FnOnce(&bootty_computer::ComputerTarget) -> Result<ComputerResult, ComputerError>,
) -> Result<CommandOutcome, ComputerError> {
    let image = ComputerResultSnapshot::try_from(snapshot)?;
    let target = image.geometry().0.clone();
    let (elements, omitted, unavailable) = match list(&target) {
        Ok(ComputerResult::Elements {
            elements, omitted, ..
        }) => (elements, omitted, None),
        Ok(_) => (Vec::new(), false, Some("the listing failed".to_owned())),
        Err(error) => (Vec::new(), false, Some(elements_unavailable(&error))),
    };
    access.observe(target, elements.clone());
    Ok(super::serialized_command_outcome(serde_json::to_value(
        bootty_agents::NativeApplicationSnapshot {
            image,
            elements,
            omitted,
            unavailable,
        },
    )?))
}

/// Why a snapshot came without elements, in words an agent can act on.
fn elements_unavailable(error: &ComputerError) -> String {
    match error {
        ComputerError::PermissionDenied(Permission::Accessibility) => {
            "Bootty does not have the Accessibility permission; use coordinates.".to_owned()
        }
        ComputerError::Unsupported => {
            "this platform does not list elements yet; use coordinates.".to_owned()
        }
        error => format!("{error}; use coordinates."),
    }
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

/// Choose the first matching window in the adapter's observation order. Semantic-only providers
/// may not expose stacking order; the exact chosen window is retained for subsequent input.
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
