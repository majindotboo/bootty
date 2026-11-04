use assert_fs::{TempDir, prelude::*};
use bootty_config::config::SshRemoteConfig;
use bootty_mux::{
    controller::SpaceId,
    repository::{
        DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceRepository, WorkspaceSnapshot,
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
        self.spaces()
            .first()
            .map(bootty_mux::repository::WorkspaceSpace::id)
    }

    fn sessions(&self, space: SpaceId) -> Option<SessionMembership> {
        self.spaces()
            .iter()
            .map(bootty_mux::repository::WorkspaceSpace::binding)
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

/// Spaces saved on the retired external backends keep loading, now on the native backend.
#[rstest]
fn spaces_saved_on_retired_backends_load_as_native(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
    #[values("native", "rmux", "tmux", "herdr", "future-backend")] stored: &str,
) {
    let (directory, repository) = repository.expect("workspace fixture");
    drop(repository);
    Connection::open(directory.path().join("session-order.sqlite3"))
        .expect("open workspace database")
        .execute(
            "UPDATE workspace_spaces SET backend = ?1 WHERE name = 'Default Space'",
            [stored],
        )
        .expect("store retired backend");

    let (_, snapshot) = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("workspace with a retired backend loads");
    assert_eq!(snapshot.spaces().len(), 1);
    assert_eq!(snapshot.spaces()[0].name(), "Default Space");
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
            &SpaceMuxOverride {
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
    assert_eq!(
        stored_binding.remote_override(),
        &SpaceRemoteOverride::Inherit
    );
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
        .map(bootty_mux::repository::WorkspaceSpace::binding)
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
                remote: SpaceRemoteOverride::Inline(remote.clone().into()),
            },
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
                remote: SpaceRemoteOverride::Inline(remote.clone().into()),
            },
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
#[case(Some("settled"), SessionState::default())]
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

/// Older saved state loads with pin off, and a session saved hidden comes back archived.
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
    assert!(repository.remove_project(scope, path).is_err());
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
        branch_prefix: "team/".to_owned(),
        start_ref: "main".to_owned(),
        scripts: vec![bootty_mux::repository::ProjectScript {
            name: "Install".to_owned(),
            command: "pnpm install\npnpm build".to_owned(),
            setup: true,
        }],
        teardown: "docker compose down".to_owned(),
    };
    repository
        .configure_project(scope, "/project", &settings)
        .unwrap();
    repository
        .toggle_project_collapsed(scope, "/project")
        .unwrap();
    let invalid = [
        bootty_mux::repository::ProjectSettings {
            start_ref: "--bad".to_owned(),
            ..settings.clone()
        },
        // Two scripts can't share a name, and a script needs a command.
        bootty_mux::repository::ProjectSettings {
            scripts: vec![settings.scripts[0].clone(), settings.scripts[0].clone()],
            ..settings.clone()
        },
        bootty_mux::repository::ProjectSettings {
            scripts: vec![bootty_mux::repository::ProjectScript {
                name: "Empty".to_owned(),
                ..bootty_mux::repository::ProjectScript::default()
            }],
            ..settings.clone()
        },
    ];
    for invalid in invalid {
        assert!(
            repository
                .configure_project(scope, "/project", &invalid)
                .is_err()
        );
    }
    drop(repository);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml")).unwrap();
    let projects = reopened.registered_projects().unwrap();
    assert_eq!(projects[0].settings, settings);
    assert!(projects[0].collapsed);
}

#[rstest]
fn removing_a_project_preserves_files_and_other_space_registration(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
) {
    let (directory, mut repository) = repository.expect("workspace fixture");
    let scope = repository.default_space().unwrap();
    let other = repository
        .create_space(
            "Other",
            "folder",
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
        )
        .unwrap()
        .unwrap();
    let project = directory.child("project");
    project.create_dir_all().unwrap();
    let file = project.child("keep.txt");
    file.write_str("keep this project").unwrap();
    let cwd = project.path().to_str().unwrap();
    repository.register_project(scope, cwd).unwrap();
    repository.register_project(other.id(), cwd).unwrap();
    let sessions = repository.sessions(scope).unwrap();
    repository.remove_project(scope, cwd).unwrap();
    repository.remove_project(scope, cwd).unwrap();
    assert_eq!(repository.sessions(scope).unwrap(), sessions);
    file.assert("keep this project");
    drop(repository);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml")).unwrap();
    let projects = reopened.registered_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].scope, other.id());
    assert_eq!(projects[0].cwd, cwd);
}

#[rstest]
fn remote_server_pin_survives_reopen_and_refuses_another_host() {
    let directory = TempDir::new().unwrap();
    let config = directory.path().join("config.toml");
    let (_repository, snapshot) = WorkspaceRepository::open(&config).unwrap();
    let scope = snapshot.spaces().first().unwrap().id();
    WorkspaceRepository::pin_remote_server(&config, scope, "server-a").unwrap();
    WorkspaceRepository::open(&config).unwrap();
    WorkspaceRepository::pin_remote_server(&config, scope, "server-a").unwrap();
    assert!(
        WorkspaceRepository::pin_remote_server(&config, scope, "server-b")
            .unwrap_err()
            .to_string()
            .contains("identity mismatch")
    );
    WorkspaceRepository::pin_remote_server(&config, scope, "server-a").unwrap();
}
#[rstest]
fn native_server_identity_is_atomic_stable_and_never_replaces_corruption() {
    let directory = TempDir::new().unwrap();
    let config = directory.path().join("config.toml");
    let id = bootty_mux::native::identity::load_server_id(&config).unwrap();
    assert_eq!(id.len(), 32);
    assert_eq!(
        bootty_mux::native::identity::load_server_id(&config).unwrap(),
        id
    );
    std::fs::write(directory.path().join("mux-server-id"), "").unwrap();
    assert!(bootty_mux::native::identity::load_server_id(&config).is_err());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("mux-server-id")).unwrap(),
        ""
    );
}

#[test]
fn legacy_session_state_ignores_retired_fields() {
    let state: SessionState = serde_json::from_value(serde_json::json!({
        "lifecycle": "settled",
        "snoozed_until": 4_102_444_800_i64,
        "auto_settle_disabled": true
    }))
    .unwrap();
    assert_eq!(state, SessionState::default());
}

#[rstest]
#[case("archived")]
#[case("deleted")]
#[case("hidden")]
fn retired_saved_sessions_are_dropped(
    repository: anyhow::Result<(TempDir, LoadedRepository)>,
    #[case] flag: &str,
) -> anyhow::Result<()> {
    let (directory, mut repository) = repository?;
    let scope = repository.default_space().unwrap();
    repository.commit_binding_state(
        scope,
        &SessionMembership::from_sessions(vec![session("old", "old"), session("live", "live")]),
    )?;
    drop(repository);
    let database = Connection::open(directory.path().join("session-order.sqlite3"))?;
    database.execute(
        "UPDATE workspace_sessions SET session_state = ?1 WHERE identity = 'old'",
        [format!("{{\"{flag}\":true,\"last_activity_at\":123}}")],
    )?;
    let (_, reopened) = WorkspaceRepository::open(&directory.path().join("config.toml"))?;
    let sessions = reopened.spaces()[0].binding().sessions();
    assert_eq!(
        sessions
            .sessions()
            .iter()
            .map(|session| session.identity.as_str())
            .collect::<Vec<_>>(),
        vec!["live"]
    );
    assert_eq!(
        database.query_row(
            "SELECT count(*) FROM workspace_sessions WHERE identity = 'old'",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );
    Ok(())
}
