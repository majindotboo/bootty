use std::{collections::BTreeMap, path::PathBuf};

use bootty_computer::{ComputerAction, ComputerTarget, HostCaptureRegion, MouseButton};
use bootty_control::{
    CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema, MutationClass, ValueType,
};

/// Local desktop work submitted through the shared command path.
#[derive(Clone, Debug, PartialEq)]
pub enum ComputerCommand {
    Status,
    Targets,
    HostCapture,
    HostSnapshot,
    HostSnapshotRegion(HostCaptureRegion),
    Application {
        access: bootty_agents::NativeApplicationAccess,
        action: ComputerAction,
    },
    Execute {
        target: ComputerTarget,
        action: ComputerAction,
        destination: Option<PathBuf>,
    },
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep the finite command schemas in one declarative table"
)]
pub(super) fn register_commands(commands: &mut BTreeMap<String, super::RegisteredCommand>) {
    for (id, title, description, names, mutation) in [
        (
            "computer.status",
            "Computer Permission Status",
            "Read feature policy and existing OS permissions without requesting access.",
            &[][..],
            MutationClass::Read,
        ),
        (
            "computer.targets",
            "List Computer Windows",
            "List exact local window tokens without enabling computer use or changing focus.",
            &[][..],
            MutationClass::Read,
        ),
        (
            "computer.capture",
            "Capture Computer Image",
            "Return a bounded PNG of the exact owning application window without writing a file.",
            &[][..],
            MutationClass::Read,
        ),
        (
            "computer.snapshot",
            "Capture Computer Window",
            "Capture the selected window to a new absolute PNG path; existing files are preserved.",
            &["target", "destination", "region"][..],
            MutationClass::Write,
        ),
        (
            "computer.click",
            "Click Computer Window",
            "Click an uncovered point inside the exact selected focused window.",
            &["target", "x", "y", "button"][..],
            MutationClass::Write,
        ),
        (
            "computer.move",
            "Move Computer Pointer",
            "Move the pointer at an uncovered point inside the exact selected focused window.",
            &["target", "x", "y"][..],
            MutationClass::Write,
        ),
        (
            "computer.scroll",
            "Scroll Computer Window",
            "Scroll at an uncovered point inside the exact selected focused window.",
            &["target", "x", "y", "delta-x", "delta-y"][..],
            MutationClass::Write,
        ),
        (
            "computer.type",
            "Type Computer Text",
            "Type bounded text into the exact selected focused window.",
            &["target", "text"][..],
            MutationClass::Write,
        ),
        (
            "computer.focus",
            "Focus Computer Window",
            "Raise the exact selected application window.",
            &["target"][..],
            MutationClass::Write,
        ),
        (
            "computer.key",
            "Press Computer Key",
            "Post a key with optional comma-separated modifiers to the exact selected focused window.",
            &["target", "key", "modifiers"][..],
            MutationClass::Write,
        ),
    ] {
        let arguments = names
            .iter()
            .map(|name| {
                let mut argument = super::argument(
                    name,
                    match *name {
                        "x" | "y" => ValueType::Number,
                        "delta-x" | "delta-y" => ValueType::Integer,
                        _ => ValueType::String,
                    },
                );
                argument.required =
                    !matches!(*name, "button" | "modifiers") && id != "computer.snapshot";
                if *name == "button" {
                    argument.choices = ["left", "right", "middle"].map(str::to_owned).to_vec();
                }
                if matches!(*name, "delta-x" | "delta-y") {
                    argument.minimum = Some(-10_000);
                    argument.maximum = Some(10_000);
                }
                argument
            })
            .collect();
        let descriptor = CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
            mutation,
            arguments: CompactSchema { arguments },
            target: None,
            palette: false,
        };
        commands.insert(
            descriptor.id.clone(),
            super::RegisteredCommand {
                descriptor,
                executor: super::CommandExecutorResolver::Computer,
            },
        );
    }
}

pub(super) fn resolve(invocation: &CommandInvocation) -> Result<ComputerCommand, CommandOutcome> {
    let invalid = || CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: format!("Invalid arguments for {}", invocation.command),
    };
    match invocation.command.as_str() {
        "computer.capture" => {
            return if invocation.arguments.is_empty()
                && invocation.target.as_ref().is_some_and(|target| {
                    target.kind == bootty_control::ResourceKind::ApplicationWindow
                }) {
                Ok(ComputerCommand::HostCapture)
            } else {
                Err(invalid())
            };
        }
        "computer.snapshot" if invocation.arguments.len() <= 1 => {
            if invocation.target.as_ref().is_some_and(|target| {
                target.kind == bootty_control::ResourceKind::ApplicationWindow
            }) {
                return resolve_host_snapshot(invocation.arguments.first().map(String::as_str))
                    .map_err(|()| invalid());
            }
            return Err(invalid());
        }
        "computer.status" if invocation.arguments.is_empty() => return Ok(ComputerCommand::Status),
        "computer.targets" if invocation.arguments.is_empty() => {
            return Ok(ComputerCommand::Targets);
        }
        _ => {}
    }
    let argument = |index: usize| {
        invocation
            .arguments
            .get(index)
            .map(String::as_str)
            .ok_or_else(invalid)
    };
    let target: ComputerTarget = serde_json::from_str(argument(0)?).map_err(|_| invalid())?;
    target.validate().map_err(|_| invalid())?;
    let coordinate =
        |index| -> Result<f64, CommandOutcome> { argument(index)?.parse().map_err(|_| invalid()) };
    let mut destination = None;
    let action = match invocation.command.as_str() {
        "computer.snapshot" => {
            let path = PathBuf::from(argument(1)?);
            if !path.is_absolute() || path.file_name().is_none() {
                return Err(invalid());
            }
            destination = Some(path);
            match invocation.arguments.get(2) {
                Some(rect) => ComputerAction::SnapshotRegion {
                    rect: serde_json::from_str(rect).map_err(|_| invalid())?,
                },
                None => ComputerAction::Snapshot,
            }
        }
        "computer.focus" => ComputerAction::Focus,
        "computer.click" => ComputerAction::Click {
            x: coordinate(1)?,
            y: coordinate(2)?,
            button: invocation
                .arguments
                .get(3)
                .map_or(Ok(MouseButton::Left), |button| {
                    serde_json::from_value(serde_json::Value::String(button.clone()))
                        .map_err(|_| invalid())
                })?,
        },
        "computer.move" => ComputerAction::Move {
            x: coordinate(1)?,
            y: coordinate(2)?,
        },
        "computer.scroll" => ComputerAction::Scroll {
            x: coordinate(1)?,
            y: coordinate(2)?,
            delta_x: argument(3)?.parse().map_err(|_| invalid())?,
            delta_y: argument(4)?.parse().map_err(|_| invalid())?,
        },
        "computer.type" => ComputerAction::TypeText {
            text: argument(1)?.to_owned(),
        },
        "computer.key" => resolve_key(
            argument(1)?,
            invocation.arguments.get(2).map(String::as_str),
        )
        .map_err(|()| invalid())?,
        _ => return Err(invalid()),
    };
    target.validate_action(&action).map_err(|_| invalid())?;
    Ok(ComputerCommand::Execute {
        target,
        action,
        destination,
    })
}

fn resolve_host_snapshot(value: Option<&str>) -> Result<ComputerCommand, ()> {
    let Some(value) = value else {
        return Ok(ComputerCommand::HostSnapshot);
    };
    let region: HostCaptureRegion = serde_json::from_str(value).map_err(|_| ())?;
    region.validate().map_err(|_| ())?;
    Ok(ComputerCommand::HostSnapshotRegion(region))
}

fn resolve_key(key: &str, modifiers: Option<&str>) -> Result<ComputerAction, ()> {
    let key = serde_json::from_value(serde_json::Value::String(key.to_owned())).map_err(|_| ())?;
    let modifiers = modifiers.map_or(Ok(Vec::new()), |modifiers| {
        modifiers
            .split(',')
            .filter(|modifier| !modifier.is_empty())
            .map(|modifier| {
                serde_json::from_value(serde_json::Value::String(modifier.to_owned()))
                    .map_err(|_| ())
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(ComputerAction::Key { key, modifiers })
}
