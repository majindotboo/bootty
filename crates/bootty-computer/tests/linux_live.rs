#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Fixture failures must fail the acceptance test immediately."
)]

use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerElement, ComputerResult, ComputerTarget,
    PressMethod,
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
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn observe(
    computer: &Computer,
    access: &ComputerAccess,
    target: &ComputerTarget,
) -> Vec<ComputerElement> {
    let ComputerResult::Elements {
        elements, omitted, ..
    } = computer
        .execute(access, target, &ComputerAction::Elements)
        .await
        .unwrap()
    else {
        panic!("expected elements")
    };
    assert!(!omitted);
    elements
}

#[rstest]
#[tokio::test]
#[ignore = "Opens an owned native Wayland GTK window; requires Python GI GTK3, AT-SPI, and a live desktop."]
async fn gtk_wayland_semantics_leave_focus_and_pointer_alone() {
    let mut probe = Probe(
        Command::new("python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/atspi_probe.py"
            ))
            .env("GDK_BACKEND", "wayland")
            .env_remove("NO_AT_BRIDGE")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut events = BufReader::new(probe.0.stdout.take().unwrap());
    let mut event = String::new();
    let ready = loop {
        event.clear();
        assert_ne!(events.read_line(&mut event).unwrap(), 0);
        let value: serde_json::Value = serde_json::from_str(&event).unwrap();
        if value["event"] == "ready" {
            break value;
        }
    };
    assert_eq!(ready["event"], "ready");
    assert_eq!(ready["display"], "GdkWaylandDisplay");
    let mut cover = Probe(
        Command::new("python3")
            .args([
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/atspi_probe.py"),
                "cover",
            ])
            .env("GDK_BACKEND", "wayland")
            .env_remove("NO_AT_BRIDGE")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut cover_events = BufReader::new(cover.0.stdout.take().unwrap());
    loop {
        event.clear();
        assert_ne!(cover_events.read_line(&mut event).unwrap(), 0);
        let value: serde_json::Value = serde_json::from_str(&event).unwrap();
        if value["active"] == true {
            break;
        }
    }

    let computer = Computer::new();
    let access = ComputerAccess::from_user_setting(true);
    let mut target = None;
    for _ in 0..20 {
        target = computer
            .targets()
            .await
            .unwrap()
            .into_iter()
            .find(|target| {
                target.process_id == i32::try_from(probe.0.id()).unwrap()
                    && target.native_id.is_some()
            });
        if target.is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let target = target.expect("The probe must register a native accessibility window");
    let observed = observe(&computer, &access, &target).await;
    let entry = observed
        .iter()
        .find(|element| element.name.as_deref() == Some("Probe text"))
        .unwrap();
    let button = observed
        .iter()
        .find(|element| element.name.as_deref() == Some("Probe press"))
        .unwrap();
    assert!(
        observed
            .iter()
            .find(|element| element.name.as_deref() == Some("Probe password"))
            .unwrap()
            .value
            .is_none()
    );
    let (x11, screen) = x11rb::connect(None).unwrap();
    let root = x11.setup().roots.get(screen).unwrap().root;
    let pointer = x11.query_pointer(root).unwrap().reply().unwrap();

    assert_eq!(
        Computer::new()
            .execute(
                &access,
                &target,
                &ComputerAction::SetValue {
                    element: Box::new(entry.clone()),
                    value: "Bootty 🥟".into(),
                }
            )
            .await
            .unwrap(),
        ComputerResult::Posted
    );
    assert_eq!(
        Computer::new()
            .execute(
                &access,
                &target,
                &ComputerAction::Press {
                    element: Box::new(button.clone()),
                    allow_click: false,
                }
            )
            .await
            .unwrap(),
        ComputerResult::Pressed {
            method: PressMethod::Action
        }
    );
    loop {
        event.clear();
        assert_ne!(events.read_line(&mut event).unwrap(), 0);
        let accepted: serde_json::Value = serde_json::from_str(&event).unwrap();
        if accepted["event"] == "changed" || accepted["event"] == "pressed" {
            assert_eq!(accepted["active"], false);
        }
        if accepted["event"] == "pressed" {
            break;
        }
    }
    let current = observe(&computer, &access, &target).await;
    assert_eq!(
        current
            .iter()
            .find(|element| element.name.as_deref() == Some("Probe text"))
            .and_then(|element| element.value.as_deref()),
        Some("Bootty 🥟")
    );
    assert!(
        current
            .iter()
            .any(|element| element.name.as_deref() == Some("presses=1"))
    );
    let after = x11.query_pointer(root).unwrap().reply().unwrap();
    assert_eq!(
        (after.root_x, after.root_y),
        (pointer.root_x, pointer.root_y)
    );
    println!(
        "Native Wayland GTK accepted Unicode set_value and semantic press in the background; pointer stayed at ({}, {}).",
        after.root_x, after.root_y
    );
}
