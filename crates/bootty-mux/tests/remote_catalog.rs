use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, bail};
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    command::MuxCommand,
    provider::{MuxBackendProvider, MuxBackendRegistry},
    remote_catalog::{Backend, CATALOG_VERSION, Catalog},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, session_matches},
};
use pretty_assertions::assert_eq;
use rusqlite::Connection;

struct MarkerBackend;

impl MuxBackend for MarkerBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot::default())
    }

    fn execute(&mut self, _command: MuxCommand) -> Result<()> {
        Ok(())
    }
}

struct MarkerProvider;

impl MuxBackendProvider for MarkerProvider {
    fn command_dispatch(&self) -> bootty_mux::provider::MuxCommandDispatch {
        bootty_mux::provider::MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Tmux
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(MarkerBackend)
    }
}

#[derive(Default)]
struct ScriptedBackend {
    snapshot: MuxSnapshot,
    execute_calls: usize,
    fail_after_apply: bool,
}

impl MuxBackend for ScriptedBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(self.snapshot.clone())
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.execute_calls = self.execute_calls.saturating_add(1);
        match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => {
                self.snapshot
                    .sessions
                    .push(session(&session_id, &session_id));
            }
            MuxCommand::RenameSession { session_id, name } => {
                let session = self
                    .snapshot
                    .sessions
                    .iter_mut()
                    .find(|session| session_matches(session, &session_id))
                    .context("scripted renamed session")?;
                session.name = name;
            }
            MuxCommand::DitchSession { session_id } => self
                .snapshot
                .sessions
                .retain(|session| !session_matches(session, &session_id)),
            _ => {}
        }
        if self.fail_after_apply {
            bail!("scripted transport disconnected after apply")
        }
        Ok(())
    }
}

fn session(id: &str, name: &str) -> MuxSession {
    MuxSession {
        id: id.to_owned(),
        name: name.to_owned(),
        active: false,
        anchor: MuxPaneAnchor::default(),
        active_window_id: None,
        windows: Vec::new(),
        tag: MuxSessionTag::default(),
    }
}

fn tagged_session(id: &str, name: &str, space_id: &str) -> MuxSession {
    MuxSession {
        tag: MuxSessionTag {
            identity: Some(format!("{id}-identity")),
            space: Some(space_id.to_owned()),
        },
        ..session(id, name)
    }
}

struct StampingBackend {
    sessions: Vec<MuxSession>,
}

impl MuxBackend for StampingBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot {
            sessions: self.sessions.clone(),
            ..MuxSnapshot::default()
        })
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        if let MuxCommand::StampSession { session_id, tag } = command
            && let Some(session) = self
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
        {
            session.tag = tag;
        }
        Ok(())
    }
}

fn open_catalog(path: &Path) -> Result<Catalog> {
    let backends = bootty_mux::provider::MuxBackendRegistry::collect([
        bootty_mux::MuxBackendKind::Rmux,
        bootty_mux::MuxBackendKind::Tmux,
    ])?;
    Catalog::open(path, None, std::sync::Arc::new(backends))
}

#[test]
fn daemon_uses_the_stored_backend_provider_without_desktop_fallback() -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let provider: Arc<dyn MuxBackendProvider> = Arc::new(MarkerProvider);
    let backends = Arc::new(MuxBackendRegistry::from_core_providers(
        [provider],
        [MuxBackendKind::Tmux],
    )?);
    let mut catalog = Catalog::open(&directory.path().join("catalog.sqlite"), None, backends)?;
    let space = catalog.create("Stored tmux", Backend::Tmux)?;

    assert_eq!(
        catalog.snapshot(&space.id, Backend::Tmux)?,
        MuxSnapshot::default()
    );
    Ok(())
}

#[test]
fn catalog_listing_skips_spaces_from_unsupported_backends() -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.path().join("catalog.sqlite");
    let mut catalog = open_catalog(&path)?;
    let supported = catalog.create("Supported", Backend::Rmux)?;
    Connection::open(&path)?.execute(
        "INSERT INTO remote_spaces (id, name, backend, position)
         VALUES ('unsupported', 'Unsupported', 'herdr', 1)",
        [],
    )?;

    assert_eq!(catalog.list()?, vec![supported]);
    Ok(())
}

fn create_space(catalog: &mut Catalog, name: &str) -> Result<String> {
    Ok(catalog.create(name, Backend::Rmux)?.id)
}

fn stored_sessions(path: &Path, space_id: &str) -> rusqlite::Result<Vec<(String, i64)>> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(
        "SELECT session_name, position FROM remote_space_sessions
         WHERE space_id = ?1 ORDER BY position",
    )?;
    statement
        .query_map([space_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

#[test]
fn snapshots_filter_tags_and_commands_cannot_cross_space_boundaries() -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Tagged")?;
    let other_id = create_space(&mut catalog, "Other")?;

    let mut backend = ScriptedBackend {
        snapshot: MuxSnapshot {
            sessions: vec![
                tagged_session("$1", "mine", &space_id),
                tagged_session("$2", "theirs", &other_id),
                session("$3", "unclaimed"),
            ],
            active_session_id: Some("$2".to_owned()),
            ..MuxSnapshot::default()
        },
        execute_calls: 0,
        fail_after_apply: false,
    };

    let snapshot = catalog.snapshot_with_backend(&space_id, Backend::Rmux, &mut backend)?;
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["mine"]
    );
    assert_eq!(
        snapshot.active_session_id, None,
        "a selection pointing outside this Space does not survive the filter"
    );
    let error = catalog
        .execute_with_backend(
            &space_id,
            Backend::Rmux,
            MuxCommand::DitchSession {
                session_id: "$2".to_owned(),
            },
            &mut backend,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("session does not belong to remote Space {space_id}")
    );
    assert_eq!(backend.execute_calls, 0);
    Ok(())
}

#[test]
fn membership_recorded_by_name_is_adopted_once_and_then_forgotten() -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Upgraded")?;
    Connection::open(&path)?.execute(
        "INSERT INTO remote_space_sessions (space_id, session_name, position)
         VALUES (?1, 'recorded', 0)",
        [&space_id],
    )?;

    let mut backend = StampingBackend {
        sessions: vec![session("$1", "recorded"), session("$2", "not-recorded")],
    };
    let snapshot = catalog.snapshot_with_backend(&space_id, Backend::Rmux, &mut backend)?;
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["recorded"]
    );
    assert_eq!(
        stored_sessions(&path, &space_id)?,
        Vec::<(String, i64)>::new(),
        "the name-keyed rows are gone once their sessions carry the tag"
    );
    Ok(())
}

#[test]
fn a_pre_journal_catalog_reopens_without_protocol_or_order_changes() -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let path = directory.path().join("daemon.sqlite");
    let connection = Connection::open(&path)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE daemon_metadata (key TEXT PRIMARY KEY);
         CREATE TABLE remote_spaces (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL UNIQUE,
             backend TEXT NOT NULL,
             position INTEGER NOT NULL
         );
         CREATE TABLE remote_space_sessions (
             space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
             session_name TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY (space_id, session_name)
         );
         INSERT INTO daemon_metadata (key) VALUES ('legacy_catalog_migrated');
         INSERT INTO remote_spaces (id, name, backend, position)
         VALUES ('second', 'Second', 'rmux', 1), ('first', 'First', 'rmux', 0);
         INSERT INTO remote_space_sessions (space_id, session_name, position)
         VALUES ('first', 'one', 0), ('first', 'two', 1);",
    )?;
    drop(connection);

    let catalog = open_catalog(&path)?;
    let spaces = catalog.list()?;

    assert_eq!(
        spaces
            .iter()
            .map(|space| space.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(
        spaces
            .iter()
            .map(|space| space.catalog_version)
            .collect::<Vec<_>>(),
        vec![CATALOG_VERSION; spaces.len()]
    );
    assert_eq!(
        stored_sessions(&path, "first")?,
        vec![("one".to_owned(), 0), ("two".to_owned(), 1)]
    );
    assert_eq!(CATALOG_VERSION, 3);
    Ok(())
}
