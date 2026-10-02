use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use assert_fs::TempDir;
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget, MutationClass,
    ResourceKind,
};
use bootty_mux::{
    provider::MuxBackendRegistry,
    repository::{SpaceMuxOverride, WorkspaceRepository},
    session_lifecycle::TaskLifecycle,
    session_membership::{SessionMembership, WorkspaceSession},
};
use bootty_ui::{AppState, commands::CommandRegistry};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};
use rusqlite::Connection;

#[path = "support/idle_frames.rs"]
mod frames;

struct Fixture {
    directory: TempDir,
    state: AppState,
    target: CommandTarget,
    other_target: CommandTarget,
}

fn saved_tasks(prefix: &str) -> SessionMembership {
    let mut sessions = SessionMembership::default();
    for (suffix, state) in [
        ("active", TaskLifecycle::Active),
        ("settled", TaskLifecycle::Settled),
        ("archived", TaskLifecycle::Archived),
    ] {
        let identity = format!("{prefix}-{suffix}");
        sessions.claim(WorkspaceSession {
            identity: identity.clone(),
            backend_name: identity.clone(),
            display_name: format!("Task {suffix}"),
            explicit: true,
            cwd: "/repo/日本語".to_owned(),
        });
        sessions.set_task_lifecycle(&identity, state);
    }
    sessions
}

#[fixture]
fn fixture() -> anyhow::Result<Fixture> {
    let directory = TempDir::new()?;
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    let primary = snapshot.spaces().first().context("primary Space")?;
    repository.commit_binding_state(primary.id(), &saved_tasks("primary"))?;
    let other = repository
        .create_space(
            "Other",
            "folder",
            [0x7A, 0xA2, 0xF7],
            false,
            SpaceMuxOverride::default(),
            false,
        )?
        .context("created other Space")?;
    repository.commit_binding_state(other.id(), &saved_tasks("other"))?;
    drop(repository);
    let mut state = AppState::new(
        config,
        Arc::new(MuxBackendRegistry::desktop()?),
        Arc::new(|| {}),
        None,
        None,
    )?;
    let listed = submit(&mut state, "spaces.list", &[], None, Caller::Socket)?;
    let CommandOutcome::Success { value, .. } = listed else {
        bail!("Space listing: {listed:?}")
    };
    let target = serde_json::from_value(
        value
            .get(0)
            .and_then(|space| space.get("target"))
            .with_context(|| format!("primary binding target in {value:?}"))?
            .clone(),
    )?;
    let other_target = serde_json::from_value(
        value
            .get(1)
            .and_then(|space| space.get("target"))
            .with_context(|| format!("other binding target in {value:?}"))?
            .clone(),
    )?;
    Ok(Fixture {
        directory,
        state,
        target,
        other_target,
    })
}

fn submit(
    state: &mut AppState,
    command: &str,
    arguments: &[&str],
    target: Option<CommandTarget>,
    caller: Caller,
) -> anyhow::Result<CommandOutcome> {
    let now = Instant::now();
    let mut invocation = CommandInvocation::new(
        command,
        arguments.iter().map(|value| (*value).to_owned()).collect(),
        caller,
    );
    invocation.target = target;
    if command == "session.close" {
        // This helper only closes the exact terminal created in a disposable fixture.
        invocation.confirmation = Some(invocation.confirmation());
    }
    let response = state
        .app_command_sender(caller)
        .submit(
            invocation,
            now.checked_add(Duration::from_secs(5))
                .context("command deadline")?,
            CommandCancellation::new(),
        )
        .map_err(|error| anyhow::anyhow!("submit {command}: {error:?}"))?;
    state.update_frame(frames::idle_frame(now));
    response
        .try_recv()
        .context("synchronous task command completes in this frame")
}

fn list(fixture: &mut Fixture, other: bool) -> anyhow::Result<serde_json::Value> {
    let target = if other {
        &fixture.other_target
    } else {
        &fixture.target
    }
    .clone();
    let outcome = submit(
        &mut fixture.state,
        "session.tasks",
        &[],
        Some(target),
        Caller::Socket,
    )?;
    let CommandOutcome::Success { value, .. } = outcome else {
        bail!("Task listing: {outcome:?}")
    };
    Ok(value)
}

#[rstest]
fn dormant_and_archived_tasks_remain_discoverable_in_saved_order(fixture: anyhow::Result<Fixture>) {
    let mut fixture = fixture.unwrap();
    let tasks = list(&mut fixture, false).unwrap();
    assert_eq!(
        tasks,
        serde_json::json!([
            {"identity":"primary-active", "title":"Task active", "cwd":"/repo/日本語", "lifecycle":"active", "attached_observed":false},
            {"identity":"primary-settled", "title":"Task settled", "cwd":"/repo/日本語", "lifecycle":"settled", "attached_observed":false},
            {"identity":"primary-archived", "title":"Task archived", "cwd":"/repo/日本語", "lifecycle":"archived", "attached_observed":false},
        ])
    );
    assert_eq!(
        fixture.state.mux().all_sessions(),
        Vec::<bootty_mux::snapshot::MuxSession>::new().as_slice()
    );
}

#[rstest]
#[case(Caller::Cli)]
#[case(Caller::Socket)]
#[case(Caller::CommandPalette)]
fn every_caller_uses_the_same_scoped_destination_command(
    fixture: anyhow::Result<Fixture>,
    #[case] caller: Caller,
) {
    let mut fixture = fixture.unwrap();
    let outcome = submit(
        &mut fixture.state,
        "session.task.set",
        &["other-active", "archived"],
        Some(fixture.other_target.clone()),
        caller,
    )
    .unwrap();
    assert!(
        matches!(outcome, CommandOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        list(&mut fixture, true).unwrap()[0]["lifecycle"],
        "archived"
    );
    assert_eq!(list(&mut fixture, false).unwrap()[0]["lifecycle"], "active");
    assert_eq!(
        fixture.state.mux().all_sessions(),
        Vec::<bootty_mux::snapshot::MuxSession>::new().as_slice()
    );
    let (_, reopened) =
        WorkspaceRepository::open(&fixture.directory.path().join("config.toml")).unwrap();
    assert_eq!(
        reopened.spaces()[1]
            .binding()
            .sessions()
            .task_lifecycle("other-active"),
        Some(TaskLifecycle::Archived)
    );
}

#[rstest]
fn stale_binding_and_cross_binding_identity_cannot_change_another_task(
    fixture: anyhow::Result<Fixture>,
) {
    let mut fixture = fixture.unwrap();
    let baseline = list(&mut fixture, false).unwrap();
    let wrong_owner = submit(
        &mut fixture.state,
        "session.task.set",
        &["other-active", "archived"],
        Some(fixture.target.clone()),
        Caller::Socket,
    )
    .unwrap();
    assert!(
        matches!(wrong_owner, CommandOutcome::StaleTarget { .. }),
        "{wrong_owner:?}"
    );
    let mut stale = fixture.target.clone();
    stale.generation = stale.generation.saturating_add(1);
    let wrong_generation = submit(
        &mut fixture.state,
        "session.task.set",
        &["primary-active", "archived"],
        Some(stale),
        Caller::Socket,
    )
    .unwrap();
    assert!(
        matches!(wrong_generation, CommandOutcome::StaleTarget { .. }),
        "{wrong_generation:?}"
    );
    assert_eq!(list(&mut fixture, false).unwrap(), baseline);
    assert_eq!(list(&mut fixture, true).unwrap()[0]["lifecycle"], "active");
}

#[rstest]
fn persistence_failure_preserves_the_live_and_saved_destination(fixture: anyhow::Result<Fixture>) {
    let mut fixture = fixture.unwrap();
    let baseline = list(&mut fixture, false).unwrap();
    let connection =
        Connection::open(fixture.directory.path().join("session-order.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_task_change BEFORE INSERT ON workspace_sessions WHEN NEW.identity = 'primary-active' AND NEW.task_lifecycle = 'archived' BEGIN SELECT RAISE(ABORT, 'forced task failure'); END;").unwrap();
    let outcome = submit(
        &mut fixture.state,
        "session.task.set",
        &["primary-active", "archived"],
        Some(fixture.target.clone()),
        Caller::Socket,
    )
    .unwrap();
    assert!(matches!(outcome, CommandOutcome::Failed { code, .. } if code == "persistence_failed"));
    assert_eq!(list(&mut fixture, false).unwrap(), baseline);
    let (_, reopened) =
        WorkspaceRepository::open(&fixture.directory.path().join("config.toml")).unwrap();
    assert_eq!(
        reopened.spaces()[0]
            .binding()
            .sessions()
            .task_lifecycle("primary-active"),
        Some(TaskLifecycle::Active)
    );
}

#[rstest]
fn command_catalog_enumerates_destinations_and_requires_a_binding_target() {
    let registry = CommandRegistry::core();
    let list = registry.describe("session.tasks").unwrap();
    assert_eq!(list.target, Some(ResourceKind::Binding));
    assert_eq!(list.mutation, MutationClass::Read);
    let set = registry.describe("session.task.set").unwrap();
    assert_eq!(set.target, Some(ResourceKind::Binding));
    assert_eq!(set.mutation, MutationClass::Write);
    assert_eq!(
        set.arguments.arguments[1].choices,
        ["active", "settled", "archived"]
    );
    let invalid = registry.resolve(CommandInvocation::new(
        "session.task.set",
        vec!["task".to_owned(), "complete".to_owned()],
        Caller::Socket,
    ));
    assert!(
        matches!(invalid, Err(CommandOutcome::Failed { code, .. }) if code == "invalid_arguments")
    );
}

#[cfg(unix)]
#[rstest]
fn a_discovered_terminal_can_be_promoted_closed_and_restored_without_restarting_it() {
    let directory = TempDir::new().unwrap();
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    config.session.shell = Some("/bin/cat".to_owned());
    config.session.shell_integration = false;
    let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path).unwrap();
    let terminal = WorkspaceSession {
        identity: "saved-identity".to_owned(),
        backend_name: "work".to_owned(),
        display_name: "Legacy task".to_owned(),
        explicit: true,
        cwd: directory
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    };
    repository
        .commit_binding_state(
            snapshot.spaces()[0].id(),
            &SessionMembership::from_sessions(vec![terminal.clone()]),
        )
        .unwrap();
    drop(repository);
    let backends = Arc::new(MuxBackendRegistry::desktop().unwrap());
    let mut state = AppState::new(
        config.clone(),
        Arc::clone(&backends),
        Arc::new(|| {}),
        None,
        None,
    )
    .unwrap();
    let spaces = submit(&mut state, "spaces.list", &[], None, Caller::Socket).unwrap();
    let CommandOutcome::Success { value: spaces, .. } = spaces else {
        panic!("Space listing: {spaces:?}")
    };
    let binding_target: CommandTarget =
        serde_json::from_value(spaces[0]["target"].clone()).unwrap();
    let terminal_target =
        serde_json::from_value(spaces[0]["sessions"][0]["target"].clone()).unwrap();
    let listed = submit(
        &mut state,
        "session.tasks",
        &[],
        Some(binding_target.clone()),
        Caller::Socket,
    )
    .unwrap();
    let CommandOutcome::Success { value: tasks, .. } = listed else {
        panic!("Task listing: {listed:?}")
    };
    assert_eq!(tasks[0]["identity"], "saved-identity");
    assert!(tasks[0]["lifecycle"].is_null());
    assert_eq!(tasks[0]["attached_observed"], true);
    let identity = tasks[0]["identity"].as_str().unwrap();
    let promoted = submit(
        &mut state,
        "session.task.set",
        &[identity, "archived"],
        Some(binding_target.clone()),
        Caller::Socket,
    )
    .unwrap();
    assert!(
        matches!(promoted, CommandOutcome::Success { .. }),
        "{promoted:?}"
    );
    assert_eq!(
        state.mux().all_sessions().len(),
        1,
        "promotion preserves the live terminal"
    );
    let closed = submit(
        &mut state,
        "session.close",
        &[],
        Some(terminal_target),
        Caller::Socket,
    )
    .unwrap();
    assert!(
        matches!(closed, CommandOutcome::Success { .. }),
        "{closed:?}"
    );
    let listed = submit(
        &mut state,
        "session.tasks",
        &[],
        Some(binding_target.clone()),
        Caller::Socket,
    )
    .unwrap();
    let CommandOutcome::Success { value: tasks, .. } = listed else {
        panic!("Dormant task listing: {listed:?}")
    };
    assert_eq!(tasks[0]["identity"], identity);
    assert_eq!(tasks[0]["lifecycle"], "archived");
    assert_eq!(tasks[0]["attached_observed"], false);
    let restored = submit(
        &mut state,
        "session.task.set",
        &[identity, "active"],
        Some(binding_target),
        Caller::Socket,
    )
    .unwrap();
    assert!(
        matches!(restored, CommandOutcome::Success { .. }),
        "{restored:?}"
    );
    assert_eq!(
        state.mux().all_sessions(),
        Vec::<bootty_mux::snapshot::MuxSession>::new().as_slice()
    );
    drop(state);
    let reopened = AppState::new(config, backends, Arc::new(|| {}), None, None).unwrap();
    assert_eq!(
        reopened.mux().all_sessions(),
        Vec::<bootty_mux::snapshot::MuxSession>::new().as_slice()
    );
    let (_, saved) = WorkspaceRepository::open(&directory.path().join("config.toml")).unwrap();
    assert_eq!(
        saved.spaces()[0].binding().sessions().get(identity),
        Some(&terminal)
    );
    assert_eq!(
        saved.spaces()[0]
            .binding()
            .sessions()
            .task_lifecycle(identity),
        Some(TaskLifecycle::Active)
    );
}
