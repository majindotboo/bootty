#![allow(clippy::redundant_closure_for_method_calls)]

use assert_fs::{TempDir, prelude::*};
use bootty_config::config::{MultiplexerBackendConfig, SshRemoteConfig};
use bootty_mux::{
    controller::SpaceId,
    membership::BackendMembership,
    repository::{
        BindingMembershipMutation, DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SpaceMuxOverride,
        SpaceRemoteOverride, WorkspaceRepository, WorkspaceSnapshot,
    },
    session_membership::{SessionMembership, SessionState, WorkspaceSession},
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};
use rusqlite::Connection;

struct LoadedRepository {
    repository: WorkspaceRepository,
    snapshot: WorkspaceSnapshot,
}

impl LoadedRepository {
    fn open(config_path: &std::path::Path) -> anyhow::Result<Self> {
        let (repository, snapshot) = WorkspaceRepository::open(config_path)?;
        Ok(Self {
            repository,
            snapshot,
        })
    }

    fn spaces(&self) -> &[bootty_mux::repository::WorkspaceSpace] {
        self.snapshot.spaces()
    }

    fn default_space(&self) -> Option<SpaceId> {
        self.spaces().first().map(|space| space.id())
    }

    fn sessions(&self, space: SpaceId) -> Option<SessionMembership> {
        self.spaces()
            .iter()
            .map(|space| space.binding())
            .find(|binding| binding.mux_scope() == space)
            .map(|binding| binding.sessions().clone())
    }
}

fn session(identity: &str, backend_name: &str) -> WorkspaceSession {
    WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: backend_name.to_owned(),
        display_name: String::new(),
        explicit: false,
        cwd: "/worktree".to_owned(),
        state: SessionState::default(),
        terminal_snapshot: None,
    }
}

fn backend_names(sessions: &SessionMembership) -> Vec<String> {
    sessions.backend_names()
}

fn membership(id: &str, name: &str, identity: &str) -> BackendMembership {
    BackendMembership {
        id: id.to_owned(),
        name: name.to_owned(),
        identity: Some(identity.to_owned()),
    }
}

impl std::ops::Deref for LoadedRepository {
    type Target = WorkspaceRepository;

    fn deref(&self) -> &Self::Target {
        &self.repository
    }
}

impl std::ops::DerefMut for LoadedRepository {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.repository
    }
}

#[fixture]
fn repository() -> anyhow::Result<(TempDir, LoadedRepository)> {
    let directory = TempDir::new()?;
    let config_path = directory.path().join("config.toml");
    let repository = LoadedRepository::open(&config_path)?;
    Ok((directory, repository))
}

#[test]
fn an_invalid_database_is_reported_instead_of_becoming_an_empty_workspace() {
    let directory = TempDir::new().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    directory
        .child("session-order.sqlite3")
        .write_str("not a sqlite database")
        .expect("write invalid database");

    assert!(WorkspaceRepository::open(&config_path).is_err());
}

#[rstest]
fn an_invalid_current_snapshot_is_rejected_instead_of_repaired(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, repository) = repository.expect("workspace fixture");
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(database).expect("open workspace database");
    connection
        .execute("UPDATE workspace_spaces SET color = 'invalid'", [])
        .expect("corrupt current color value");
    drop(connection);

    let error = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect_err("invalid current snapshot must fail");
    assert!(error.to_string().contains("load "));
}

#[rstest]
#[case(0)]
#[case(3)]
#[case(4)]
#[case(8)]
fn unsupported_workspace_revisions_fail_without_converting_or_resetting_data(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
    #[case] revision: i64,
) -> anyhow::Result<()> {
    let (directory, repository) = repository?;
    let expected = repository.snapshot.clone();
    drop(repository);
    let config_path = directory.path().join("config.toml");
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database)?;
    connection.pragma_update(None, "user_version", revision)?;

    WorkspaceRepository::open(&config_path).expect_err("unsupported revision must fail");
    let unchanged: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(unchanged, revision);
    connection.pragma_update(None, "user_version", 7)?;
    let (_, reopened) = WorkspaceRepository::open(&config_path)?;
    assert_eq!(reopened, expected);
    Ok(())
}

#[rstest]
fn name_keyed_storage_is_rejected_without_importing_or_deleting_records() -> anyhow::Result<()> {
    let directory = TempDir::new()?;
    let config_path = directory.path().join("config.toml");
    let connection = Connection::open(directory.path().join("session-order.sqlite3"))?;
    connection.execute_batch(
        "CREATE TABLE session_groups (id INTEGER PRIMARY KEY, position INTEGER);
         CREATE TABLE sessions (name TEXT, group_id INTEGER, position INTEGER);
         CREATE TABLE session_name_metadata (session_id TEXT, generated_name TEXT, cwd TEXT, explicit INTEGER);
         INSERT INTO session_groups VALUES (1, 0);
         INSERT INTO sessions VALUES ('work', 1, 0);
         INSERT INTO session_name_metadata VALUES ('work', 'work', '/work', 1);",
    )?;

    WorkspaceRepository::open(&config_path).expect_err("name-keyed storage must fail");
    let row: (String, i64, i64) =
        connection.query_row("SELECT name, group_id, position FROM sessions", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    assert_eq!(row, ("work".to_owned(), 1, 0));
    let tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(tables, 3);
    Ok(())
}

#[rstest]
fn unsupported_backend_spaces_do_not_block_supported_workspace_state(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let supported = repository
        .create_space(
            "Supported",
            "folder",
            [0x7A, 0xA2, 0xF7],
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Rmux),
                remote: SpaceRemoteOverride::Local,
            },
            false,
        )
        .expect("create supported Space")
        .expect("supported Space");
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    Connection::open(database)
        .expect("open workspace database")
        .execute(
            "UPDATE workspace_spaces SET backend = 'future-backend' WHERE name = 'Default Space'",
            [],
        )
        .expect("store unsupported backend");

    let (_, snapshot) = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("supported workspace state remains available");
    assert_eq!(snapshot.spaces(), &[supported]);
}

#[rstest]
fn a_space_update_that_fails_leaves_every_field_as_it_was(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let space = &repository.spaces()[0];
    let space_id = space.id();
    let original_remote_id = space.remote_id().to_owned();
    let original_position = space.position();
    let binding_scope = space.binding().mux_scope();
    let database = directory.path().join("session-order.sqlite3");
    let trigger_connection = Connection::open(&database).expect("open workspace database");
    trigger_connection
        .execute_batch(
            "CREATE TRIGGER fail_space_binding_update
             BEFORE UPDATE OF backend, remote ON workspace_spaces
             BEGIN
                 SELECT RAISE(ABORT, 'forced binding update failure');
             END;",
        )
        .expect("install binding update failure");

    let error = repository
        .update_space(
            binding_scope,
            "Updated Space",
            "star",
            [9, 8, 7],
            true,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Local,
            },
        )
        .expect_err("binding failure must reject the whole space update");
    assert!(error.to_string().contains("forced binding update failure"));

    drop(trigger_connection);
    drop(repository);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml"))
        .expect("workspace repository");
    let stored_space = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == space_id)
        .expect("stored space");
    assert_eq!(stored_space.remote_id(), original_remote_id.as_str());
    assert_eq!(stored_space.name(), "Default Space");
    assert_eq!(stored_space.icon(), DEFAULT_SPACE_ICON);
    assert_eq!(stored_space.color(), DEFAULT_SPACE_COLOR);
    assert!(!stored_space.tint_sidebar());
    assert_eq!(stored_space.position(), original_position);

    let stored_binding = stored_space.binding();
    assert_eq!(stored_binding.mux_scope(), space_id);
    assert_eq!(stored_binding.backend_override(), None);
    assert_eq!(
        stored_binding.remote_override(),
        &SpaceRemoteOverride::Inherit
    );
    assert!(!stored_binding.hide_tmux_status());
}

#[rstest]
fn a_two_space_commit_is_atomic_when_the_second_space_fails(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let first_binding = repository.spaces()[0].binding().clone();
    let second_space = repository
        .create_space(
            "Second",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .expect("create second space")
        .expect("second space");
    let second_binding = second_space.binding().clone();

    let mut first = first_binding.sessions().clone();
    let mut second = second_binding.sessions().clone();
    first.claim(session("id-1", "first-old"));
    second.claim(session("id-2", "second-old"));
    repository
        .commit_binding_states(&[
            (first_binding.mux_scope(), first.clone()),
            (second_binding.mux_scope(), second.clone()),
        ])
        .expect("commit baseline binding states");

    first.claim(session("id-3", "first-new"));
    second.claim(session("id-4", "second-new"));
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_second_binding_session
             BEFORE INSERT ON workspace_sessions
             WHEN NEW.space_id = {} AND NEW.backend_name = 'second-new'
             BEGIN
                 SELECT RAISE(ABORT, 'forced second binding failure');
             END;",
            second_binding.mux_scope().persistence_value()
        ))
        .expect("install second binding failure");
    drop(connection);

    repository
        .commit_binding_states(&[
            (first_binding.mux_scope(), first),
            (second_binding.mux_scope(), second),
        ])
        .expect_err("the second binding failure must roll back the first binding");
    drop(repository);

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"))
        .expect("workspace repository");
    let stored = reopened
        .spaces()
        .iter()
        .map(|space| space.binding())
        .map(|binding| (binding.mux_scope(), backend_names(binding.sessions())))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        stored.get(&first_binding.mux_scope()),
        Some(&vec!["first-old".to_owned()])
    );
    assert_eq!(
        stored.get(&second_binding.mux_scope()),
        Some(&vec!["second-old".to_owned()])
    );
}

#[rstest]
fn a_remote_backend_success_is_recovered_after_its_metadata_commit_fails(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let binding = repository.spaces()[0].binding().clone();
    let scope = binding.mux_scope();
    let mutation = BindingMembershipMutation::Create {
        identity: "id-1".to_owned(),
        session_name: "created-name".to_owned(),
        display_name: "created-name".to_owned(),
        explicit: true,
        cwd: "/worktree".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &mutation)
        .expect("journal remote create before backend execution");

    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute_batch(
            "CREATE TRIGGER fail_remote_metadata_commit
             BEFORE INSERT ON workspace_sessions
             WHEN NEW.backend_name = 'created-name'
             BEGIN
                 SELECT RAISE(ABORT, 'forced remote metadata failure');
             END;",
        )
        .expect("install metadata failure");
    let mut sessions = binding.sessions().clone();
    repository
        .commit_binding_membership_mutation(scope, &mutation, &mut sessions)
        .expect_err("metadata failure must retain the binding operation journal");
    assert!(sessions.is_empty());
    assert_eq!(
        repository
            .pending_binding_membership_mutations(scope)
            .expect("read pending mutations")
            .iter()
            .map(|pending| pending.mutation())
            .collect::<Vec<_>>(),
        [&mutation]
    );

    connection
        .execute("DROP TRIGGER fail_remote_metadata_commit", [])
        .expect("remove metadata failure");
    drop(connection);
    assert!(
        repository
            .reconcile_binding_membership_mutations(
                scope,
                &[membership("$4", "created-name-2", "id-1")],
                &mut sessions,
            )
            .expect("reconcile authoritative backend snapshot"),
    );
    assert_eq!(backend_names(&sessions), vec!["created-name"]);
    assert_eq!(
        repository
            .pending_binding_membership_mutations(scope)
            .expect("read cleared mutations"),
        Vec::<bootty_mux::repository::PendingBindingMembershipMutation>::new()
    );
    drop(repository);

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"))
        .expect("workspace repository");
    assert_eq!(
        backend_names(reopened.spaces()[0].binding().sessions()),
        vec!["created-name"]
    );
}

#[rstest]
fn remote_rename_and_ditch_mutations_commit_binding_membership(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (_directory, mut repository) = repository.expect("workspace fixture");
    let binding = repository.spaces()[0].binding().clone();
    let scope = binding.mux_scope();
    let mut sessions = binding.sessions().clone();
    sessions.claim(session("id-1", "old-name"));
    repository
        .commit_binding_state(scope, &sessions)
        .expect("commit baseline membership");

    let rename = BindingMembershipMutation::Rename {
        identity: "id-1".to_owned(),
        old_name: "old-name".to_owned(),
        new_name: "new-name".to_owned(),
        display_name: "New name".to_owned(),
        explicit: true,
    };
    repository
        .begin_binding_membership_mutation(scope, &rename)
        .expect("journal rename");
    repository
        .commit_binding_membership_mutation(scope, &rename, &mut sessions)
        .expect("commit rename");
    assert_eq!(backend_names(&sessions), vec!["new-name"]);
    assert_eq!(
        sessions.get("id-1").map(|claimed| claimed.label()),
        Some("New name"),
        "the claim keeps its identity across the rename"
    );

    let ditch = BindingMembershipMutation::Ditch {
        identity: "id-1".to_owned(),
        old_name: "new-name".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &ditch)
        .expect("journal ditch");
    repository
        .commit_binding_membership_mutation(scope, &ditch, &mut sessions)
        .expect("commit ditch");
    assert_eq!(
        sessions.get("id-1").map(|saved| saved.label()),
        Some("New name")
    );

    let replacement = BindingMembershipMutation::Create {
        identity: "id-2".to_owned(),
        session_name: "new-name".to_owned(),
        display_name: "new-name".to_owned(),
        explicit: false,
        cwd: "/worktree".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &replacement)
        .expect("journal the replacement create");
    repository
        .commit_binding_membership_mutation(scope, &replacement, &mut sessions)
        .expect("commit the replacement create");

    assert_eq!(
        sessions.get("id-2").map(|claimed| claimed.label()),
        Some("new-name"),
        "the ditched session's display name is not reused"
    );
    assert_eq!(
        sessions.get("id-1").map(|saved| saved.label()),
        Some("New name")
    );
}

#[rstest]
#[case::ditch_with_create_fields(
    "INSERT INTO workspace_pending_binding_operations (space_id, operation, identity, old_name, new_name, cwd) VALUES (?1, 'ditch', 'id-1', 'old-name', 'forbidden', '/forbidden')"
)]
#[case::create_with_invalid_explicit(
    "INSERT INTO workspace_pending_binding_operations (space_id, operation, identity, new_name, display_name, explicit, cwd) VALUES (?1, 'create', 'id-1', 'backend-name', 'display-name', 2, '/worktree')"
)]
fn malformed_pending_operations_are_rejected_on_reopen(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
    #[case] statement: &str,
) {
    let (directory, repository) = repository.expect("workspace fixture");
    let scope = repository.spaces()[0].binding().mux_scope();
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(database).expect("open workspace database");
    connection
        .execute(statement, [scope.persistence_value()])
        .expect("insert invalid pending operation");
    drop(connection);

    assert!(WorkspaceRepository::open(&directory.path().join("config.toml")).is_err());
}

#[rstest]
fn spaces_preserve_identity_appearance_and_remote_placement(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let remote = SshRemoteConfig {
        host: "devbox".to_owned(),
        user: Some("dev".to_owned()),
        port: Some(2222),
        program: "ssh".to_owned(),
        args: vec!["-i".to_owned(), "~/.ssh/devbox".to_owned()],
    };

    let created = repository
        .create_space(
            " Review ",
            "terminal",
            [1, 2, 3],
            true,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Inline(remote.clone().into()),
            },
            false,
        )
        .expect("create space")
        .expect("valid space");
    assert!(
        repository
            .create_space(
                "   ",
                DEFAULT_SPACE_ICON,
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride::default(),
                false,
            )
            .expect("reject blank name")
            .is_none()
    );
    let duplicate = repository
        .create_space(
            "review",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .expect("create duplicate")
        .expect("valid duplicate");
    assert_eq!(duplicate.name(), "review 2");

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"))
        .expect("workspace repository");
    let stored = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == created.id())
        .expect("stored space");
    assert_eq!(stored.name(), "Review");
    assert_eq!(stored.icon(), "terminal");
    assert_eq!(stored.color(), [1, 2, 3]);
    assert!(stored.tint_sidebar());
    assert_eq!(
        stored.binding().remote_override(),
        &SpaceRemoteOverride::Inline(remote.into())
    );
}

#[rstest]
fn session_membership_is_binding_scoped_and_persists(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let first_binding = repository.default_space().expect("default Space");
    let second_space = repository
        .create_space(
            "Second",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .expect("create second space")
        .expect("second space");
    let second_scope = second_space.binding().mux_scope();

    let mut first = repository
        .sessions(first_binding)
        .expect("first binding membership");
    let mut second = second_space.binding().sessions().clone();
    for (identity, name) in [
        ("id-1", "arc/migrations"),
        ("id-2", "arc/readiness"),
        ("id-3", "agents"),
        ("id-4", "bootty"),
    ] {
        first.claim(session(identity, name));
    }
    second.claim(session("id-5", "other"));
    assert!(first.move_before("id-3", Some("id-1")));
    first.set_display_name("id-1", "agents/main", true);

    let first_scope = first_binding;
    repository
        .commit_binding_state(first_scope, &first)
        .expect("commit first binding");
    repository
        .commit_binding_state(second_scope, &second)
        .expect("commit second binding");

    repository = LoadedRepository::open(&directory.path().join("config.toml"))
        .expect("workspace repository");
    let first = repository
        .sessions(first_binding)
        .expect("reopened first binding");
    assert_eq!(
        backend_names(&first),
        vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
    );
    let named = first.get("id-1").expect("named session");
    assert_eq!(
        (named.label(), named.backend_name.as_str(), named.explicit),
        ("agents/main", "arc/migrations", true)
    );
    assert_eq!(
        backend_names(
            &repository
                .sessions(second_scope)
                .expect("reopened second binding")
        ),
        vec!["other"],
        "one Space's sessions never leak into another's"
    );
}

#[rstest]
fn a_failed_binding_commit_keeps_the_committed_snapshot_and_database(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let binding = repository.default_space().expect("default Space");
    let scope = binding;
    let mut committed = repository.sessions(binding).expect("binding membership");
    assert!(committed.claim(session("id-1", "stable")));
    repository
        .commit_binding_state(scope, &committed)
        .expect("commit baseline");

    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");

    let mut candidate = committed.clone();
    assert!(candidate.claim(session("id-2", "uncommitted")));
    let error = repository
        .commit_binding_state(scope, &candidate)
        .expect_err("locked database must reject the candidate");
    assert!(error.to_string().contains("workspace persistence error"));
    lock.execute_batch("ROLLBACK").expect("release write lock");
    drop(lock);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml"))
        .expect("workspace repository");
    assert_eq!(reopened.sessions(binding), Some(committed));
}

#[rstest]
fn a_stranded_mutation_does_not_block_a_change_to_another_session(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (_directory, mut repository) = repository.expect("workspace fixture");
    let scope = repository.spaces()[0].binding().mux_scope();
    let stranded = BindingMembershipMutation::Create {
        identity: "stranded-id".to_owned(),
        session_name: "stranded-name".to_owned(),
        display_name: "stranded-name".to_owned(),
        explicit: true,
        cwd: String::new(),
    };
    repository
        .begin_binding_membership_mutation(scope, &stranded)
        .expect("journal the first mutation");

    let next = BindingMembershipMutation::Create {
        identity: "next-id".to_owned(),
        session_name: "next-name".to_owned(),
        display_name: "next-name".to_owned(),
        explicit: true,
        cwd: String::new(),
    };
    repository
        .begin_binding_membership_mutation(scope, &next)
        .expect("a stranded entry does not block the next change");
    let replacement = BindingMembershipMutation::Create {
        identity: "next-id".to_owned(),
        session_name: "replacement".to_owned(),
        display_name: "replacement".to_owned(),
        explicit: true,
        cwd: String::new(),
    };
    repository
        .begin_binding_membership_mutation(scope, &replacement)
        .unwrap();
    let pending = repository
        .pending_binding_membership_mutations(scope)
        .expect("read the pending mutations");
    assert_eq!(
        pending
            .iter()
            .map(|entry| entry.mutation())
            .collect::<Vec<_>>(),
        [&replacement, &stranded],
    );
}

#[rstest]
fn wsl_space_placement_survives_repository_reopen(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let remote = bootty_config::config::WslRemoteConfig {
        distribution: bootty_config::config::WslDistribution::new("Ubuntu 開発").unwrap(),
    };
    let created = repository
        .create_space(
            "Linux",
            "terminal",
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Rmux),
                remote: SpaceRemoteOverride::Inline(remote.clone().into()),
            },
            false,
        )
        .unwrap()
        .unwrap();
    let reopened = LoadedRepository::open(&directory.path().join("config.toml"))
        .expect("workspace repository");
    let stored = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == created.id())
        .unwrap();
    assert_eq!(
        stored.binding().remote_override(),
        &SpaceRemoteOverride::Inline(remote.into())
    );
}

#[rstest]
fn supported_saved_identity_schema_gains_state_without_losing_session_data(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) -> anyhow::Result<()> {
    let (directory, mut repository) = repository?;
    let scope = repository.default_space().expect("Space");
    let expected = SessionMembership::from_sessions(vec![session("saved", "backend")]);
    repository.commit_binding_state(scope, &expected)?;
    drop(repository);
    let database = Connection::open(directory.path().join("session-order.sqlite3"))?;
    database.execute_batch("ALTER TABLE workspace_sessions DROP COLUMN session_state; ALTER TABLE workspace_sessions DROP COLUMN terminal_snapshot; ALTER TABLE workspace_spaces DROP COLUMN selected_session_identity;")?;
    database.pragma_update(None, "user_version", 5)?;
    let (_, reopened) = WorkspaceRepository::open(&directory.path().join("config.toml"))?;
    assert_eq!(reopened.spaces()[0].binding().sessions(), &expected);
    assert_eq!(
        database.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
        7
    );
    Ok(())
}

#[rstest]
#[case(None, SessionState::default())]
#[case(Some("active"), SessionState::default())]
#[case(Some("settled"), SessionState { lifecycle: bootty_mux::session_membership::SessionLifecycle::Settled, ..SessionState::default() })]
#[case(Some("archived"), SessionState { archived: true, ..SessionState::default() })]
fn prototype_task_lifecycle_preserves_saved_work(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
    #[case] lifecycle: Option<&str>,
    #[case] state: SessionState,
) -> anyhow::Result<()> {
    let (directory, mut repository) = repository?;
    let scope = repository.default_space().expect("Space");
    let mut saved = session("saved", "backend");
    saved.display_name = "Saved purpose".into();
    saved.explicit = true;
    repository.commit_binding_state(
        scope,
        &SessionMembership::from_sessions(vec![saved.clone()]),
    )?;
    drop(repository);
    let database = Connection::open(directory.path().join("session-order.sqlite3"))?;
    database.execute_batch("ALTER TABLE workspace_sessions DROP COLUMN session_state; ALTER TABLE workspace_sessions DROP COLUMN terminal_snapshot; ALTER TABLE workspace_spaces DROP COLUMN selected_session_identity; ALTER TABLE workspace_sessions ADD COLUMN task_lifecycle TEXT;")?;
    database.execute(
        "UPDATE workspace_sessions SET task_lifecycle = ?1",
        [lifecycle],
    )?;
    database.pragma_update(None, "user_version", 6)?;
    let (_, reopened) = WorkspaceRepository::open(&directory.path().join("config.toml"))?;
    saved.state = state;
    assert_eq!(
        reopened.spaces()[0].binding().sessions().get("saved"),
        Some(&saved)
    );
    Ok(())
}

#[rstest]
#[case::unknown_lifecycle("{\"lifecycle\":\"unknown\"}")]
#[case::negative_deadline("{\"snoozed_until\":-1}")]
#[case::negative_activity("{\"last_activity_at\":-1}")]
#[case::unknown_field("{\"unknown\":true}")]
fn malformed_session_state_is_reported_without_rewriting_the_record(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
    #[case] malformed: &str,
) -> anyhow::Result<()> {
    let (directory, mut repository) = repository?;
    let scope = repository.default_space().expect("Space");
    repository.commit_binding_state(
        scope,
        &SessionMembership::from_sessions(vec![session("saved", "backend")]),
    )?;
    let database = Connection::open(directory.path().join("session-order.sqlite3"))?;
    database.execute(
        "UPDATE workspace_sessions SET session_state = ?1",
        [malformed],
    )?;
    WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect_err("unknown state must fail");
    assert_eq!(
        database.query_row("SELECT session_state FROM workspace_sessions", [], |row| {
            row.get::<_, String>(0)
        })?,
        malformed
    );
    Ok(())
}

#[rstest]
fn saved_lifecycle_round_trips_through_membership_recovery(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) -> anyhow::Result<()> {
    use bootty_mux::session_membership::{SessionLifecycle, SessionState};
    let (directory, mut repository) = repository?;
    let scope = repository.default_space().expect("Space");
    let mut saved = session("saved", "backend");
    saved.state = SessionState {
        lifecycle: SessionLifecycle::Settled,
        pinned: true,
        archived: true,
        deleted: true,
        hidden: true,
        snoozed_until: Some(100),
        last_activity_at: Some(90),
    };
    let expected = SessionMembership::from_sessions(vec![saved]);
    repository.commit_binding_state(scope, &expected)?;
    let mutation = BindingMembershipMutation::Rename {
        identity: "saved".to_owned(),
        old_name: "backend".to_owned(),
        new_name: "renamed".to_owned(),
        display_name: "Purpose".to_owned(),
        explicit: true,
    };
    repository.begin_binding_membership_mutation(scope, &mutation)?;
    let mut recovered = expected.clone();
    repository.reconcile_binding_membership_mutations(
        scope,
        &[membership("backend-id", "renamed", "saved")],
        &mut recovered,
    )?;
    let (_, reopened) = WorkspaceRepository::open(&directory.path().join("config.toml"))?;
    let loaded = reopened.spaces()[0].binding().sessions();
    assert_eq!(
        loaded.get("saved").expect("saved").state,
        expected.get("saved").expect("saved").state
    );
    assert_eq!(loaded.get("saved").expect("saved").backend_name, "renamed");
    Ok(())
}

#[rstest]
fn existing_saved_state_defaults_pin_without_losing_its_other_values(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) -> anyhow::Result<()> {
    use bootty_mux::session_membership::SessionLifecycle;
    let (directory, mut repository) = repository?;
    let scope = repository.default_space().expect("Space");
    let mut saved = session("saved", "backend");
    saved.state.lifecycle = SessionLifecycle::Settled;
    saved.state.archived = true;
    saved.state.hidden = true;
    saved.state.snoozed_until = Some(100);
    let expected = SessionMembership::from_sessions(vec![saved]);
    repository.commit_binding_state(scope, &expected)?;
    drop(repository);
    let database = Connection::open(directory.path().join("session-order.sqlite3"))?;
    database.execute(
        "UPDATE workspace_sessions SET session_state = ?1",
        [r#"{"lifecycle":"settled","archived":true,"deleted":false,"hidden":true,"snoozed_until":100}"#],
    )?;
    let (_, reopened) = WorkspaceRepository::open(&directory.path().join("config.toml"))?;
    assert_eq!(reopened.spaces()[0].binding().sessions(), &expected);
    Ok(())
}

#[rstest]
fn registered_empty_projects_and_disclosure_survive_reopening(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, repository) = repository.expect("workspace fixture");
    let scope = repository.default_space().expect("default Space");
    repository
        .register_project(scope, "/empty/project")
        .unwrap();
    repository
        .register_project(scope, "/empty/project")
        .unwrap();
    repository
        .toggle_project_collapsed(scope, "/empty/project")
        .unwrap();
    assert!(repository.sessions(scope).expect("membership").is_empty());
    drop(repository);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml")).unwrap();
    let projects = reopened.registered_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(
        (&projects[0].cwd, projects[0].scope, projects[0].collapsed),
        (&"/empty/project".to_owned(), scope, true)
    );
    reopened
        .toggle_project_collapsed(scope, "/empty/project")
        .unwrap();
    assert!(!reopened.registered_projects().unwrap()[0].collapsed);
}

#[rstest]
#[case("relative/path")]
#[case("/path\nwith-control")]
fn invalid_project_paths_do_not_change_the_registry(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
    #[case] path: &str,
) {
    let (_directory, repository) = repository.expect("workspace fixture");
    let scope = repository.default_space().expect("default Space");
    assert!(repository.register_project(scope, path).is_err());
    assert!(repository.toggle_project_collapsed(scope, path).is_err());
    assert_eq!(repository.registered_projects().unwrap(), Vec::new());
}

#[rstest]
fn collapsing_an_observed_project_registers_it_atomically(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (_directory, repository) = repository.expect("workspace fixture");
    let scope = repository.default_space().expect("default Space");
    repository
        .toggle_project_collapsed(scope, "/observed/project")
        .unwrap();
    assert_eq!(repository.registered_projects().unwrap().len(), 1);
    assert!(repository.registered_projects().unwrap()[0].collapsed);
}

#[rstest]
fn project_customization_survives_restart_and_failed_validation(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, repository) = repository.expect("workspace fixture");
    let scope = repository.default_space().unwrap();
    let settings = bootty_mux::repository::ProjectSettings {
        name: "Bootty".to_owned(),
        icon: "terminal".to_owned(),
        icon_path: Some("/local/project.png".to_owned()),
        provider: "claude".to_owned(),
        isolated: true,
        branch_prefix: "luan/".to_owned(),
        start_ref: "main".to_owned(),
    };
    repository
        .configure_project(scope, "/project", &settings)
        .unwrap();
    repository
        .toggle_project_collapsed(scope, "/project")
        .unwrap();
    let mut invalid = settings.clone();
    invalid.start_ref = "--bad".to_owned();
    assert!(
        repository
            .configure_project(scope, "/project", &invalid)
            .is_err()
    );
    drop(repository);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml")).unwrap();
    let projects = reopened.registered_projects().unwrap();
    assert_eq!(projects[0].settings, settings);
    assert!(projects[0].collapsed);
}
