#![cfg(target_os = "windows")]
#![expect(
    unsafe_code,
    reason = "Read-only Win32 probes verify UIA does not move focus or the pointer."
)]

use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerElement, ComputerResult, PressMethod,
};
use windows::Win32::{
    Foundation::POINT,
    UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow},
};

async fn elements(
    computer: &Computer,
    access: &ComputerAccess,
    target: &bootty_computer::ComputerTarget,
) -> Result<Vec<ComputerElement>, Box<dyn std::error::Error>> {
    match computer
        .execute(access, target, &ComputerAction::Elements)
        .await?
    {
        ComputerResult::Elements {
            elements, omitted, ..
        } => {
            if omitted {
                return Err("Calculator's tree should fit in one observation".into());
            }
            Ok(elements)
        }
        result => Err(format!("Expected accessibility elements, got {result:?}").into()),
    }
}

#[tokio::test]
#[ignore = "Requires an open, non-minimized Calculator behind another app; changes its current calculation."]
async fn calculator_uia_calculates_in_background_without_capture_or_input_events() {
    let computer = Computer::new();
    let access = ComputerAccess::from_user_setting(true);
    let targets = computer.targets().await.unwrap();
    let candidates: Vec<_> = targets
        .iter()
        .filter(|target| target.title.as_deref() == Some("Calculator"))
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "Open exactly one Windows Calculator window first"
    );
    let target = candidates.first().unwrap();
    // SAFETY: GetForegroundWindow has no preconditions.
    let foreground = unsafe { GetForegroundWindow() };
    assert_ne!(
        foreground.0.expose_provenance(),
        usize::try_from(target.window_id).unwrap()
    );
    let mut pointer = POINT::default();
    // SAFETY: pointer is a valid out-pointer for the call.
    unsafe { GetCursorPos(&raw mut pointer) }.unwrap();
    for identifier in [
        "clearButton",
        "num7Button",
        "multiplyButton",
        "num8Button",
        "equalButton",
    ] {
        let observed = elements(&computer, &access, target).await.unwrap();
        let element = observed
            .into_iter()
            .find(|element| element.identifier.as_deref() == Some(identifier))
            .unwrap();
        let action: ComputerAction = serde_json::from_value(serde_json::json!({
            "action": "press", "element": element, "allow_click": false,
        }))
        .unwrap();
        assert_eq!(
            computer.execute(&access, target, &action).await.unwrap(),
            ComputerResult::Pressed {
                method: PressMethod::Action
            }
        );
        // SAFETY: GetForegroundWindow has no preconditions.
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        let mut current_pointer = POINT::default();
        // SAFETY: current_pointer is a valid out-pointer for the call.
        unsafe { GetCursorPos(&raw mut current_pointer) }.unwrap();
        assert_eq!(
            (current_pointer.x, current_pointer.y),
            (pointer.x, pointer.y)
        );
    }
    let mut result = None;
    for _ in 0..20 {
        result = elements(&computer, &access, target)
            .await
            .unwrap()
            .into_iter()
            .find(|element| element.identifier.as_deref() == Some("CalculatorResults"));
        if result.as_ref().and_then(|element| element.name.as_deref()) == Some("Display is 56") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        result.and_then(|element| element.name).as_deref(),
        Some("Display is 56")
    );
    // SAFETY: GetForegroundWindow has no preconditions.
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
}
