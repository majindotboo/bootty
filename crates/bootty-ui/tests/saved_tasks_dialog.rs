use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use assert_fs::TempDir;
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, ResourceKind,
};
use bootty_mux::{
    provider::MuxBackendRegistry,
    repository::{SpaceMuxOverride, WorkspaceRepository},
    session_lifecycle::TaskLifecycle,
    session_membership::{SessionMembership, WorkspaceSession},
};
use bootty_ui::{AppEffect, AppState, commands::CommandRegistry};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[path = "support/idle_frames.rs"]
mod frames;

struct Fixture {
    _directory: TempDir,
    state: AppState,
    target: CommandTarget,
}

#[fixture]
fn fixture() -> anyhow::Result<Fixture> {
    let directory = TempDir::new()?;
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let (mut repository, _) = WorkspaceRepository::open(&config.config_path)?;
    let space = repository
        .create_space(
            "Other",
            "folder",
            [0x7A, 0xA2, 0xF7],
            false,
            SpaceMuxOverride::default(),
            false,
        )?
        .context("created other Space")?;
    let mut membership = SessionMembership::from_sessions(vec![WorkspaceSession {
        identity: "saved-task".to_owned(),
        backend_name: "task".to_owned(),
        display_name: "Saved task".to_owned(),
        explicit: true,
        cwd: "/repo/日本語".to_owned(),
    }]);
    membership.set_task_lifecycle("saved-task", TaskLifecycle::Archived);
    repository.commit_binding_state(space.id(), &membership)?;
    drop(repository);
    let mut state = AppState::new(
        config,
        Arc::new(MuxBackendRegistry::desktop()?),
        Arc::new(|| {}),
        None,
        None,
    )?;
    let (listed, _) = invoke(
        &mut state,
        CommandInvocation::new("spaces.list", Vec::new(), Caller::Socket),
    )?;
    let CommandOutcome::Success { value, .. } = listed else {
        anyhow::bail!("Space listing: {listed:?}");
    };
    let target = serde_json::from_value(
        value
            .get(1)
            .and_then(|space| space.get("target"))
            .with_context(|| format!("other binding target in {value:?}"))?
            .clone(),
    )?;
    Ok(Fixture {
        _directory: directory,
        state,
        target,
    })
}

fn invoke(
    state: &mut AppState,
    invocation: CommandInvocation,
) -> anyhow::Result<(CommandOutcome, Vec<AppEffect>)> {
    let now = Instant::now();
    let response = state
        .app_command_sender(invocation.caller)
        .submit(
            invocation,
            now.checked_add(Duration::from_secs(5))
                .context("deadline")?,
            CommandCancellation::new(),
        )
        .map_err(|error| anyhow::anyhow!("submit: {error:?}"))?;
    let effects = state.update_frame(frames::idle_frame(now));
    Ok((
        response
            .try_recv()
            .context("synchronous command response")?,
        effects,
    ))
}

#[rstest]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
#[case(Caller::CommandPalette)]
fn saved_tasks_open_the_exact_space_without_changing_records_or_starting_terminals(
    fixture: anyhow::Result<Fixture>,
    #[case] caller: Caller,
) {
    let mut fixture = fixture.unwrap();
    let mut invocation = CommandInvocation::new("session.tasks.show", Vec::new(), caller);
    invocation.target = Some(fixture.target.clone());
    let (outcome, effects) = invoke(&mut fixture.state, invocation).unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        effects
            .iter()
            .filter_map(|effect| match effect {
                AppEffect::OpenSavedTasks { title, target } => Some((title.as_str(), target)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![("Other", &fixture.target)]
    );
    assert_eq!(
        fixture.state.mux().all_sessions(),
        Vec::<bootty_mux::snapshot::MuxSession>::new().as_slice()
    );
    let mut listing = CommandInvocation::new("session.tasks", Vec::new(), Caller::Socket);
    listing.target = Some(fixture.target);
    let (outcome, _) = invoke(&mut fixture.state, listing).unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Success { ref value, .. }
        if value[0]["identity"] == "saved-task" && value[0]["lifecycle"] == "archived" && value[0]["attached_observed"] == false),
        "{outcome:?}"
    );
}

#[rstest]
fn stale_saved_task_targets_cannot_open_a_dialog_for_another_binding(
    fixture: anyhow::Result<Fixture>,
) {
    let mut fixture = fixture.unwrap();
    let mut stale = fixture.target;
    stale.generation = stale.generation.saturating_add(1);
    let mut invocation = CommandInvocation::new("session.tasks.show", Vec::new(), Caller::Socket);
    invocation.target = Some(stale);
    let (outcome, effects) = invoke(&mut fixture.state, invocation).unwrap();
    assert!(
        matches!(outcome, CommandOutcome::StaleTarget { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, AppEffect::OpenSavedTasks { .. }))
            .collect::<Vec<_>>(),
        Vec::<&AppEffect>::new()
    );
}

#[rstest]
fn saved_tasks_are_reachable_from_the_palette_and_accept_no_extra_arguments() {
    let registry = CommandRegistry::core();
    let descriptor = registry.describe("session.tasks.show").unwrap();
    assert!(descriptor.palette);
    assert_eq!(descriptor.target, Some(ResourceKind::Binding));
    let invalid = registry.resolve(CommandInvocation::new(
        "session.tasks.show",
        vec!["other".to_owned()],
        Caller::Socket,
    ));
    assert!(
        matches!(invalid, Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments")
    );
}
