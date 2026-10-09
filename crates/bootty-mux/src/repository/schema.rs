#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceSchemaKind {
    Fresh,
    Current,
    SavedIdentities,
    Lifecycle,
    TaskLifecycle,
}

pub(super) fn classify_schema(
    conn: &Connection,
    revision: i64,
) -> rusqlite::Result<WorkspaceSchemaKind> {
    let tables = user_tables(conn)?;
    if revision == 0 && tables.is_empty() {
        return Ok(WorkspaceSchemaKind::Fresh);
    }
    if revision != WORKSPACE_SNAPSHOT_REVISION && revision != 6 && revision != 5 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    for (table, columns) in [
        (
            "workspace_spaces",
            [
                "id",
                "remote_id",
                "name",
                "icon",
                "color",
                "tint_sidebar",
                "position",
                "backend",
                "remote",
                "hide_tmux_status",
                "unavailable",
                "selected_session_id",
                "selected_window_id",
            ]
            .as_slice(),
        ),
        (
            "workspace_sessions",
            [
                "identity",
                "space_id",
                "backend_name",
                "display_name",
                "explicit",
                "cwd",
                "position",
            ]
            .as_slice(),
        ),
        (
            "workspace_window_state",
            ["window_key", "selected_space_id"].as_slice(),
        ),
        (
            "workspace_pending_binding_operations",
            [
                "identity",
                "space_id",
                "operation",
                "old_name",
                "new_name",
                "display_name",
                "explicit",
                "cwd",
            ]
            .as_slice(),
        ),
    ] {
        if !tables.contains(table) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if !table_has_columns(conn, table, columns)? {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    if revision == 5 {
        // Only the supported saved-identity schema gains lifecycle fields. Legacy membership
        // formats remain unsupported; validation above must succeed before any write.
        if table_columns(conn, "workspace_sessions")?.contains("session_state") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(WorkspaceSchemaKind::SavedIdentities)
    } else if revision == 6
        && table_has_columns(conn, "workspace_sessions", &["task_lifecycle"])?
        && !table_columns(conn, "workspace_sessions")?.contains("session_state")
    {
        Ok(WorkspaceSchemaKind::TaskLifecycle)
    } else if revision == 6 && table_has_columns(conn, "workspace_sessions", &["session_state"])? {
        if table_columns(conn, "workspace_sessions")?.contains("terminal_snapshot") {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(WorkspaceSchemaKind::Lifecycle)
    } else if table_has_columns(
        conn,
        "workspace_sessions",
        &["session_state", "terminal_snapshot"],
    )? && table_has_columns(conn, "workspace_spaces", &["selected_session_identity"])?
    {
        Ok(WorkspaceSchemaKind::Current)
    } else {
        Err(rusqlite::Error::InvalidQuery)
    }
}

pub(super) fn migrate_task_lifecycle(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let invalid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_sessions WHERE task_lifecycle NOT IN ('active', 'settled', 'archived'))",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(rusqlite::Error::InvalidQuery);
    }
    tx.execute_batch(
        "ALTER TABLE workspace_sessions ADD COLUMN session_state TEXT NOT NULL DEFAULT '{}';
         UPDATE workspace_sessions SET session_state = CASE task_lifecycle
             WHEN 'settled' THEN '{\"lifecycle\":\"settled\"}'
             WHEN 'archived' THEN '{\"archived\":true}'
             ELSE '{}' END;",
    )
}

pub(super) fn user_tables(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect()
}

pub(super) fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect()
}

fn table_has_columns(conn: &Connection, table: &str, required: &[&str]) -> rusqlite::Result<bool> {
    let columns = table_columns(conn, table)?;
    Ok(required.iter().all(|column| columns.contains(*column)))
}

pub(super) fn create_workspace_schema(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_spaces (
            id INTEGER PRIMARY KEY,
            remote_id TEXT UNIQUE,
            name TEXT NOT NULL,
            icon TEXT NOT NULL DEFAULT 'folder',
            color TEXT NOT NULL DEFAULT '#7AA2F7',
            tint_sidebar INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL UNIQUE,
            backend TEXT NOT NULL DEFAULT 'inherit',
            remote TEXT,
            hide_tmux_status INTEGER NOT NULL DEFAULT 0,
            unavailable INTEGER NOT NULL DEFAULT 0,
            selected_session_id TEXT,
            selected_session_identity TEXT,
            selected_window_id TEXT
        );
        CREATE TABLE IF NOT EXISTS workspace_projects (
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            cwd TEXT NOT NULL,
            collapsed INTEGER NOT NULL DEFAULT 0 CHECK(collapsed IN (0, 1)),
            settings TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY(space_id, cwd)
        );
        CREATE TABLE IF NOT EXISTS workspace_sessions (
            identity TEXT PRIMARY KEY,
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            backend_name TEXT NOT NULL,
            display_name TEXT NOT NULL DEFAULT '',
            explicit INTEGER NOT NULL DEFAULT 0,
            cwd TEXT NOT NULL DEFAULT '',
            session_state TEXT NOT NULL DEFAULT '{}',
            terminal_snapshot TEXT,
            position INTEGER NOT NULL,
            UNIQUE(space_id, position)
        );
        CREATE TABLE IF NOT EXISTS workspace_window_state (
            window_key TEXT PRIMARY KEY,
            selected_space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS workspace_pending_binding_operations (
            identity TEXT PRIMARY KEY,
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            operation TEXT NOT NULL,
            old_name TEXT,
            new_name TEXT,
            display_name TEXT,
            explicit INTEGER,
            cwd TEXT
        );",
    )
}

pub(super) fn new_remote_space_id(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    tx.query_row(
        "SELECT lower(hex(randomblob(4))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(6)))",
        [],
        |row| row.get(0),
    )
}
