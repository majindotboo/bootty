//! Native desktop commands use the same mailbox as terminal and agent commands.
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    commands::runtime::CommandDispatch,
    state::{AppEffect, AppState},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerError, ComputerResult, Permission,
};
use bootty_control::{
    ArgumentSchema, Caller, CommandCancellation, CommandDescriptor, CommandOutcome, CompactSchema,
    MutationClass, ValueType,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputerCommand {
    Setup,
    Status,
    PermissionRequest,
    Snapshot,
    Click,
    TypeText,
    Key,
    Scroll,
    Move,
    Activate,
    Enable,
}

impl ComputerCommand {
    pub(crate) const ALL: [Self; 11] = [
        Self::Setup,
        Self::Status,
        Self::PermissionRequest,
        Self::Snapshot,
        Self::Click,
        Self::TypeText,
        Self::Key,
        Self::Scroll,
        Self::Move,
        Self::Activate,
        Self::Enable,
    ];

    pub(crate) fn descriptor(self) -> CommandDescriptor {
        let (id, title, names) = match self {
            Self::Setup => ("computer.setup", "Computer use…", &[][..]),
            Self::Status => ("computer.status", "Computer permissions", &[][..]),
            Self::PermissionRequest => (
                "computer.permission.request",
                "Grant computer permission…",
                &["permission"][..],
            ),
            Self::Snapshot => (
                "computer.snapshot",
                "Capture desktop",
                &["destination", "display_id"][..],
            ),
            Self::Click => ("computer.click", "Click desktop", &["x", "y", "button"][..]),
            Self::TypeText => ("computer.type", "Type desktop text", &["text"][..]),
            Self::Key => (
                "computer.key",
                "Press desktop key",
                &["key", "modifiers"][..],
            ),
            Self::Scroll => (
                "computer.scroll",
                "Scroll desktop",
                &["x", "y", "delta_x", "delta_y"][..],
            ),
            Self::Move => ("computer.move", "Move desktop pointer", &["x", "y"][..]),
            Self::Activate => (
                "computer.activate",
                "Activate application",
                &["bundle_id"][..],
            ),
            Self::Enable => ("computer.enable", "Enable computer use", &["enabled"][..]),
        };
        let arguments = names
            .iter()
            .map(|name| ArgumentSchema {
                name: (*name).into(),
                value_type: match *name {
                    "x" | "y" => ValueType::Number,
                    "delta_x" | "delta_y" | "display_id" => ValueType::Integer,
                    _ => ValueType::String,
                },
                required: !matches!(*name, "destination" | "display_id" | "button" | "modifiers"),
                choices: match *name {
                    "permission" => vec!["accessibility".into(), "screen_recording".into()],
                    "button" => vec!["left".into(), "right".into(), "middle".into()],
                    "enabled" => vec!["true".into(), "false".into()],
                    _ => Vec::new(),
                },
                minimum: None,
                maximum: None,
            })
            .collect();
        CommandDescriptor {
            id: id.into(), title: title.into(),
            description: "Controls the local desktop after explicit user enabling; coordinates use desktop points. Permission setup requires a user action.".into(),
            mutation: if self == Self::Status { MutationClass::Read } else { MutationClass::Write },
            arguments: CompactSchema { arguments }, target: None, palette: self == Self::Setup,
        }
    }

    fn action(self, args: &[String]) -> Result<ComputerAction, ComputerError> {
        let argument = |index: usize| {
            args.get(index)
                .map(String::as_str)
                .ok_or_else(|| ComputerError::InvalidAction(format!("missing argument {index}")))
        };
        let coordinate = |index: usize| -> Result<f64, ComputerError> {
            argument(index)?
                .parse()
                .map_err(|_| ComputerError::InvalidAction("coordinates must be numbers".into()))
        };
        let choice = |text: &str| {
            serde_json::from_value(serde_json::Value::String(text.into()))
                .map_err(ComputerError::from)
        };
        match self {
            Self::Snapshot => Ok(ComputerAction::Snapshot {
                display_id: args
                    .get(1)
                    .filter(|value| !value.is_empty())
                    .map(|value| {
                        value.parse().map_err(|_| {
                            ComputerError::InvalidAction("display_id must be an integer".into())
                        })
                    })
                    .transpose()?,
            }),
            Self::Click => Ok(ComputerAction::Click {
                x: coordinate(0)?,
                y: coordinate(1)?,
                button: choice(args.get(2).map_or("left", String::as_str))?,
            }),
            Self::Move => Ok(ComputerAction::Move {
                x: coordinate(0)?,
                y: coordinate(1)?,
            }),
            Self::TypeText => Ok(ComputerAction::TypeText {
                text: argument(0)?.into(),
            }),
            Self::Key => Ok(ComputerAction::Key {
                key: serde_json::from_value(serde_json::Value::String(argument(0)?.into()))?,
                modifiers: args.get(1).map_or(Ok(Vec::new()), |value| {
                    value
                        .split(',')
                        .filter(|value| !value.is_empty())
                        .map(|value| {
                            serde_json::from_value(serde_json::Value::String(value.into()))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })?,
            }),
            Self::Scroll => Ok(ComputerAction::Scroll {
                x: coordinate(0)?,
                y: coordinate(1)?,
                delta_x: argument(2)?.parse().map_err(|_| {
                    ComputerError::InvalidAction("delta_x must be an integer".into())
                })?,
                delta_y: argument(3)?.parse().map_err(|_| {
                    ComputerError::InvalidAction("delta_y must be an integer".into())
                })?,
            }),
            Self::Activate => Ok(ComputerAction::Activate {
                bundle_id: argument(0)?.into(),
            }),
            Self::Setup | Self::Status | Self::Enable | Self::PermissionRequest => Err(
                ComputerError::InvalidAction("command has no input action".into()),
            ),
        }
    }
}

impl AppState {
    pub(super) fn dispatch_computer_command(
        &mut self,
        command: ComputerCommand,
        arguments: &[String],
        caller: Caller,
        enabled: bool,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if command == ComputerCommand::Setup {
            if let Err(error) = bootty_mux::executor::begin_synchronous_command(execution) {
                return CommandDispatch::Complete(super::command_outcome_for_mux_error(error));
            }
            effects.push(AppEffect::OpenComputerSetup);
            return CommandDispatch::Complete(CommandOutcome::success());
        }
        if matches!(
            command,
            ComputerCommand::Enable | ComputerCommand::PermissionRequest
        ) && !matches!(
            caller,
            Caller::CommandPalette | Caller::Keybinding | Caller::BuiltinKeybinding
        ) {
            return CommandDispatch::Complete(CommandOutcome::Denied {
                message: "Enable computer use and grant permissions from Bootty's setup screen."
                    .into(),
            });
        }
        if command == ComputerCommand::Enable {
            if let Err(error) = bootty_mux::executor::begin_synchronous_command(execution) {
                return CommandDispatch::Complete(super::command_outcome_for_mux_error(error));
            }
            let Some(next) = arguments
                .first()
                .and_then(|value| value.parse::<bool>().ok())
            else {
                return CommandDispatch::Complete(failure("enabled must be true or false"));
            };
            let mut document = self.config_document();
            if let Err(error) = document.set_bool(&["computer-use"], next) {
                return CommandDispatch::Complete(failure(error.to_string()));
            }
            return CommandDispatch::Complete(match self.commit_settings_document(document) {
                Ok((_, warning, accepted_effects)) => {
                    effects.extend(accepted_effects);
                    warning.map_or_else(
                        || CommandOutcome::Success {
                            value: serde_json::json!({"enabled": next}),
                            warnings: Vec::new(),
                        },
                        |warning| {
                            CommandOutcome::success_with_warning("configuration_warning", warning)
                        },
                    )
                }
                Err(error) => failure(error.to_string()),
            });
        }
        let arguments = arguments.to_vec();
        self.dispatch_committed_command(execution, move || {
            let run = || -> Result<serde_json::Value, ComputerError> {
                let executable = std::env::current_exe()?;
                let helper = executable.parent().ok_or_else(|| ComputerError::Helper("application directory unavailable".into()))?.join("bootty-computer");
                let computer = Computer::new(helper);
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                match command {
                    ComputerCommand::Status => Ok(serde_json::json!({"enabled": enabled, "permissions": runtime.block_on(computer.status())?})),
                    ComputerCommand::PermissionRequest => {
                        let permission: Permission = serde_json::from_value(serde_json::Value::String(arguments.first().ok_or_else(|| ComputerError::InvalidAction("permission is required".into()))?.clone()))?;
                        Ok(serde_json::json!({"enabled": enabled, "permissions": runtime.block_on(computer.request_permission(permission))?}))
                    }
                    _ => {
                        let result = runtime.block_on(computer.execute(&ComputerAccess::from_user_setting(enabled), &command.action(&arguments)?))?;
                        capture_value(result, arguments.first().filter(|path| !path.is_empty()).map(String::as_str))
                    }
                }
            };
            match run() {
                Ok(value) => CommandOutcome::Success { value, warnings: Vec::new() },
                Err(ComputerError::Unsupported) => CommandOutcome::Unsupported { message: "Computer use requires macOS; screenshots require macOS 14 or later.".into() },
                Err(error @ (ComputerError::Disabled | ComputerError::PermissionDenied(_) | ComputerError::SecureInput)) => CommandOutcome::Denied { message: error.to_string() },
                Err(error) => failure(error.to_string()),
            }
        })
    }
}

fn capture_value(
    result: ComputerResult,
    destination: Option<&str>,
) -> Result<serde_json::Value, ComputerError> {
    let ComputerResult::Snapshot {
        png_base64,
        pixel_width,
        pixel_height,
        display_id,
        bounds,
        application,
        bundle_id,
        elements,
    } = result
    else {
        return Ok(serde_json::json!({"result": "posted"}));
    };
    let bytes = STANDARD
        .decode(png_base64)
        .map_err(|error| ComputerError::Helper(error.to_string()))?;
    let path = write_snapshot(&bytes, destination)?;
    let total = elements.len();
    let mut accepted = Vec::new();
    let mut budget = 0usize;
    // Control replies fit below its 128 KiB transport bound; the PNG remains a private file.
    for element in elements {
        let encoded = serde_json::to_vec(&element)?;
        budget = budget.saturating_add(encoded.len());
        if budget > 90 * 1024 {
            break;
        }
        accepted.push(element);
    }
    Ok(
        serde_json::json!({"result": "snapshot", "path": path, "pixel_width":pixel_width, "pixel_height":pixel_height, "display_id":display_id, "bounds":bounds, "application":application, "bundle_id":bundle_id, "elements":accepted, "omitted_elements":total.saturating_sub(accepted.len())}),
    )
}

fn write_snapshot(bytes: &[u8], destination: Option<&str>) -> Result<PathBuf, ComputerError> {
    if let Some(path) = destination {
        let path = Path::new(path);
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(path)
            .map_err(|error| ComputerError::Io(error.error))?;
        Ok(path.to_path_buf())
    } else {
        let mut file = tempfile::Builder::new()
            .prefix("bootty-computer-")
            .suffix(".png")
            .tempfile()?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        let (_, path) = file
            .keep()
            .map_err(|error| ComputerError::Io(error.error))?;
        Ok(path)
    }
}

fn failure(message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "computer_failed".into(),
        message: message.into(),
    }
}
