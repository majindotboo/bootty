use bootty_computer::{
    ComputerAction, ComputerTarget, DisplayBounds, HostCaptureRegion, Key, Modifier,
};
use bootty_control::{
    Caller, CommandInvocation, CommandOutcome, CommandTarget, MutationClass, ResourceKind,
};
use bootty_ui::commands::{
    CommandCatalog, CommandExecutor, ComputerCommand, CoreCommandExecutor, agent_may_use,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn target() -> ComputerTarget {
    ComputerTarget {
        window_id: 42,
        native_id: None,
        process_id: 123,
        bundle_id: "dev.bootty.test-target".into(),
        application_name: None,
        launch_time: 1000.0,
        bounds: DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        title: None,
    }
}

#[rstest]
#[case(Caller::CommandPalette)]
#[case(Caller::Keybinding)]
#[case(Caller::BuiltinKeybinding)]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
#[case(Caller::Luau)]
#[case(Caller::Internal)]
fn shortcuts_use_one_typed_command_for_all_callers(
    target: ComputerTarget,
    #[case] caller: Caller,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = CommandCatalog::default()
        .resolve(CommandInvocation::new(
            "computer.key",
            vec![
                serde_json::to_string(&target)?,
                "return".into(),
                "command,shift".into(),
            ],
            caller,
        ))
        .map_err(|outcome| format!("{outcome:?}"))?;
    let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
        return Err("shortcut did not resolve to the shared computer executor".into());
    };
    assert_eq!(
        actual,
        ComputerCommand::Execute {
            target,
            action: ComputerAction::Key {
                key: Key::Return,
                modifiers: vec![Modifier::Command, Modifier::Shift]
            },
            destination: None,
        }
    );
    assert_eq!(resolved.invocation.caller, caller);
    Ok(())
}

/// Asserts an `invalid_arguments` failure whose message starts with `reason`. Parser details may
/// follow the reason.
fn assert_refused(
    invocation: CommandInvocation,
    reason: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = CommandCatalog::default().resolve(invocation).err();
    let Some(CommandOutcome::Failed { code, message }) = &outcome else {
        return Err(format!("expected an argument failure, got {outcome:?}").into());
    };
    let head = message.get(..reason.len()).unwrap_or(message);
    assert_eq!(
        (code.as_str(), head),
        ("invalid_arguments", reason),
        "full message: {message}"
    );
    Ok(())
}

/// The fixture target's bounds, as refusals report them.
const OUTSIDE: &str = "outside the target bounds {x: 0, y: 0, width: 800, height: 600}";

#[rstest]
#[case(
    "computer.snapshot",
    vec!["relative.png".into()],
    "Invalid arguments for computer.snapshot: destination must be an absolute file path".into()
)]
#[case(
    "computer.snapshot",
    vec![
        std::env::temp_dir().join("bootty-crop.png").to_string_lossy().into_owned(),
        r#"{"x":700,"y":0,"width":200,"height":100}"#.into(),
    ],
    format!(
        "Invalid arguments for computer.snapshot: invalid computer action: \
         capture rectangle {{x: 700, y: 0, width: 200, height: 100}} is {OUTSIDE}"
    )
)]
#[case("computer.move", vec!["NaN".into(), "0".into()], "invalid x argument for computer.move".into())]
#[case(
    "computer.move",
    vec!["800".into(), "0".into()],
    format!(
        "Invalid arguments for computer.move: invalid computer action: coordinates (800, 0) \
         are {OUTSIDE}; x and y are screen points, not window-relative points"
    )
)]
#[case(
    "computer.click",
    vec!["0".into(), "0".into(), "unknown".into()],
    "invalid button argument for computer.click".into()
)]
#[case(
    "computer.scroll",
    vec!["0".into(), "0".into(), "10001".into(), "0".into()],
    "invalid delta-x argument for computer.scroll".into()
)]
#[case(
    "computer.type",
    vec![String::new()],
    "Invalid arguments for computer.type: invalid computer action: text must contain 1–4096 bytes".into()
)]
#[case(
    "computer.type",
    vec!["a".repeat(4097)],
    "Invalid arguments for computer.type: invalid computer action: text must contain 1–4096 bytes".into()
)]
#[case(
    "computer.key",
    vec!["return".into(), "command,command,command,command,command".into()],
    "Invalid arguments for computer.key: invalid computer action: at most four modifiers are allowed".into()
)]
#[case(
    "computer.key",
    vec!["return".into(), "hyper".into()],
    "Invalid arguments for computer.key: unknown modifier `hyper`; use command, control, option or shift".into()
)]
#[case(
    "computer.elements",
    vec!["extra".into()],
    "invalid root argument for computer.elements".into()
)]
#[case(
    "computer.press",
    vec!["{}".into()],
    "Invalid arguments for computer.press: element must be one entry from computer.elements, passed as JSON: ".into()
)]
#[case(
    "computer.press",
    vec![element_json(900.0)],
    format!("Invalid arguments for computer.press: invalid computer action: the element center (910, 20) is {OUTSIDE}")
)]
#[case(
    "computer.press",
    vec![element_json(10.0), "maybe".into()],
    "invalid allow-click argument for computer.press".into()
)]
#[case(
    "computer.set-value",
    vec![element_json(10.0), "a".repeat(4097)],
    "Invalid arguments for computer.set-value: invalid computer action: value must contain at most 4096 bytes and no NUL".into()
)]
#[case(
    "computer.set-value",
    vec![element_json(10.0), "a\0b".into()],
    "Invalid arguments for computer.set-value: invalid computer action: value must contain at most 4096 bytes and no NUL".into()
)]
#[case(
    "computer.set-value",
    vec![element_json(900.0), "text".into()],
    format!("Invalid arguments for computer.set-value: invalid computer action: the element center (910, 20) is {OUTSIDE}")
)]
#[case(
    "computer.set-value",
    vec![MENU_ITEM.into(), "text".into()],
    "Invalid arguments for computer.set-value: invalid computer action: \
     menu bar items can only be pressed"
        .into()
)]
fn malformed_actions_are_refused_with_their_reason(
    target: ComputerTarget,
    #[case] command: &str,
    #[case] arguments: Vec<String>,
    #[case] reason: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut values = vec![serde_json::to_string(&target)?];
    values.extend(arguments);
    assert_refused(
        CommandInvocation::new(command, values, Caller::Socket),
        &reason,
    )
}

#[rstest]
#[case::window_id("4036")]
#[case::empty("{}")]
#[case::fake_permission(
    "{\"window_id\":42,\"process_id\":123,\"bundle_id\":\"dev.bootty.test-target\",\"launch_time\":1000,\"bounds\":{\"x\":0,\"y\":0,\"width\":800,\"height\":600},\"accessibility_granted\":true}"
)]
fn targets_other_than_listed_window_objects_are_refused(
    #[case] target: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_refused(
        CommandInvocation::new(
            "computer.type",
            vec![target.into(), "hello".into()],
            Caller::Socket,
        ),
        "Invalid arguments for computer.type: \
         target must be one window object from computer.targets, passed as JSON: ",
    )
}

/// `describe` says where targets and elements come from, and which space points use.
#[rstest]
#[case(
    "computer.type",
    "Pass target as one window object from computer.targets, as JSON."
)]
#[case(
    "computer.click",
    "x and y are screen points inside the target's bounds"
)]
#[case(
    "computer.move",
    "x and y are screen points inside the target's bounds"
)]
#[case(
    "computer.scroll",
    "x and y are screen points inside the target's bounds"
)]
#[case(
    "computer.snapshot",
    "region is a JSON {x, y, width, height} rectangle in screen points"
)]
#[case(
    "computer.press",
    "Pass element as one entry from computer.elements, as JSON."
)]
fn descriptions_explain_argument_formats(#[case] id: &str, #[case] note: &str) {
    let descriptor = CommandCatalog::default()
        .describe(id)
        .expect("computer command descriptor");
    assert!(
        descriptor.description.contains(note),
        "{id} description lacks {note:?}: {}",
        descriptor.description
    );
}

/// A menu item as `computer.elements <target> menu_bar` returns it, without a frame.
const MENU_ITEM: &str = r#"{"path":[2,0,1],"root":"menu_bar","role":"MenuItem","name":"New Tab"}"#;

/// The menu bar listing and its items go through the same commands as window elements.
#[rstest]
#[case(vec!["menu_bar".into()], "computer.elements", MutationClass::Read)]
#[case(vec!["window".into()], "computer.elements", MutationClass::Read)]
#[case(vec![MENU_ITEM.into()], "computer.press", MutationClass::Write)]
fn menu_bar_commands_share_the_typed_computer_executor(
    target: ComputerTarget,
    #[case] arguments: Vec<String>,
    #[case] id: &str,
    #[case] mutation: MutationClass,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut values = vec![serde_json::to_string(&target)?];
    values.extend(arguments.clone());
    let resolved = CommandCatalog::default()
        .resolve(CommandInvocation::new(id, values, Caller::Cli))
        .map_err(|outcome| format!("{outcome:?}"))?;
    assert_eq!(resolved.descriptor.mutation, mutation);
    let action = match arguments[0].as_str() {
        "menu_bar" => ComputerAction::MenuBar,
        "window" => ComputerAction::Elements,
        element => ComputerAction::Press {
            element: Box::new(serde_json::from_str(element)?),
            allow_click: true,
        },
    };
    let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
        return Err("menu bar command did not resolve to the shared computer executor".into());
    };
    assert_eq!(
        actual,
        ComputerCommand::Execute {
            target,
            action,
            destination: None,
        }
    );
    Ok(())
}

/// A listed element as `computer.elements` returns it; `x` places its frame.
fn element_json(x: f64) -> String {
    serde_json::json!({"path":[0, 2],"role":"Button","name":"Back",
        "frame":{"x":x,"y":10.0,"width":20.0,"height":20.0}})
    .to_string()
}

#[rstest]
#[case("computer.elements", vec![], MutationClass::Read)]
#[case("computer.set-value", vec![element_json(10.0), "text".into()], MutationClass::Write)]
#[case("computer.set-value", vec![element_json(10.0), String::new()], MutationClass::Write)]
#[case("computer.press", vec![element_json(10.0)], MutationClass::Write)]
#[case("computer.press", vec![element_json(10.0), "false".into()], MutationClass::Write)]
fn element_commands_share_the_typed_computer_executor(
    target: ComputerTarget,
    #[case] id: &str,
    #[case] arguments: Vec<String>,
    #[case] mutation: MutationClass,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut values = vec![serde_json::to_string(&target)?];
    values.extend(arguments.clone());
    let resolved = CommandCatalog::default()
        .resolve(CommandInvocation::new(id, values, Caller::Socket))
        .map_err(|outcome| format!("{outcome:?}"))?;
    assert_eq!(resolved.descriptor.mutation, mutation);
    let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
        return Err("element command did not resolve to the shared computer executor".into());
    };
    let action = match arguments.first() {
        Some(element) if id == "computer.set-value" => ComputerAction::SetValue {
            element: Box::new(serde_json::from_str(element)?),
            value: arguments[1].clone(),
        },
        Some(element) => ComputerAction::Press {
            element: Box::new(serde_json::from_str(element)?),
            allow_click: arguments.get(1).map_or(Ok(true), |value| value.parse())?,
        },
        None => ComputerAction::Elements,
    };
    assert_eq!(
        actual,
        ComputerCommand::Execute {
            target,
            action,
            destination: None,
        }
    );
    Ok(())
}

#[rstest]
#[case("computer.status", ComputerCommand::Status)]
#[case("computer.targets", ComputerCommand::Targets)]
fn discovery_commands_are_read_only(
    #[case] id: &str,
    #[case] expected: ComputerCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = CommandCatalog::default()
        .resolve(CommandInvocation::new(id, Vec::new(), Caller::Socket))
        .map_err(|outcome| format!("{outcome:?}"))?;
    assert_eq!(
        resolved.descriptor.mutation,
        bootty_control::MutationClass::Read
    );
    let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
        return Err("discovery did not resolve to the shared computer executor".into());
    };
    assert_eq!(actual, expected);
    Ok(())
}

proptest! {
    #[test]
    fn host_capture_preserves_the_original_window(handle in "[a-zA-Z0-9_-]{1,48}", generation in 1_u64..u64::MAX) {
        let window = CommandTarget { kind: ResourceKind::ApplicationWindow, handle, generation };
        let mut invocation = CommandInvocation::new("computer.snapshot", Vec::new(), Caller::Internal);
        invocation.target = Some(window.clone());
        let resolved = CommandCatalog::default().resolve(invocation)
            .map_err(|outcome| TestCaseError::fail(format!("{outcome:?}")))?;
        let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
            return Err(TestCaseError::fail("capture did not resolve to the shared computer executor"));
        };
        prop_assert_eq!(actual, ComputerCommand::HostSnapshot);
        prop_assert_eq!(resolved.invocation.target, Some(window));
        prop_assert_eq!(resolved.invocation.arguments, Vec::<String>::new());
        prop_assert_eq!(resolved.invocation.caller, Caller::Internal);
        prop_assert_eq!(resolved.descriptor.target, Some(ResourceKind::ApplicationWindow));
    }
}

#[rstest]
#[case(None)]
#[case(Some(ResourceKind::Binding))]
#[case(Some(ResourceKind::Terminal))]
#[case(Some(ResourceKind::Instance))]
fn host_capture_has_no_implicit_or_client_selected_window(
    #[case] kind: Option<ResourceKind>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut invocation = CommandInvocation::new("computer.snapshot", Vec::new(), Caller::Socket);
    invocation.target = kind.map(|kind| CommandTarget {
        kind,
        handle: "test-target".into(),
        generation: 1,
    });
    let outcome = CommandCatalog::default()
        .resolve(invocation)
        .err()
        .ok_or("host capture accepted a missing or wrong window kind")?;
    let CommandOutcome::Failed { code, .. } = outcome else {
        return Err("host capture did not produce an argument failure".into());
    };
    assert_eq!(code, "invalid_arguments");
    Ok(())
}

#[rstest]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
fn explicit_snapshot_target_and_path_keep_the_existing_command_shape(
    target: ComputerTarget,
    #[case] caller: Caller,
) -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::env::temp_dir().join("bootty-manual-snapshot.png");
    let resolved = CommandCatalog::default()
        .resolve(CommandInvocation::new(
            "computer.snapshot",
            vec![
                serde_json::to_string(&target)?,
                destination.to_string_lossy().into_owned(),
            ],
            caller,
        ))
        .map_err(|outcome| format!("{outcome:?}"))?;
    let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
        return Err("manual snapshot did not resolve to the shared computer executor".into());
    };
    assert_eq!(
        actual,
        ComputerCommand::Execute {
            target,
            action: ComputerAction::Snapshot,
            destination: Some(destination)
        }
    );
    assert_eq!(resolved.invocation.caller, caller);
    assert_eq!(resolved.descriptor.target, None);
    Ok(())
}

#[rstest]
#[case(Caller::Internal)]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
fn host_crop_preserves_exact_window_and_frame_geometry(
    #[case] caller: Caller,
) -> Result<(), Box<dyn std::error::Error>> {
    let window = CommandTarget {
        kind: ResourceKind::ApplicationWindow,
        handle: "exact-window".into(),
        generation: 37,
    };
    let geometry = HostCaptureRegion {
        frame_width: 800.0,
        frame_height: 600.0,
        rect: DisplayBounds {
            x: 600.5,
            y: 60.0,
            width: 199.5,
            height: 500.0,
        },
    };
    let mut invocation = CommandInvocation::new(
        "computer.snapshot",
        vec![serde_json::to_string(&geometry)?],
        caller,
    );
    invocation.target = Some(window.clone());
    let resolved = CommandCatalog::default()
        .resolve(invocation)
        .map_err(|outcome| format!("{outcome:?}"))?;
    let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
        return Err("host crop did not resolve to shared computer executor".into());
    };
    assert_eq!(actual, ComputerCommand::HostSnapshotRegion(geometry));
    assert_eq!(resolved.invocation.target, Some(window));
    assert_eq!(
        resolved.descriptor.target,
        Some(ResourceKind::ApplicationWindow)
    );
    Ok(())
}

#[rstest]
#[case(700.5, 100.0)]
#[case(-0.5, 100.0)]
#[case(0.0, 0.0)]
fn host_crop_rejects_partial_edges_before_dispatch(
    #[case] x: f64,
    #[case] width: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let geometry = HostCaptureRegion {
        frame_width: 800.0,
        frame_height: 600.0,
        rect: DisplayBounds {
            x,
            y: 0.0,
            width,
            height: 100.0,
        },
    };
    let mut invocation = CommandInvocation::new(
        "computer.snapshot",
        vec![serde_json::to_string(&geometry)?],
        Caller::Internal,
    );
    invocation.target = Some(CommandTarget {
        kind: ResourceKind::ApplicationWindow,
        handle: "exact-window".into(),
        generation: 37,
    });
    let Err(CommandOutcome::Failed { code, .. }) = CommandCatalog::default().resolve(invocation)
    else {
        return Err("invalid crop was not rejected before dispatch".into());
    };
    assert_eq!(code, "invalid_arguments");
    Ok(())
}

#[rstest]
fn explicit_crop_keeps_the_captured_target_and_destination(
    target: ComputerTarget,
) -> Result<(), Box<dyn std::error::Error>> {
    let rect = DisplayBounds {
        x: 20.0,
        y: 40.0,
        width: 100.0,
        height: 80.0,
    };
    let path = std::env::temp_dir().join("bootty-explicit-crop.png");
    let resolved = CommandCatalog::default()
        .resolve(CommandInvocation::new(
            "computer.snapshot",
            vec![
                serde_json::to_string(&target)?,
                path.to_string_lossy().into_owned(),
                serde_json::to_string(&rect)?,
            ],
            Caller::Socket,
        ))
        .map_err(|outcome| format!("{outcome:?}"))?;
    let CommandExecutor::Core(CoreCommandExecutor::Computer(actual)) = resolved.executor else {
        return Err("explicit crop did not resolve to shared computer executor".into());
    };
    assert_eq!(
        actual,
        ComputerCommand::Execute {
            target,
            action: ComputerAction::SnapshotRegion { rect },
            destination: Some(path)
        }
    );
    Ok(())
}

#[rstest]
#[case(Caller::Internal)]
#[case(Caller::Socket)]
#[case(Caller::Cli)]
fn inline_capture_requires_exact_original_application_window(#[case] caller: Caller) {
    let target = CommandTarget {
        kind: ResourceKind::ApplicationWindow,
        handle: "exact-window".into(),
        generation: 37,
    };
    let mut invocation = CommandInvocation::new("computer.capture", Vec::new(), caller);
    invocation.target = Some(target.clone());
    let resolved = CommandCatalog::default().resolve(invocation).unwrap();
    assert!(matches!(
        resolved.executor,
        CommandExecutor::Core(CoreCommandExecutor::Computer(ComputerCommand::HostCapture))
    ));
    assert_eq!(resolved.invocation.target, Some(target));
    assert_eq!(
        resolved.descriptor.target,
        Some(ResourceKind::ApplicationWindow)
    );
    assert_eq!(
        resolved.descriptor.mutation,
        bootty_control::MutationClass::Read
    );
    assert!(!resolved.descriptor.palette);
}

#[rstest]
#[case(None, Vec::new())]
#[case(Some(ResourceKind::Binding), Vec::new())]
#[case(Some(ResourceKind::Terminal), Vec::new())]
#[case(Some(ResourceKind::ApplicationWindow), vec!["/caller/path.png".into()])]
#[case(Some(ResourceKind::ApplicationWindow), vec!["{\"window_id\":42}".into()])]
fn inline_capture_cannot_select_a_path_or_native_target(
    #[case] kind: Option<ResourceKind>,
    #[case] arguments: Vec<String>,
) {
    let mut invocation = CommandInvocation::new("computer.capture", arguments, Caller::Socket);
    invocation.target = kind.map(|kind| CommandTarget {
        kind,
        handle: "exact-target".into(),
        generation: 37,
    });
    assert!(
        matches!(CommandCatalog::default().resolve(invocation), Err(CommandOutcome::Failed {code,..}) if code=="invalid_arguments")
    );
}

/// Agents never drive the Bootty they run in, whose windows hold their approval cards, nor
/// terminals; another Bootty instance is an ordinary app.
#[rstest]
#[case::this_bootty(i32::try_from(std::process::id()).unwrap(), "dev.bootty.desktop", false)]
#[case::another_bootty(123, "dev.bootty.desktop.dev.0123456789abcdef", true)]
#[case::terminal(123, "com.mitchellh.ghostty", false)]
#[case::system_settings(123, "com.apple.systempreferences", false)]
#[case::linux_terminal(123, "org.kde.konsole", false)]
#[case::linux_terminal_executable(123, "konsole", false)]
#[case::linux_settings(123, "org.gnome.Settings", false)]
#[case::editor(123, "dev.zed.Zed", true)]
fn agents_use_any_app_except_their_host_and_shells(
    target: ComputerTarget,
    #[case] process_id: i32,
    #[case] bundle_id: &str,
    #[case] allowed: bool,
) {
    let window = ComputerTarget {
        process_id,
        bundle_id: bundle_id.into(),
        ..target
    };
    assert_eq!(agent_may_use(&window), allowed);
}
