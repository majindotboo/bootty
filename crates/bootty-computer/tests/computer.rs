use std::path::PathBuf;

use bootty_computer::{Computer, ComputerAccess, ComputerAction, ComputerError, MouseButton};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

proptest! {
    #[test]
    fn keyboard_shortcuts_accept_letters_digits_and_function_keys(
        key in prop_oneof![
            proptest::char::range('a', 'z').prop_map(|key| key.to_string()),
            (0..10_u8).prop_map(|key| key.to_string()),
            (1..21_u8).prop_map(|key| format!("f{key}")),
        ]
    ) {
        let wire = serde_json::json!({"action": "key", "key": key, "modifiers": ["command"]});
        let action = serde_json::from_value::<ComputerAction>(wire.clone())?;
        prop_assert_eq!(serde_json::to_value(action)?, wire);
    }

    #[test]
    fn disabled_access_never_starts_a_helper(x in any::<f64>(), y in any::<f64>()) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let computer = Computer::new(PathBuf::from("/missing/bootty-computer-helper"));
        let result = runtime.block_on(computer.execute(
            &ComputerAccess::from_user_setting(false),
            &ComputerAction::Click { x, y, button: MouseButton::Left },
        ));
        prop_assert!(matches!(result, Err(ComputerError::Disabled)));
    }
}

#[rstest]
#[case(f64::NAN, 0.0)]
#[case(0.0, f64::INFINITY)]
#[case(f64::NEG_INFINITY, 0.0)]
#[tokio::test]
async fn rejects_invalid_coordinates_before_starting_helper(#[case] x: f64, #[case] y: f64) {
    let result = Computer::new(PathBuf::from("/missing/bootty-computer-helper"))
        .execute(
            &ComputerAccess::from_user_setting(true),
            &ComputerAction::Move { x, y },
        )
        .await;
    assert_eq!(
        result.err().map(|error| error.to_string()),
        Some("invalid computer action: coordinates must be finite".into())
    );
}

#[rstest]
#[case(String::new())]
#[case("a".repeat(4_097))]
#[tokio::test]
async fn rejects_invalid_text_before_starting_helper(#[case] text: String) {
    let result = Computer::new(PathBuf::from("/missing/bootty-computer-helper"))
        .execute(
            &ComputerAccess::from_user_setting(true),
            &ComputerAction::TypeText { text },
        )
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{Computer, ComputerAccess, ComputerAction};
    use assert_fs::prelude::*;
    use bootty_computer::{ComputerResult, Permission};
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use std::os::unix::fs::PermissionsExt;

    fn helper(response: &str) -> Result<assert_fs::TempDir, Box<dyn std::error::Error>> {
        let directory = assert_fs::TempDir::new()?;
        let file = directory.child("computer-helper");
        // Exercise the executable protocol boundary through the public API.
        let escaped = response.replace('\'', "'\\''");
        file.write_str(&format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{escaped}'\n"
        ))?;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o755))?;
        Ok(directory)
    }

    #[rstest]
    #[case("accessibility_denied", "permission not granted: Accessibility")]
    #[case("screen_recording_denied", "permission not granted: ScreenRecording")]
    #[case("secure_input", "secure input is active; computer use is paused")]
    #[tokio::test]
    async fn preserves_platform_failure_boundaries(
        #[case] code: &str,
        #[case] expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = helper(&format!(
            r#"{{"error":{{"code":"{code}","message":"denied"}}}}"#
        ))?;
        let computer = Computer::new(directory.path().join("computer-helper"));
        let result = computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &ComputerAction::Snapshot { display_id: None },
            )
            .await;
        assert_eq!(
            result.err().map(|error| error.to_string()),
            Some(expected.into())
        );
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn reports_posted_input_and_reads_status_without_enabling()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = helper(r#"{"value":{"result":"posted"}}"#)?;
        let computer = Computer::new(directory.path().join("computer-helper"));
        let result = computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &ComputerAction::TypeText {
                    text: "🦀".into()
                },
            )
            .await?;
        assert_eq!(result, ComputerResult::Posted);

        let directory = helper(
            r#"{"value":{"accessibility":"not_granted","screen_recording":"granted","secure_input":false}}"#,
        )?;
        let computer = Computer::new(directory.path().join("computer-helper"));
        assert_eq!(
            computer.status().await?.accessibility,
            bootty_computer::PermissionStatus::NotGranted
        );
        assert_eq!(
            computer
                .request_permission(Permission::ScreenRecording)
                .await?
                .screen_recording,
            bootty_computer::PermissionStatus::Granted
        );
        Ok(())
    }
}
