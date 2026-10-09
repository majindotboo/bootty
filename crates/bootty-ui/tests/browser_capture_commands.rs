use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_config::config::MultiplexerBackendConfig;
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use bootty_ui::{
    AppEffect, AppState,
    commands::{BrowserAction, CommandCatalog, CommandExecutor, CoreCommandExecutor},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[path = "support/idle_frames.rs"]
mod frames;
#[allow(
    clippy::expect_used,
    reason = "Existing shared backend fixture uses checked setup"
)]
mod support;
#[path = "support/config.rs"]
mod test_config;

proptest! {
    #[test]
    fn snapshot_requires_the_exact_page_document_and_window(page in 1_i64..=i64::MAX, document in "[0-9a-f]{32}") {
        let target = CommandTarget { kind: ResourceKind::ApplicationWindow, handle: "captured-window".into(), generation: 17 };
        let mut invocation = CommandInvocation::new("browser.snapshot", vec![page.to_string(), document.clone()], Caller::Socket);
        invocation.target = Some(target.clone());
        let resolved = CommandCatalog::default().resolve(invocation).map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        prop_assert!(matches!(resolved.executor, CommandExecutor::Core(CoreCommandExecutor::Browser(BrowserAction::Snapshot(Some(actual)), Some(actual_page))) if actual == document && actual_page == u64::try_from(page).unwrap_or_default()));
        prop_assert_eq!(resolved.invocation.target, Some(target));
        prop_assert_eq!(resolved.descriptor.mutation, bootty_control::MutationClass::Read);
        prop_assert!(!resolved.descriptor.palette);
    }

    #[test]
    fn native_input_keeps_the_exact_page_and_window(page in 1_i64..=i64::MAX, text in "[a-zA-Z0-9]{1,100}") {
        let target = CommandTarget { kind: ResourceKind::ApplicationWindow, handle: "captured-window".into(), generation: 17 };
        let action = bootty_browser::BrowserInput::Type { text };
        let encoded = serde_json::to_string(&action).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let mut invocation = CommandInvocation::new("browser.input", vec![page.to_string(), encoded], Caller::Socket);
        invocation.target = Some(target.clone());
        let resolved = CommandCatalog::default().resolve(invocation).map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        prop_assert!(matches!(resolved.executor, CommandExecutor::Core(CoreCommandExecutor::Browser(BrowserAction::Input(actual), Some(actual_page))) if actual == action && actual_page == u64::try_from(page).unwrap_or_default()));
        prop_assert_eq!(resolved.invocation.target, Some(target));
    }

    #[test]
    fn capture_requires_the_requested_positive_page(page in 1_i64..=i64::MAX) {
        let expected_page = u64::try_from(page).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let target = CommandTarget { kind: ResourceKind::ApplicationWindow, handle: "captured-window".into(), generation: 17 };
        let mut invocation = CommandInvocation::new("browser.capture", vec![page.to_string()], Caller::Socket);
        invocation.target = Some(target.clone());
        let resolved = CommandCatalog::default().resolve(invocation)
            .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        prop_assert!(matches!(resolved.executor, CommandExecutor::Core(CoreCommandExecutor::Browser(BrowserAction::Capture, Some(actual))) if actual == expected_page));
        prop_assert_eq!(resolved.invocation.target, Some(target));
        prop_assert_eq!(resolved.descriptor.target, Some(ResourceKind::ApplicationWindow));
        prop_assert!(!resolved.descriptor.palette);
    }
}

#[rstest]
#[case(Vec::new())]
#[case(vec!["0".into(), "1234567890abcdef1234567890abcdef".into()])]
#[case(vec!["1".into(), "foreign-document".into()])]
#[case(vec!["1".into(), "1234567890abcdef1234567890abcdef".into(), "foreign".into()])]
fn snapshot_refuses_implicit_or_foreign_documents(#[case] arguments: Vec<String>) {
    assert!(
        CommandCatalog::default()
            .resolve(CommandInvocation::new(
                "browser.snapshot",
                arguments,
                Caller::Socket
            ))
            .is_err()
    );
}

#[rstest]
#[case(Caller::Internal, true)]
#[case(Caller::Socket, false)]
fn annotation_capture_uses_local_policy_only(#[case] caller: Caller, #[case] allowed: bool) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.computer.enabled = false;
    config.computer.capture_enabled = false;
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).unwrap();
    let now = Instant::now();
    let deadline = now.checked_add(Duration::from_secs(1)).unwrap();
    let discover = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::new(
                "resource.current",
                vec!["application_window".into()],
                caller,
            ),
            deadline,
            CommandCancellation::new(),
        )
        .unwrap();
    state.update_frame(frames::idle_frame(now));
    let CommandOutcome::Success { value, .. } = discover.try_recv().unwrap() else {
        panic!("Exact window discovery failed");
    };
    let mut invocation =
        CommandInvocation::new("browser.capture", vec!["1".into(), "7".into()], caller);
    invocation.target = Some(serde_json::from_value(value["target"].clone()).unwrap());
    let response = state
        .app_command_sender(caller)
        .submit(invocation, deadline, CommandCancellation::new())
        .unwrap();

    state.update_frame(frames::idle_frame(now));
    let outcome = response.try_recv().unwrap();
    if allowed {
        assert!(matches!(
            outcome,
            CommandOutcome::Unsupported { .. } | CommandOutcome::Unavailable { .. }
        ));
    } else {
        assert!(matches!(outcome, CommandOutcome::Denied { .. }));
    }
}

#[rstest]
#[case(Vec::new())]
#[case(vec!["0".into()])]
#[case(vec!["-1".into()])]
#[case(vec!["9223372036854775808".into()])]
#[case(vec![u64::MAX.to_string()])]
#[case(vec!["1".into(), "/arbitrary/output.png".into()])]
fn capture_cannot_choose_an_implicit_page_or_output_path(#[case] arguments: Vec<String>) {
    assert!(matches!(
        CommandCatalog::default().resolve(CommandInvocation::new(
            "browser.capture",
            arguments,
            Caller::Socket
        )),
        Err(CommandOutcome::Failed { .. })
    ));
}

#[rstest]
#[case(false, false)]
#[case(false, true)]
#[case(true, false)]
fn capture_policy_is_checked_before_any_browser_or_capture_effect(
    #[case] enabled: bool,
    #[case] capture_enabled: bool,
    #[values(Caller::Internal, Caller::Cli, Caller::Socket)] caller: Caller,
    #[values("browser.capture", "computer.capture")] command: &str,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let mut config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    config.computer.enabled = enabled;
    config.computer.capture_enabled = capture_enabled;
    let mut state =
        AppState::new(config, support::backends(), Arc::new(|| {}), None, None).unwrap();
    let now = Instant::now();
    let deadline = now.checked_add(Duration::from_secs(1)).unwrap();
    let discover = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::new(
                "resource.current",
                vec!["application_window".into()],
                caller,
            ),
            deadline,
            CommandCancellation::new(),
        )
        .unwrap();
    state.update_frame(frames::idle_frame(now));
    let CommandOutcome::Success { value, .. } = discover.try_recv().unwrap() else {
        panic!("Exact window discovery failed");
    };
    let arguments = if command == "browser.capture" {
        vec!["1".into()]
    } else {
        Vec::new()
    };
    let mut invocation = CommandInvocation::new(command, arguments, caller);
    invocation.target = Some(serde_json::from_value(value["target"].clone()).unwrap());
    let response = state
        .app_command_sender(caller)
        .submit(invocation, deadline, CommandCancellation::new())
        .unwrap();
    let effects = state.update_frame(frames::idle_frame(now));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Browser(_)))
    );
    let outcome = response.try_recv().unwrap();
    let CommandOutcome::Denied { message } = outcome else {
        panic!("Unexpected capture outcome: {outcome:?}");
    };
    assert_eq!(message, "Computer capture is disabled in Bootty settings");
    assert_eq!(state.mux().all_sessions().len(), 0);
}

proptest! {
    #[test]
    fn annotation_capture_requires_positive_private_intent(page in 1_i64..=i64::MAX, intent in 1_i64..=i64::MAX) {
        let mut invocation = CommandInvocation::new("browser.capture",vec![page.to_string(),intent.to_string()],Caller::Socket);
        invocation.target=Some(CommandTarget {kind:ResourceKind::ApplicationWindow,handle:"exact-window".into(),generation:17});
        let resolved=CommandCatalog::default().resolve(invocation).map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let expected_page=u64::try_from(page).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let expected_intent=u64::try_from(intent).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert!(matches!(resolved.executor,CommandExecutor::Core(CoreCommandExecutor::Browser(BrowserAction::CaptureAnnotation(actual),Some(actual_page))) if actual==expected_intent && actual_page==expected_page));
    }
}

#[rstest]
#[case("0")]
#[case("-1")]
#[case("/caller/path.png")]
#[case("9223372036854775808")]
fn annotation_capture_cannot_supply_a_path_or_invalid_intent(#[case] intent: &str) {
    assert!(
        CommandCatalog::default()
            .resolve(CommandInvocation::new(
                "browser.capture",
                vec!["1".into(), intent.into()],
                Caller::Socket
            ))
            .is_err()
    );
}
