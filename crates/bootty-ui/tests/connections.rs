use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};
use bootty_mux::provider::MuxBackendRegistry;
use bootty_ui::{AppEffect, AppState, commands::CommandRegistry};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[path = "support/idle_frames.rs"]
mod frames;

#[fixture]
fn state() -> Result<(assert_fs::TempDir, AppState), Box<dyn std::error::Error>> {
    let directory = assert_fs::TempDir::new()?;
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..Default::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
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
    caller: Caller,
) -> Result<(CommandOutcome, Vec<AppEffect>), Box<dyn std::error::Error>> {
    let now = Instant::now();
    let arguments = if command == "connections.enable" {
        vec!["127.0.0.1".into()]
    } else {
        Vec::new()
    };
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
fn remote_and_agent_callers_cannot_change_pairing(
    state: Result<(assert_fs::TempDir, AppState), Box<dyn std::error::Error>>,
    #[case] caller: Caller,
) {
    let result = (|| {
        let (_directory, mut state) = state?;
        let mut outcomes = Vec::new();
        for command in [
            "connections.enable",
            "connections.revoke",
            "connections.copy",
        ] {
            outcomes.push(submit(&mut state, command, caller)?.0);
        }
        Ok::<_, Box<dyn std::error::Error>>(outcomes)
    })();
    assert_eq!(result.as_ref().err().map(ToString::to_string), None);
    let Ok(outcomes) = result else {
        return;
    };
    for outcome in outcomes {
        assert!(
            matches!(outcome, CommandOutcome::Denied { .. }),
            "{outcome:?}"
        );
    }
}

#[rstest]
fn setup_is_a_native_palette_command_on_the_shared_path(
    state: Result<(assert_fs::TempDir, AppState), Box<dyn std::error::Error>>,
) {
    let result = (|| {
        let (_directory, mut state) = state?;
        submit(&mut state, "connections.setup", Caller::CommandPalette)
    })();
    assert_eq!(result.as_ref().err().map(ToString::to_string), None);
    let Ok((outcome, effects)) = result else {
        return;
    };
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(effects.contains(&AppEffect::OpenConnections));
    assert!(
        CommandRegistry::core()
            .palette_commands()
            .any(|command| command.action() == "connections.setup")
    );
}
