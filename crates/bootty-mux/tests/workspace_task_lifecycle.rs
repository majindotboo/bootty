use std::{collections::HashSet, sync::Arc};

use assert_fs::TempDir;
use bootty_config::config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig};
use bootty_mux::{
    MuxBackendKind,
    command::MuxCommand,
    controller::SpaceId,
    provider::MuxBackendRegistry,
    repository::{BindingMembershipMutation, WorkspaceRepository},
    session_lifecycle::TaskLifecycle,
    session_membership::{SessionMembership, WorkspaceSession},
    snapshot::MuxSessionTag,
    workspace::WorkspaceRuntime,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::{fixture, rstest};
use rusqlite::Connection;

#[fixture]
fn directory() -> anyhow::Result<TempDir> {
    Ok(TempDir::new()?)
}

fn terminal(identity: &str, name: &str) -> WorkspaceSession {
    WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: name.to_owned(),
        display_name: format!("Task {identity}"),
        explicit: true,
        cwd: "/repo/日本語".to_owned(),
    }
}

fn membership() -> SessionMembership {
    SessionMembership::from_sessions(vec![terminal("task", "work"), terminal("shell", "shell")])
}

#[rstest]
#[case(TaskLifecycle::Active)]
#[case(TaskLifecycle::Settled)]
#[case(TaskLifecycle::Archived)]
fn promoted_tasks_survive_attachment_loss_and_restore_in_place(#[case] state: TaskLifecycle) {
    let mut sessions = membership();
    let task = sessions.get("task").cloned().expect("task membership");
    assert!(sessions.set_task_lifecycle("task", state));
    assert!(!sessions.set_task_lifecycle("task", state));
    assert!(!sessions.set_task_lifecycle("missing", state));
    assert!(!sessions.retain_alive(&HashSet::from(["shell"])));
    assert_eq!(sessions.get("task"), Some(&task));
    assert_eq!(sessions.tasks().collect::<Vec<_>>(), vec![(&task, state)]);

    sessions.claim(terminal("replacement", "work"));
    assert_eq!(sessions.task_lifecycle("replacement"), None);
    assert!(
        sessions.set_task_lifecycle("task", TaskLifecycle::Active)
            || state == TaskLifecycle::Active
    );
    assert_eq!(sessions.sessions().first(), Some(&task));
    assert_eq!(sessions.task_lifecycle("task"), Some(TaskLifecycle::Active));
}

#[rstest]
#[case(TaskLifecycle::Active)]
#[case(TaskLifecycle::Settled)]
#[case(TaskLifecycle::Archived)]
fn task_close_and_reopen_preserve_identity_content_and_destination(
    directory: anyhow::Result<TempDir>,
    #[case] state: TaskLifecycle,
) {
    let directory = directory.expect("temporary workspace");
    let config = directory.path().join("config.toml");
    let (mut repository, snapshot) = WorkspaceRepository::open(&config).unwrap();
    let scope = snapshot.spaces()[0].id();
    let mut sessions = membership();
    sessions.set_task_lifecycle("task", state);
    repository.commit_binding_state(scope, &sessions).unwrap();
    let closed = BindingMembershipMutation::Ditch {
        identity: "task".to_owned(),
        old_name: "work".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &closed)
        .unwrap();
    repository
        .commit_binding_membership_mutation(scope, &closed, &mut sessions)
        .unwrap();
    assert_eq!(sessions, {
        let mut expected = membership();
        expected.set_task_lifecycle("task", state);
        expected
    });
    drop(repository);
    let (_, reopened) = WorkspaceRepository::open(&config).unwrap();
    assert_eq!(reopened.spaces()[0].binding().sessions(), &sessions);
}

#[rstest]
fn revision_five_migrates_without_requiring_a_removed_binding_table(
    directory: anyhow::Result<TempDir>,
) {
    let directory = directory.expect("temporary workspace");
    let config = directory.path().join("config.toml");
    let (mut repository, original) = WorkspaceRepository::open(&config).unwrap();
    let scope = original.spaces()[0].id();
    let sessions = membership();
    repository.commit_binding_state(scope, &sessions).unwrap();
    repository
        .set_binding_restore_state(scope, false, Some("work"), Some("window"))
        .unwrap();
    repository.set_selected_space("window-key", scope).unwrap();
    drop(repository);
    let (_, baseline) = WorkspaceRepository::open(&config).unwrap();
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE workspace_sessions DROP COLUMN task_lifecycle; PRAGMA user_version = 5;",
        )
        .unwrap();
    drop(connection);

    let (_, migrated) = WorkspaceRepository::open(&config).unwrap();
    assert_eq!(migrated, baseline);
    let (_, reopened) = WorkspaceRepository::open(&config).unwrap();
    assert_eq!(reopened, baseline);
    assert_eq!(reopened.spaces()[0].binding().sessions().tasks().count(), 0);
}

#[rstest]
fn failed_migration_keeps_revision_five_rows_and_schema(directory: anyhow::Result<TempDir>) {
    let directory = directory.expect("temporary workspace");
    let config = directory.path().join("config.toml");
    let (mut repository, snapshot) = WorkspaceRepository::open(&config).unwrap();
    let scope = snapshot.spaces()[0].id();
    repository
        .commit_binding_state(scope, &membership())
        .unwrap();
    drop(repository);
    let connection = Connection::open(directory.path().join("session-order.sqlite3")).unwrap();
    connection.execute_batch("ALTER TABLE workspace_sessions DROP COLUMN task_lifecycle; PRAGMA user_version = 5; UPDATE workspace_spaces SET color = 'invalid';").unwrap();
    assert!(WorkspaceRepository::open(&config).is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM workspace_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(connection.query_row("SELECT count(*) FROM pragma_table_info('workspace_sessions') WHERE name = 'task_lifecycle'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[rstest]
fn invalid_task_destination_is_rejected_without_repairing_saved_records(
    directory: anyhow::Result<TempDir>,
) {
    let directory = directory.expect("temporary workspace");
    let config = directory.path().join("config.toml");
    let (mut repository, snapshot) = WorkspaceRepository::open(&config).unwrap();
    repository
        .commit_binding_state(snapshot.spaces()[0].id(), &membership())
        .unwrap();
    drop(repository);
    let connection = Connection::open(directory.path().join("session-order.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE workspace_sessions SET task_lifecycle = 'unknown' WHERE identity = 'task'",
            [],
        )
        .unwrap();
    assert!(WorkspaceRepository::open(&config).is_err());
    assert_eq!(
        connection
            .query_row(
                "SELECT task_lifecycle FROM workspace_sessions WHERE identity = 'task'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "unknown"
    );
}

#[rstest]
fn dormant_task_actions_commit_before_live_publication_and_never_start_a_process(
    directory: anyhow::Result<TempDir>,
) {
    let directory = directory.expect("temporary workspace");
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let (mut repository, snapshot) = WorkspaceRepository::open(&config.config_path).unwrap();
    let scope = snapshot.spaces()[0].id();
    let space_tag = snapshot.spaces()[0].remote_id().to_owned();
    let mut sessions = SessionMembership::from_sessions(vec![terminal("task", "work")]);
    sessions.set_task_lifecycle("task", TaskLifecycle::Active);
    repository.commit_binding_state(scope, &sessions).unwrap();
    drop(repository);
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let mut workspace = WorkspaceRuntime::open(
        &config,
        "main",
        Arc::new(MuxBackendRegistry::collect([MuxBackendKind::Native]).unwrap()),
        AppearanceVariant::Light,
        Arc::clone(&repaint),
    )
    .unwrap();
    assert!(
        workspace
            .binding(scope)
            .unwrap()
            .mux()
            .all_sessions()
            .is_empty(),
        "startup must not recreate a task's missing attachment"
    );
    assert!(
        workspace
            .set_session_lifecycle(
                SpaceId::from_persistence(i64::MAX),
                "task",
                TaskLifecycle::Archived
            )
            .is_err()
    );
    assert!(
        workspace
            .set_session_lifecycle(scope, "missing", TaskLifecycle::Archived)
            .is_err()
    );
    assert!(
        workspace
            .set_session_lifecycle(scope, "task", TaskLifecycle::Settled)
            .unwrap()
    );

    let connection = Connection::open(directory.path().join("session-order.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_archive BEFORE INSERT ON workspace_sessions WHEN NEW.task_lifecycle = 'archived' BEGIN SELECT RAISE(ABORT, 'forced archive failure'); END;").unwrap();
    assert!(
        workspace
            .set_session_lifecycle(scope, "task", TaskLifecycle::Archived)
            .is_err()
    );
    assert_eq!(
        workspace
            .binding(scope)
            .unwrap()
            .sessions()
            .task_lifecycle("task"),
        Some(TaskLifecycle::Settled)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT task_lifecycle FROM workspace_sessions WHERE identity = 'task'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "settled"
    );
    connection
        .execute_batch("DROP TRIGGER reject_archive;")
        .unwrap();
    assert!(
        workspace
            .set_session_lifecycle(scope, "task", TaskLifecycle::Archived)
            .unwrap()
    );
    assert!(
        workspace
            .set_session_lifecycle(scope, "task", TaskLifecycle::Active)
            .unwrap()
    );
    assert_eq!(
        workspace.binding(scope).unwrap().sessions().get("task"),
        sessions.get("task")
    );
    assert!(
        workspace
            .binding(scope)
            .unwrap()
            .mux()
            .all_sessions()
            .is_empty(),
        "archive and restore must not create a backend session"
    );
    let (_, reopened) = WorkspaceRepository::open(&config.config_path).unwrap();
    assert_eq!(
        reopened.spaces()[0].binding().sessions(),
        workspace.binding(scope).unwrap().sessions()
    );

    // Reusing the lost attachment's name does not establish task identity.
    workspace
        .binding_mut(scope)
        .unwrap()
        .mux_mut()
        .execute_command(
            &repaint,
            &config.multiplexer,
            MuxCommand::CreateProjectSession {
                session_id: "work".to_owned(),
                cwd: directory.path().to_string_lossy().into_owned(),
                tag: MuxSessionTag::default(),
                argv: None,
            },
        );
    workspace.reconcile_binding_states(&repaint).unwrap();
    let binding = workspace.binding(scope).unwrap();
    let replacement = binding.mux().backend_session_by_id_or_name("work").unwrap();
    assert_eq!(replacement.tag.identity, None);
    assert!(
        binding.mux().sessions().is_empty(),
        "an untagged replacement must not occupy a task's Space row"
    );
    assert_eq!(
        binding.sessions().task_lifecycle("task"),
        Some(TaskLifecycle::Active)
    );
    assert_eq!(binding.sessions().get("task"), sessions.get("task"));

    workspace
        .binding_mut(scope)
        .unwrap()
        .mux_mut()
        .execute_command(
            &repaint,
            &config.multiplexer,
            MuxCommand::StampSession {
                session_id: "work".to_owned(),
                tag: MuxSessionTag {
                    identity: Some("task".to_owned()),
                    space: Some(space_tag),
                },
            },
        );
    assert!(
        workspace
            .binding(scope)
            .unwrap()
            .task_attachment_observed("task")
    );
    assert!(
        workspace
            .detach_session_from_space(scope, "work", &repaint)
            .unwrap()
    );
    workspace.reconcile_binding_states(&repaint).unwrap();
    let detached = workspace.binding(scope).unwrap();
    assert!(!detached.task_attachment_observed("task"));
    assert_eq!(
        detached.member_sessions(),
        Vec::<&bootty_mux::snapshot::MuxSession>::new()
    );
    assert!(
        detached.mux().sessions().is_empty(),
        "a detached terminal remains outside the task's Space"
    );
    assert_eq!(
        detached.sessions().task_lifecycle("task"),
        Some(TaskLifecycle::Active)
    );
    assert!(
        workspace
            .session_finder_groups()
            .iter()
            .any(|group| group.label == "No space"
                && group.sessions.iter().any(|session| session.name == "work"))
    );
    assert!(
        workspace
            .adopt_session_into_binding(scope, "work", &repaint)
            .unwrap()
    );
    assert!(
        workspace
            .binding(scope)
            .unwrap()
            .task_attachment_observed("task")
    );
    assert_eq!(
        workspace
            .binding(scope)
            .unwrap()
            .sessions()
            .task_lifecycle("task"),
        Some(TaskLifecycle::Active)
    );
}

proptest! {
    #[test]
    fn destination_changes_preserve_saved_content_and_order(seed in any::<SessionSeed>(), destinations in prop::collection::vec(0_u8..3, 0..30)) {
        let mut sessions = membership();
        sessions.set_display_name("task", &seed.display_name, true);
        sessions.set_cwd("task", &seed.cwd);
        let original = sessions.sessions().to_vec();
        for destination in destinations {
            let state = match destination {
                0 => TaskLifecycle::Active,
                1 => TaskLifecycle::Settled,
                _ => TaskLifecycle::Archived,
            };
            sessions.set_task_lifecycle("task", state);
            assert_eq!(sessions.sessions(), original.as_slice());
            assert_eq!(sessions.task_lifecycle("task"), Some(state));
            assert_eq!(sessions.task_lifecycle("shell"), None);
        }
    }
}

#[derive(Arbitrary, Debug)]
struct SessionSeed {
    display_name: String,
    cwd: String,
}
