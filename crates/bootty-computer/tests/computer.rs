use std::path::PathBuf;

use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerError, ComputerTarget, DisplayBounds,
    HostCaptureRegion, MouseButton,
};
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn target() -> ComputerTarget {
    ComputerTarget {
        window_id: 42,
        process_id: 123,
        bundle_id: "dev.bootty.test-target".into(),
        launch_time: 1_000.0,
        bounds: DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        title: None,
    }
}

proptest! {
    #[test]
    fn disabled_access_never_starts_a_helper(x in any::<f64>(), y in any::<f64>()) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = runtime.block_on(Computer::new(PathBuf::from("/missing/computer-helper")).execute(
            &ComputerAccess::from_user_setting(false), &target(),
            &ComputerAction::Click { x, y, button: MouseButton::Left },
        ));
        prop_assert!(matches!(result, Err(ComputerError::Disabled)));
    }

    #[test]
    fn pointer_actions_outside_target_never_start_helper(x in -1_000_000.0..-0.01_f64, y in -1_000_000.0..1_000_000.0_f64) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = runtime.block_on(Computer::new(PathBuf::from("/missing/computer-helper")).execute(
            &ComputerAccess::from_user_setting(true), &target(), &ComputerAction::Move { x, y },
        ));
        prop_assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
    }
}

#[rstest]
#[case(f64::NAN, 0.0)]
#[case(0.0, f64::INFINITY)]
#[case(800.0, 0.0)]
#[case(0.0, 600.0)]
#[tokio::test]
async fn rejects_invalid_coordinates_before_helper(
    target: ComputerTarget,
    #[case] x: f64,
    #[case] y: f64,
) {
    let result = Computer::new(PathBuf::from("/missing/computer-helper"))
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::Move { x, y },
        )
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
}

#[rstest]
#[case(0, 123, 800.0)]
#[case(42, 0, 800.0)]
#[case(42, 123, 0.0)]
#[case(42, 123, f64::INFINITY)]
#[tokio::test]
async fn rejects_invalid_target_before_helper(
    mut target: ComputerTarget,
    #[case] window_id: u32,
    #[case] process_id: i32,
    #[case] width: f64,
) {
    target.window_id = window_id;
    target.process_id = process_id;
    target.bounds.width = width;
    let result = Computer::new(PathBuf::from("/missing/computer-helper"))
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::Snapshot,
        )
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidTarget)));
}

#[rstest]
#[case(String::new())]
#[case("a".repeat(4097))]
#[tokio::test]
async fn refuses_unbounded_text_before_helper(target: ComputerTarget, #[case] text: String) {
    let result = Computer::new(PathBuf::from("/missing/computer-helper"))
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::TypeText { text },
        )
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
}

proptest! {
    #[test]
    fn frame_local_regions_follow_negative_desktop_origins(
        origin_x in -10_000.0..10_000.0_f64,
        origin_y in -10_000.0..10_000.0_f64,
        local_x in 0_u32..700,
        local_y in 0_u32..500,
    ) {
        let mut target = target();
        target.bounds.x = origin_x;
        target.bounds.y = origin_y;
        let geometry = HostCaptureRegion {
            frame_width: 800.0, frame_height: 600.0,
            rect: DisplayBounds { x: f64::from(local_x), y: f64::from(local_y), width: 100.0, height: 100.0 },
        };
        let region = geometry.resolve(&target)?;
        prop_assert_eq!(region.x.to_bits(), (origin_x + f64::from(local_x)).to_bits());
        prop_assert_eq!(region.y.to_bits(), (origin_y + f64::from(local_y)).to_bits());
        prop_assert_eq!(region.width.to_bits(), 100.0_f64.to_bits());
        prop_assert_eq!(region.height.to_bits(), 100.0_f64.to_bits());
        prop_assert!(target.validate_action(&ComputerAction::SnapshotRegion { rect: region }).is_ok(), "valid contained region must be accepted");
    }
}

#[rstest]
#[case(-0.01, 0.0, 100.0, 100.0)]
#[case(0.0, -0.01, 100.0, 100.0)]
#[case(700.01, 0.0, 100.0, 100.0)]
#[case(0.0, 500.01, 100.0, 100.0)]
#[case(0.0, 0.0, 0.0, 100.0)]
#[case(f64::NAN, 0.0, 100.0, 100.0)]
#[tokio::test]
async fn invalid_capture_regions_never_start_helper(
    target: ComputerTarget,
    #[case] x: f64,
    #[case] y: f64,
    #[case] width: f64,
    #[case] height: f64,
) {
    let action = ComputerAction::SnapshotRegion {
        rect: DisplayBounds {
            x,
            y,
            width,
            height,
        },
    };
    let result = Computer::new(PathBuf::from("/missing/computer-helper"))
        .execute(&ComputerAccess::from_user_setting(true), &target, &action)
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
}

#[rstest]
#[case(800.0, 600.0, true)]
#[case(799.0, 600.0, false)]
#[case(800.0, 601.0, false)]
fn captured_frame_resize_invalidates_region(
    target: ComputerTarget,
    #[case] frame_width: f64,
    #[case] frame_height: f64,
    #[case] accepted: bool,
) {
    let geometry = HostCaptureRegion {
        frame_width,
        frame_height,
        rect: DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        },
    };
    pretty_assertions::assert_eq!(geometry.resolve(&target).is_ok(), accepted);
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{Computer, ComputerAccess, ComputerAction, ComputerTarget, DisplayBounds, target};
    use assert_fs::prelude::*;
    use bootty_computer::{ComputerError, ComputerResult, Permission};
    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;

    struct Helper {
        directory: assert_fs::TempDir,
        computer: Computer,
    }

    impl Helper {
        fn new(status: &Value, response: &Value) -> Result<Self, Box<dyn std::error::Error>> {
            let directory = assert_fs::TempDir::new()?;
            directory
                .child("status.json")
                .write_str(&json!({"value": status}).to_string())?;
            directory
                .child("response.json")
                .write_str(&response.to_string())?;
            let executable = directory.child("computer-helper");
            executable.write_str(
                "#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\nrequest=$(cat)\ncase \"$request\" in\n*'\"method\":\"status\"'*) printf '%s\\n' status >> calls; cat status.json;;\n*) printf '%s\\n' execute >> calls; printf '%s' \"$request\" > request.json; cat response.json;;\nesac\n",
            )?;
            std::fs::set_permissions(executable.path(), std::fs::Permissions::from_mode(0o755))?;
            let computer = Computer::new(executable.path().to_owned());
            Ok(Self {
                directory,
                computer,
            })
        }

        fn calls(&self) -> Result<String, std::io::Error> {
            std::fs::read_to_string(self.directory.child("calls").path())
        }
    }

    fn status(accessibility: &str, recording: &str, secure: bool) -> Value {
        json!({"accessibility": accessibility, "screen_recording": recording, "secure_input": secure})
    }

    #[rstest]
    #[case(
        ComputerAction::Snapshot,
        "granted",
        "not_granted",
        false,
        "permission not granted: ScreenRecording"
    )]
    #[case(ComputerAction::TypeText { text: "hello".into() }, "not_granted", "granted", false, "permission not granted: Accessibility")]
    #[case(
        ComputerAction::Snapshot,
        "granted",
        "granted",
        true,
        "secure input is active; computer use is paused"
    )]
    #[tokio::test]
    async fn denied_or_secure_status_never_executes(
        target: ComputerTarget,
        #[case] action: ComputerAction,
        #[case] accessibility: &str,
        #[case] recording: &str,
        #[case] secure: bool,
        #[case] expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let helper = Helper::new(
            &status(accessibility, recording, secure),
            &json!({"value": {"result": "posted"}}),
        )?;
        let result = helper
            .computer
            .execute(&ComputerAccess::from_user_setting(true), &target, &action)
            .await;
        assert_eq!(
            result.err().map(|error| error.to_string()),
            Some(expected.into())
        );
        assert_eq!(helper.calls()?, "status\n");
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn capture_does_not_require_input_permission(
        target: ComputerTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = json!({"value": {"result": "snapshot", "target": target,
            "pixel_width": 800, "pixel_height": 600, "png_base64": "iVBORw0KGgo="}});
        let helper = Helper::new(&status("not_granted", "granted", false), &response)?;
        let result = helper
            .computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::Snapshot,
            )
            .await?;
        let ComputerResult::Snapshot { .. } = result else {
            return Err("capture returned an input result".into());
        };
        assert_eq!(helper.calls()?, "status\nexecute\n");
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn input_does_not_require_capture_permission(
        target: ComputerTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let helper = Helper::new(
            &status("granted", "not_granted", false),
            &json!({"value": {"result": "posted"}}),
        )?;
        let action = ComputerAction::TypeText {
            text: "🦀".into()
        };
        let result = helper
            .computer
            .execute(&ComputerAccess::from_user_setting(true), &target, &action)
            .await?;
        assert_eq!(result, ComputerResult::Posted);
        let wire: Value = serde_json::from_str(&std::fs::read_to_string(
            helper.directory.child("request.json").path(),
        )?)?;
        assert_eq!(wire.get("target"), Some(&serde_json::to_value(target)?));
        assert_eq!(helper.calls()?, "status\nexecute\n");
        Ok(())
    }

    #[rstest]
    #[case("accessibility_denied", "permission not granted: Accessibility")]
    #[case("target_unavailable", "computer target is unavailable")]
    #[case("stale_target", "computer target changed; select it again")]
    #[case("target_not_focused", "computer target is not the focused window")]
    #[case("secure_input", "secure input is active; computer use is paused")]
    #[tokio::test]
    async fn current_permission_and_target_failures_are_not_retried(
        target: ComputerTarget,
        #[case] code: &str,
        #[case] expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let helper = Helper::new(
            &status("granted", "granted", false),
            &json!({"error": {"code": code, "message": "refused"}}),
        )?;
        let result = helper
            .computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::TypeText {
                    text: "hello".into(),
                },
            )
            .await;
        assert_eq!(
            result.err().map(|error| error.to_string()),
            Some(expected.into())
        );
        assert_eq!(helper.calls()?, "status\nexecute\n");
        Ok(())
    }

    #[rstest]
    #[case(99, 800, "computer target changed; select it again")]
    #[case(
        42,
        1601,
        "computer-control helper failed: screenshot exceeds the image limit"
    )]
    #[tokio::test]
    async fn refuses_wrong_target_or_unbounded_image(
        target: ComputerTarget,
        #[case] window_id: u32,
        #[case] pixel_width: u32,
        #[case] expected: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut captured = target.clone();
        captured.window_id = window_id;
        let helper = Helper::new(
            &status("not_granted", "granted", false),
            &json!({"value": {
                "result": "snapshot", "target": captured, "pixel_width": pixel_width, "pixel_height": 600,
                "png_base64": "iVBORw0KGgo="
            }}),
        )?;
        let result = helper
            .computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::Snapshot,
            )
            .await;
        assert_eq!(
            result.err().map(|error| error.to_string()),
            Some(expected.into())
        );
        Ok(())
    }

    #[rstest]
    #[case(true, true)]
    #[case(false, true)]
    #[case(true, false)]
    #[tokio::test]
    async fn capture_reply_must_report_the_exact_requested_region(
        target: ComputerTarget,
        #[case] includes_region: bool,
        #[case] matches_region: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rect = DisplayBounds {
            x: 20.0,
            y: 40.0,
            width: 100.0,
            height: 80.0,
        };
        let mut reported = rect.clone();
        if !matches_region {
            reported.x += 1.0;
        }
        let helper = Helper::new(
            &status("not_granted", "granted", false),
            &json!({"value": {
                "result": "snapshot", "target": target, "pixel_width": 100, "pixel_height": 80,
                "png_base64": "iVBORw0KGgo=", "region": includes_region.then_some(rect.clone()),
                "requested_region": includes_region.then_some(reported)
            }}),
        )?;
        let result = helper
            .computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::SnapshotRegion { rect: rect.clone() },
            )
            .await;
        assert_eq!(result.is_ok(), includes_region && matches_region);
        if let Ok(ComputerResult::Snapshot { region, .. }) = result {
            assert_eq!(region, Some(rect.clone()));
        }
        let wire: Value = serde_json::from_str(&std::fs::read_to_string(
            helper.directory.child("request.json").path(),
        )?)?;
        assert_eq!(wire.get("action"), Some(&json!("snapshot_region")));
        assert_eq!(wire.get("rect"), Some(&serde_json::to_value(rect)?));
        assert_eq!(helper.calls()?, "status\nexecute\n");
        Ok(())
    }

    #[rstest]
    #[case(1.0, 1.0, 1.0, 100, true)]
    #[case(0.5, 2.0, 0.5, 50, true)]
    #[case(0.5, 0.0, 0.5, 50, false)]
    #[case(0.5, 2.0, 1.0, 50, false)]
    #[tokio::test]
    async fn actual_snapped_capture_maps_pixels_without_changing_intent(
        mut target: ComputerTarget,
        #[case] requested_x: f64,
        #[case] actual_x: f64,
        #[case] reported_request_x: f64,
        #[case] pixel_width: u32,
        #[case] accepted: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        target.bounds.x = -1600.0;
        let requested = DisplayBounds {
            x: target.bounds.x + requested_x,
            y: 40.0,
            width: 103.0,
            height: 80.0,
        };
        let actual = DisplayBounds {
            x: target.bounds.x + actual_x,
            y: 40.0,
            width: 100.0,
            height: 80.0,
        };
        let reported_request = DisplayBounds {
            x: target.bounds.x + reported_request_x,
            ..requested.clone()
        };
        let helper = Helper::new(
            &status("not_granted", "granted", false),
            &json!({"value": {
                "result": "snapshot", "target": target, "pixel_width": pixel_width, "pixel_height": 40,
                "png_base64": "iVBORw0KGgo=", "region": actual, "requested_region": reported_request
            }}),
        )?;
        let result = helper
            .computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::SnapshotRegion {
                    rect: requested.clone(),
                },
            )
            .await;
        assert_eq!(result.is_ok(), accepted);
        if let Ok(ComputerResult::Snapshot {
            region,
            requested_region,
            ..
        }) = result
        {
            assert_eq!(region, Some(actual));
            assert_eq!(requested_region, Some(requested));
        }
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn cropped_capture_cannot_use_accessibility_instead_of_screen_permission(
        target: ComputerTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let helper = Helper::new(
            &status("granted", "not_granted", false),
            &json!({"value": {"result": "posted"}}),
        )?;
        let result = helper
            .computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::SnapshotRegion {
                    rect: DisplayBounds {
                        x: 0.0,
                        y: 0.0,
                        width: 100.0,
                        height: 100.0,
                    },
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(ComputerError::PermissionDenied(Permission::ScreenRecording))
        ));
        assert_eq!(helper.calls()?, "status\n");
        Ok(())
    }

    #[rstest]
    #[tokio::test]
    async fn capture_permission_revoked_after_preflight_is_reported(
        target: ComputerTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let helper = Helper::new(
            &status("not_granted", "granted", false),
            &json!({"error": {
                "code": "screen_recording_denied", "message": "revoked"
            }}),
        )?;
        let result = helper
            .computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::Snapshot,
            )
            .await;
        let Err(ComputerError::PermissionDenied(Permission::ScreenRecording)) = result else {
            return Err("revoked capture permission was not reported".into());
        };
        assert_eq!(helper.calls()?, "status\nexecute\n");
        Ok(())
    }
}
