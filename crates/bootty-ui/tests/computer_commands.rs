use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use assert_fs::prelude::*;
use bootty_config::config::MultiplexerBackendConfig;
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use bootty_mux::provider::MuxBackendRegistry;
use bootty_ui::{AppEffect, AppState, commands::CommandRegistry};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[path = "support/idle_frames.rs"]
mod frames;
#[path = "support/config.rs"]
mod test_config;

#[fixture]
fn isolated_state() -> Result<(assert_fs::TempDir, AppState), Box<dyn std::error::Error>> {
    let directory = assert_fs::TempDir::new()?;
    directory
        .child("config.toml")
        .write_str("computer-use = false\n[multiplexer]\nbackend = 'native'\n")?;
    let config = test_config::config(
        directory.path().join("config.toml"),
        MultiplexerBackendConfig::Native,
    );
    let state = AppState::new(
        config,
        Arc::new(MuxBackendRegistry::desktop()?),
        Arc::new(|| {}),
        None,
        None,
    )?;
    Ok((directory, state))
}

fn submit(
    state: &mut AppState,
    command: &str,
    arguments: Vec<String>,
    caller: Caller,
) -> Result<(CommandOutcome, Vec<AppEffect>), Box<dyn std::error::Error>> {
    let now = Instant::now();
    // A wire caller cannot acquire host authority by forging the invocation's caller field.
    let receiver = state
        .app_command_sender(caller)
        .submit(
            CommandInvocation::new(command, arguments, Caller::CommandPalette),
            now.checked_add(Duration::from_secs(5))
                .ok_or("deadline overflow")?,
            CommandCancellation::new(),
        )
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let effects = state.update_frame(frames::idle_frame(now));
    Ok((receiver.try_recv()?, effects))
}

#[rstest]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
#[case(Caller::Internal)]
#[case(Caller::Luau)]
fn wire_and_agent_callers_cannot_enable_or_prompt(
    isolated_state: Result<(assert_fs::TempDir, AppState), Box<dyn std::error::Error>>,
    #[case] caller: Caller,
) {
    let result = (|| {
        let (directory, mut state) = isolated_state?;
        let mut outcomes = Vec::new();
        for (command, argument) in [
            ("computer.enable", "true"),
            ("computer.permission.request", "accessibility"),
        ] {
            outcomes.push(submit(&mut state, command, vec![argument.into()], caller)?.0);
        }
        Ok::<_, Box<dyn std::error::Error>>((directory, state, outcomes))
    })();
    assert_eq!(result.as_ref().err().map(ToString::to_string), None);
    let Ok((directory, state, outcomes)) = result else {
        return;
    };
    for outcome in outcomes {
        assert!(
            matches!(outcome, CommandOutcome::Denied { .. }),
            "{outcome:?}"
        );
        assert!(!state.config().computer_use);
    }
    directory
        .child("config.toml")
        .assert("computer-use = false\n[multiplexer]\nbackend = 'native'\n");
}

#[rstest]
fn user_enable_persists_before_live_publication_and_setup_uses_shared_path(
    isolated_state: Result<(assert_fs::TempDir, AppState), Box<dyn std::error::Error>>,
) {
    let result = (|| {
        let (directory, mut state) = isolated_state?;
        let (enabled, _) = submit(
            &mut state,
            "computer.enable",
            vec!["true".into()],
            Caller::CommandPalette,
        )?;
        let document: toml_edit::DocumentMut =
            std::fs::read_to_string(directory.path().join("config.toml"))?.parse()?;
        let (setup, effects) = submit(
            &mut state,
            "computer.setup",
            Vec::new(),
            Caller::CommandPalette,
        )?;
        Ok::<_, Box<dyn std::error::Error>>((state, enabled, document, setup, effects))
    })();
    assert_eq!(result.as_ref().err().map(ToString::to_string), None);
    let Ok((state, outcome, document, setup, effects)) = result else {
        return;
    };
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(state.config().computer_use);
    assert_eq!(
        document
            .get("computer-use")
            .and_then(toml_edit::Item::as_bool),
        Some(true)
    );
    assert!(matches!(setup, CommandOutcome::Success { .. }), "{setup:?}");
    assert!(effects.contains(&AppEffect::OpenComputerSetup));
    assert!(
        CommandRegistry::core()
            .palette_commands()
            .any(|command| command.action() == "computer.setup")
    );
}
