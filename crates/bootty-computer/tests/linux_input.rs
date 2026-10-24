#![allow(
    clippy::too_many_lines,
    reason = "One opt-in native journey verifies cleanup as well as delivery."
)]
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Native fixture failures must fail immediately."
)]

use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerResult, ComputerTarget, Key, MouseButton,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

use std::{
    io::{BufRead as _, BufReader},
    process::{Child, Command, Stdio},
};
use x11rb::{connection::Connection as _, protocol::xproto::ConnectionExt as _};

struct Probe(Child);
impl Drop for Probe {
    fn drop(&mut self) {
        _ = self.0.kill();
        _ = self.0.wait();
    }
}

fn event(events: &mut BufReader<std::process::ChildStdout>, kind: &str) -> serde_json::Value {
    loop {
        let mut line = String::new();
        assert_ne!(events.read_line(&mut line).unwrap(), 0);
        let event: serde_json::Value = serde_json::from_str(&line).unwrap();
        if event.get("event").and_then(serde_json::Value::as_str) == Some(kind) {
            return event;
        }
    }
}

async fn act(computer: &Computer, target: &ComputerTarget, action: ComputerAction) {
    assert_eq!(
        computer
            .execute(&ComputerAccess::from_user_setting(true), target, &action)
            .await
            .unwrap(),
        ComputerResult::Posted
    );
}

// This is deliberately opt-in: the caller supplies an owned display, never the user's desktop.
#[rstest]
#[tokio::test]
#[ignore = "Uses the owned X11 display in BOOTTY_TEST_DISPLAY and Python GI GTK3/AT-SPI."]
async fn gtk_x11_independent_buttons_keys_and_cleanup() {
    let display =
        std::env::var("BOOTTY_TEST_DISPLAY").expect("an explicitly owned display is required");
    let mut probe = Probe(
        Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/atspi_probe.py"
            ))
            .env("DISPLAY", &display)
            .env("GDK_BACKEND", "x11")
            .env_remove("NO_AT_BRIDGE")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut events = BufReader::new(probe.0.stdout.take().unwrap());
    event(&mut events, "ready");
    let mut cover = Probe(
        Command::new("python3")
            .args([
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/atspi_probe.py"),
                "cover",
                "offset",
            ])
            .env("DISPLAY", &display)
            .env("GDK_BACKEND", "x11")
            .env_remove("NO_AT_BRIDGE")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut cover_events = BufReader::new(cover.0.stdout.take().unwrap());
    event(&mut cover_events, "ready");
    let computer = Computer::new();
    let access = ComputerAccess::from_user_setting(true);
    let targets = computer.targets_on_display(&display).await.unwrap();
    let target = targets
        .iter()
        .find(|target| {
            target.process_id == i32::try_from(probe.0.id()).unwrap() && target.native_id.is_none()
        })
        .expect("real X11 discovery")
        .clone();
    let cover_target = targets
        .iter()
        .find(|target| target.process_id == target_pid(&cover) && target.native_id.is_none())
        .unwrap();
    act(&computer, cover_target, ComputerAction::Focus).await;
    let semantic = targets
        .iter()
        .find(|target| target.process_id == target_pid(&probe) && target.native_id.is_some())
        .expect("separate real AT-SPI discovery")
        .clone();
    assert_eq!(target.display.as_deref(), Some(display.as_str()));
    assert!(target.backend_id.is_some());
    assert!(semantic.display.is_none() && semantic.backend_id.is_none());
    let ComputerResult::Elements { elements, .. } = computer
        .execute(&access, &semantic, &ComputerAction::Elements)
        .await
        .unwrap()
    else {
        panic!("elements");
    };
    let entry = elements
        .iter()
        .find(|element| element.name.as_deref() == Some("Probe text"))
        .unwrap();
    assert_eq!(entry.value.as_deref(), Some("original"));
    act(
        &computer,
        &semantic,
        ComputerAction::SetValue {
            element: Box::new(entry.clone()),
            value: "Independent input 🥟".into(),
        },
    )
    .await;
    assert_eq!(event(&mut events, "changed")["active"], false);
    let ComputerResult::Elements { elements, .. } = computer
        .execute(&access, &semantic, &ComputerAction::Elements)
        .await
        .unwrap()
    else {
        panic!("elements");
    };
    assert!(
        elements
            .iter()
            .any(|element| element.value.as_deref() == Some("Independent input 🥟"))
    );
    let button = elements
        .iter()
        .find(|element| element.name.as_deref() == Some("Probe press"))
        .unwrap();
    assert!(matches!(
        computer
            .execute(
                &access,
                &semantic,
                &ComputerAction::Press {
                    element: Box::new(button.clone()),
                    allow_click: false
                }
            )
            .await
            .unwrap(),
        ComputerResult::Pressed { .. }
    ));
    assert_eq!(event(&mut events, "pressed")["active"], false);
    let ComputerResult::Elements { elements, .. } = computer
        .execute(&access, &semantic, &ComputerAction::Elements)
        .await
        .unwrap()
    else {
        panic!("elements");
    };
    assert!(
        elements
            .iter()
            .any(|element| element.name.as_deref() == Some("presses=1"))
    );
    assert!(matches!(
        computer
            .execute(&access, &target, &ComputerAction::Snapshot)
            .await
            .unwrap(),
        ComputerResult::Snapshot { .. }
    ));

    let (x11, screen) = x11rb::connect(Some(&display)).unwrap();
    let root = x11.setup().roots[screen].root;
    let pointer_mask = || u16::from(x11.query_pointer(root).unwrap().reply().unwrap().mask) & 0x700;
    let (x, y) = (
        target.bounds.x + 30.0,
        target.bounds.y + target.bounds.height - 30.0,
    );
    for presses in [
        [MouseButton::Left, MouseButton::Right],
        [MouseButton::Right, MouseButton::Left],
    ] {
        for releases in [
            [MouseButton::Left, MouseButton::Right],
            [MouseButton::Right, MouseButton::Left],
        ] {
            assert_eq!(pointer_mask(), 0);
            for button in presses {
                act(
                    &computer,
                    &target,
                    ComputerAction::ButtonDown { x, y, button },
                )
                .await;
            }
            assert_eq!(
                pointer_mask(),
                0x500,
                "both buttons must really be down on the X server"
            );
            for button in releases {
                act(
                    &computer,
                    &target,
                    ComputerAction::ButtonUp { x, y, button },
                )
                .await;
            }
            assert_eq!(pointer_mask(), 0);
            let mut received = Vec::new();
            while received.len() < 4 {
                let input = event(&mut events, "input");
                if input["type"] == "button-press" || input["type"] == "button-release" {
                    assert_eq!(input["active"], false);
                    received.push((
                        input["type"].as_str().unwrap().to_owned(),
                        input["button"].as_u64().unwrap(),
                    ));
                }
            }
            let code = |button| if button == MouseButton::Left { 1 } else { 3 };
            assert_eq!(
                received,
                vec![
                    ("button-press".into(), code(presses[0])),
                    ("button-press".into(), code(presses[1])),
                    ("button-release".into(), code(releases[0])),
                    ("button-release".into(), code(releases[1]))
                ]
            );
            println!("button order accepted: {received:?}");
        }
    }
    act(
        &computer,
        &target,
        ComputerAction::ButtonDown {
            x,
            y,
            button: MouseButton::Left,
        },
    )
    .await;
    assert!(
        computer
            .execute(
                &access,
                cover_target,
                &ComputerAction::ButtonDown {
                    x: cover_target.bounds.x + 20.0,
                    y: cover_target.bounds.y + 20.0,
                    button: MouseButton::Right
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        pointer_mask(),
        0,
        "changing the exact target during a hold is rejected and cleaned up"
    );
    // The same real token is stale for a new backend owner, even on the same display.
    act(
        &computer,
        &target,
        ComputerAction::ButtonDown {
            x,
            y,
            button: MouseButton::Left,
        },
    )
    .await;
    let replacement = Computer::new();
    assert!(
        replacement
            .execute(
                &access,
                &target,
                &ComputerAction::ButtonDown {
                    x,
                    y,
                    button: MouseButton::Right
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        pointer_mask(),
        0x100,
        "a different owner must not release this owner's button"
    );
    act(&computer, &target, ComputerAction::ReleaseInputs).await;
    assert_eq!(pointer_mask(), 0);
    // A real old token becomes invalid when its original window is resized.
    act(
        &computer,
        &target,
        ComputerAction::ButtonDown {
            x,
            y,
            button: MouseButton::Left,
        },
    )
    .await;
    x11.configure_window(
        target.window_id,
        &x11rb::protocol::xproto::ConfigureWindowAux::new().width(500),
    )
    .unwrap()
    .check()
    .unwrap();
    assert!(
        computer
            .execute(
                &access,
                &target,
                &ComputerAction::ButtonUp {
                    x,
                    y,
                    button: MouseButton::Left
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        pointer_mask(),
        0,
        "failed identity validation releases owned input"
    );
    let target = computer
        .targets_on_display(&display)
        .await
        .unwrap()
        .into_iter()
        .find(|current| current.process_id == target.process_id && current.native_id.is_none())
        .unwrap();
    // Keyboard events need actual X11 focus. This explicit focus action affects only the owned display.
    act(&computer, &target, ComputerAction::Focus).await;
    let keys_down = || x11.query_keymap().unwrap().reply().unwrap().keys;
    let count = |keys: [u8; 32]| keys.into_iter().map(u8::count_ones).sum::<u32>();
    assert_eq!(count(keys_down()), 0);
    for presses in [[Key::Down, Key::Right], [Key::Control, Key::Right]] {
        for releases in [presses, [presses[1], presses[0]]] {
            for key in presses {
                act(&computer, &target, ComputerAction::KeyDown { key }).await;
            }
            assert_eq!(count(keys_down()), 2);
            act(
                &computer,
                &target,
                ComputerAction::KeyUp { key: releases[0] },
            )
            .await;
            assert_eq!(count(keys_down()), 1);
            act(
                &computer,
                &target,
                ComputerAction::KeyUp { key: releases[1] },
            )
            .await;
            assert_eq!(count(keys_down()), 0);
            let mut received = Vec::new();
            while received.len() < 4 {
                let input = event(&mut events, "input");
                if input["type"] == "key-press" || input["type"] == "key-release" {
                    received.push((
                        input["type"].as_str().unwrap().to_owned(),
                        input["key"].as_str().unwrap().to_owned(),
                    ));
                }
            }
            let name = |key| match key {
                Key::Down => "Down",
                Key::Right => "Right",
                Key::Control => "Control_L",
                _ => panic!("not one of the fixture keys"),
            };
            assert_eq!(
                received,
                vec![
                    ("key-press".into(), name(presses[0]).into()),
                    ("key-press".into(), name(presses[1]).into()),
                    ("key-release".into(), name(releases[0]).into()),
                    ("key-release".into(), name(releases[1]).into())
                ]
            );
            println!(
                "independent keyboard state accepted: {presses:?} -> {releases:?}, received {received:?}"
            );
        }
    }
    act(
        &computer,
        &target,
        ComputerAction::KeyDown { key: Key::Keypad1 },
    )
    .await;
    assert_eq!(count(keys_down()), 1);
    act(
        &computer,
        &target,
        ComputerAction::KeyUp { key: Key::Keypad1 },
    )
    .await;
    assert_eq!(count(keys_down()), 0);
    act(
        &computer,
        &target,
        ComputerAction::KeyDown { key: Key::Control },
    )
    .await;
    act(
        &computer,
        &target,
        ComputerAction::ButtonDown {
            x,
            y,
            button: MouseButton::Left,
        },
    )
    .await;
    act(&computer, &target, ComputerAction::ReleaseInputs).await;
    assert_eq!(count(keys_down()), 0);
    assert_eq!(pointer_mask(), 0);
    act(
        &computer,
        &target,
        ComputerAction::KeyDown { key: Key::Control },
    )
    .await;
    assert!(
        computer
            .execute(
                &access,
                cover_target,
                &ComputerAction::KeyDown { key: Key::Right }
            )
            .await
            .is_err()
    );
    assert_eq!(
        count(keys_down()),
        0,
        "wrong keyboard target failure releases the held modifier"
    );
    act(
        &computer,
        &target,
        ComputerAction::KeyDown { key: Key::Control },
    )
    .await;
    act(
        &computer,
        &target,
        ComputerAction::ButtonDown {
            x,
            y,
            button: MouseButton::Right,
        },
    )
    .await;
    drop(computer);
    for _ in 0..100 {
        if count(keys_down()) == 0 && pointer_mask() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(count(keys_down()), 0, "owner shutdown releases held keys");
    assert_eq!(pointer_mask(), 0, "owner shutdown releases held buttons");
    println!("exact target {target:?}; semantic target {semantic:?}; both input classes released");
}

fn target_pid(probe: &Probe) -> i32 {
    i32::try_from(probe.0.id()).unwrap()
}
