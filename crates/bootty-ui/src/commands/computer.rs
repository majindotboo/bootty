use std::{collections::BTreeMap, path::PathBuf};

use bootty_computer::{
    ComputerAction, ComputerElement, ComputerTarget, ElementRoot, HostCaptureRegion, MouseButton,
};
use bootty_control::{
    CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema, MutationClass, ValueType,
};

/// Local desktop work submitted through the shared command path.
#[derive(Clone, Debug, PartialEq)]
pub enum ComputerCommand {
    Status,
    Targets,
    Icons(Vec<String>),
    HostCapture,
    HostSnapshot,
    HostSnapshotRegion(HostCaptureRegion),
    Execute {
        target: ComputerTarget,
        action: ComputerAction,
        destination: Option<PathBuf>,
    },
}

/// An agent asks to use a local app by bundle id or name. Grants are checked, or requested from
/// the user, when the command runs.
pub struct ApplicationCommand {
    pub service: std::sync::Arc<bootty_agents::NativeAgentService>,
    pub conversation: bootty_control::CommandTarget,
    pub caller: bootty_control::Caller,
    pub application: String,
    pub action: bootty_agents::NativeApplicationAction,
}

/// Whether an agent may use this window.
///
/// Agents never drive this Bootty process, whose windows hold their own approval cards, nor
/// apps that run shell commands or change OS permissions outside the conversation's approvals.
/// Other Bootty instances, such as a development build under test, stay usable.
#[must_use]
pub fn agent_may_use(target: &ComputerTarget) -> bool {
    // Deliberate limit: a fixed list of well-known terminals. Extend it when users report
    // another terminal, or replace it with an OS-level category check when available.
    const BLOCKED: &[&str] = &[
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        "com.mitchellh.ghostty",
        "dev.warp.Warp-Stable",
        "net.kovidgoyal.kitty",
        "org.alacritty",
        "com.github.wez.wezterm",
        "com.apple.systempreferences",
        "com.apple.SecurityAgent",
        "org.kde.konsole",
        "konsole",
        "org.gnome.Terminal",
        "gnome-terminal-server",
        "org.gnome.Ptyxis",
        "ptyxis",
        "org.gnome.Console",
        "kgx",
        "org.wezfurlong.wezterm",
        "wezterm-gui",
        "Alacritty",
        "alacritty",
        "kitty",
        "Ghostty",
        "ghostty",
        "xterm",
        "XTerm",
        "foot",
        "footclient",
        "org.kde.systemsettings",
        "systemsettings",
        "org.gnome.Settings",
        "gnome-control-center",
    ];
    i32::try_from(std::process::id()).ok() != Some(target.process_id)
        && !BLOCKED.contains(&target.bundle_id.as_str())
}

/// How `describe` explains argument formats shared across computer commands.
const ARGUMENT_NOTES: [(&str, &str); 4] = [
    (
        "target",
        "Pass target as one window object from computer.targets, as JSON.",
    ),
    (
        "x",
        "x and y are screen points inside the target's bounds, not window-relative points.",
    ),
    (
        "region",
        "region is a JSON {x, y, width, height} rectangle in screen points inside the target's bounds.",
    ),
    (
        "element",
        "Pass element as one entry from computer.elements, as JSON.",
    ),
];

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
            "computer.icons",
            "List App Icons",
            "Return small PNG icons for apps named by a JSON array of bundle ids, without OS access.",
            &["bundle-ids"][..],
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
            "Click a point inside the exact selected window, even behind other windows on macOS.",
            &["target", "x", "y", "button"][..],
            MutationClass::Write,
        ),
        (
            "computer.move",
            "Move Computer Pointer",
            "Move the pointer to a point inside the exact selected window, even behind other windows on macOS.",
            &["target", "x", "y"][..],
            MutationClass::Write,
        ),
        (
            "computer.scroll",
            "Scroll Computer Window",
            "Scroll at a point inside the exact selected window, even behind other windows on macOS.",
            &["target", "x", "y", "delta-x", "delta-y"][..],
            MutationClass::Write,
        ),
        (
            "computer.type",
            "Type Computer Text",
            "Type bounded text into the exact selected window while it is its app's focused window.",
            &["target", "text"][..],
            MutationClass::Write,
        ),
        (
            "computer.focus",
            "Focus Computer Window",
            "Make the exact selected window its app's focused window; on macOS the app stays in the background.",
            &["target"][..],
            MutationClass::Write,
        ),
        (
            "computer.key",
            "Press Computer Key",
            "Post a key with optional comma-separated modifiers to the exact selected window while it is its app's focused window. On macOS, a Command shortcut for an app that is not frontmost presses the menu item with that shortcut instead, and fails when the item is disabled, as commands that act on a window are while their app is in the background.",
            &["target", "key", "modifiers"][..],
            MutationClass::Write,
        ),
        (
            "computer.elements",
            "List Computer Elements",
            "List the accessibility elements of the exact selected window, in screen order. With root menu_bar, list its app's menu bar instead, in menu order and without opening a menu or bringing the app forward; those elements have no frame, and the system's Apple menu and the app menu's Services are left out.",
            &["target", "root"][..],
            MutationClass::Read,
        ),
        (
            "computer.press",
            "Press Computer Element",
            "Press a listed element. Set allow-click to false to require its accessibility action without pointer input. Menu bar elements are pressed through accessibility only, and only menu items without a submenu. Success means the app accepted the press; a background app disables the menu commands that act on a window.",
            &["target", "element", "allow-click"][..],
            MutationClass::Write,
        ),
        (
            "computer.set-value",
            "Set Computer Element Value",
            "Replace a listed editable element's value through accessibility, without focus, clicks or key events. An empty value clears it. This does not submit the value.",
            &["target", "element", "value"][..],
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
                    !matches!(*name, "button" | "modifiers" | "allow-click" | "root")
                        && id != "computer.snapshot";
                if *name == "button" {
                    argument.choices = ["left", "right", "middle"].map(str::to_owned).to_vec();
                }
                if *name == "allow-click" {
                    argument.choices = ["true", "false"].map(str::to_owned).to_vec();
                }
                if *name == "root" {
                    argument.choices = ["window", "menu_bar"].map(str::to_owned).to_vec();
                }
                if matches!(*name, "delta-x" | "delta-y") {
                    argument.minimum = Some(-10_000);
                    argument.maximum = Some(10_000);
                }
                argument
            })
            .collect();
        let mut description = description.to_owned();
        for (name, note) in ARGUMENT_NOTES {
            if names.contains(&name) {
                description.push(' ');
                description.push_str(note);
            }
        }
        let descriptor = CommandDescriptor {
            id: id.to_owned(),
            title: title.to_owned(),
            description,
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
    parse(invocation).map_err(|reason| CommandOutcome::Failed {
        code: "invalid_arguments".to_owned(),
        message: format!("Invalid arguments for {}: {reason}", invocation.command),
    })
}

/// Parse a computer command, or say which argument is wrong and why.
fn parse(invocation: &CommandInvocation) -> Result<ComputerCommand, String> {
    let host_window = invocation
        .target
        .as_ref()
        .is_some_and(|target| target.kind == bootty_control::ResourceKind::ApplicationWindow);
    match invocation.command.as_str() {
        "computer.capture" => {
            return if invocation.arguments.is_empty() && host_window {
                Ok(ComputerCommand::HostCapture)
            } else {
                Err("takes no arguments and needs an application window target".to_owned())
            };
        }
        "computer.snapshot" if invocation.arguments.len() <= 1 => {
            if host_window {
                return resolve_host_snapshot(invocation.arguments.first().map(String::as_str));
            }
            return Err("target and an absolute destination path are required".to_owned());
        }
        "computer.status" if invocation.arguments.is_empty() => return Ok(ComputerCommand::Status),
        "computer.targets" if invocation.arguments.is_empty() => {
            return Ok(ComputerCommand::Targets);
        }
        "computer.icons" => return resolve_icons(&invocation.arguments),
        _ => {}
    }
    let target: ComputerTarget = serde_json::from_str(
        invocation
            .arguments
            .first()
            .ok_or_else(|| "target is required".to_owned())?,
    )
    .map_err(|error| {
        format!("target must be one window object from computer.targets, passed as JSON: {error}")
    })?;
    target.validate().map_err(|error| error.to_string())?;
    let (action, destination) = parse_action(&invocation.command, &invocation.arguments)?;
    target
        .validate_action(&action)
        .map_err(|error| error.to_string())?;
    Ok(ComputerCommand::Execute {
        target,
        action,
        destination,
    })
}

/// The action named by the arguments after the target, with a snapshot's destination.
fn parse_action(
    command: &str,
    arguments: &[String],
) -> Result<(ComputerAction, Option<PathBuf>), String> {
    let argument = |index: usize, name: &str| {
        arguments
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("{name} is required"))
    };
    let coordinate = |index, name| -> Result<f64, String> {
        argument(index, name)?
            .parse()
            .map_err(|_| format!("{name} must be a number"))
    };
    let delta = |index, name| -> Result<i32, String> {
        argument(index, name)?
            .parse()
            .map_err(|_| format!("{name} must be an integer"))
    };
    let element = |index| -> Result<Box<ComputerElement>, String> {
        serde_json::from_str(argument(index, "element")?).map_err(|error| {
            format!("element must be one entry from computer.elements, passed as JSON: {error}")
        })
    };
    let action = match command {
        "computer.snapshot" => {
            let path = PathBuf::from(argument(1, "destination")?);
            if !path.is_absolute() || path.file_name().is_none() {
                return Err("destination must be an absolute file path".to_owned());
            }
            let action = match arguments.get(2) {
                Some(rect) => ComputerAction::SnapshotRegion {
                    rect: serde_json::from_str(rect).map_err(|error| {
                        format!(
                            "region must be a JSON {{x, y, width, height}} rectangle in screen \
                             points: {error}"
                        )
                    })?,
                },
                None => ComputerAction::Snapshot,
            };
            return Ok((action, Some(path)));
        }
        "computer.focus" => ComputerAction::Focus,
        "computer.click" => ComputerAction::Click {
            x: coordinate(1, "x")?,
            y: coordinate(2, "y")?,
            button: arguments.get(3).map_or(Ok(MouseButton::Left), |button| {
                serde_json::from_value(serde_json::Value::String(button.clone()))
                    .map_err(|_| "button must be left, right or middle".to_owned())
            })?,
        },
        "computer.move" => ComputerAction::Move {
            x: coordinate(1, "x")?,
            y: coordinate(2, "y")?,
        },
        "computer.scroll" => ComputerAction::Scroll {
            x: coordinate(1, "x")?,
            y: coordinate(2, "y")?,
            delta_x: delta(3, "delta-x")?,
            delta_y: delta(4, "delta-y")?,
        },
        "computer.type" => ComputerAction::TypeText {
            text: argument(1, "text")?.to_owned(),
        },
        "computer.key" => resolve_key(argument(1, "key")?, arguments.get(2).map(String::as_str))?,
        "computer.elements" => match arguments.get(1).map_or(Ok(ElementRoot::Window), |root| {
            serde_json::from_value(serde_json::Value::String(root.clone()))
                .map_err(|_| "root must be window or menu_bar".to_owned())
        })? {
            ElementRoot::Window => ComputerAction::Elements,
            ElementRoot::MenuBar => ComputerAction::MenuBar,
        },
        "computer.press" => ComputerAction::Press {
            element: element(1)?,
            allow_click: arguments.get(2).map_or(Ok(true), |value| {
                value
                    .parse()
                    .map_err(|_| "allow-click must be true or false".to_owned())
            })?,
        },
        "computer.set-value" => ComputerAction::SetValue {
            element: element(1)?,
            value: argument(2, "value")?.to_owned(),
        },
        _ => return Err("unsupported arguments".to_owned()),
    };
    Ok((action, None))
}

/// One JSON array of at most [`bootty_computer::MAX_ICON_REQUEST`] bundle ids.
fn resolve_icons(arguments: &[String]) -> Result<ComputerCommand, String> {
    let [bundle_ids] = arguments else {
        return Err("bundle-ids is required".to_owned());
    };
    let ids: Vec<String> = serde_json::from_str(bundle_ids)
        .map_err(|error| format!("bundle-ids must be a JSON array of strings: {error}"))?;
    if ids.len() > bootty_computer::MAX_ICON_REQUEST {
        return Err(format!(
            "bundle-ids lists {} apps; at most {} are allowed",
            ids.len(),
            bootty_computer::MAX_ICON_REQUEST
        ));
    }
    Ok(ComputerCommand::Icons(ids))
}

fn resolve_host_snapshot(value: Option<&str>) -> Result<ComputerCommand, String> {
    let Some(value) = value else {
        return Ok(ComputerCommand::HostSnapshot);
    };
    let region: HostCaptureRegion = serde_json::from_str(value)
        .map_err(|error| format!("region must be a JSON window-frame crop: {error}"))?;
    region.validate().map_err(|error| error.to_string())?;
    Ok(ComputerCommand::HostSnapshotRegion(region))
}

fn resolve_key(key: &str, modifiers: Option<&str>) -> Result<ComputerAction, String> {
    let key = serde_json::from_value(serde_json::Value::String(key.to_owned())).map_err(|_| {
        format!("unknown key `{key}`; use a lowercase name such as a, 1, f5, return or page_up")
    })?;
    let modifiers = modifiers.map_or(Ok(Vec::new()), |modifiers| {
        modifiers
            .split(',')
            .filter(|modifier| !modifier.is_empty())
            .map(|modifier| {
                serde_json::from_value(serde_json::Value::String(modifier.to_owned())).map_err(
                    |_| {
                        format!(
                            "unknown modifier `{modifier}`; use command, control, option or shift"
                        )
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(ComputerAction::Key { key, modifiers })
}
