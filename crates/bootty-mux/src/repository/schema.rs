use super::{Connection, HashSet, Transaction, WORKSPACE_SNAPSHOT_REVISION};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceSchemaKind {
    Fresh,
    Current,
}

pub(super) fn classify_schema(
    conn: &Connection,
    revision: i64,
) -> rusqlite::Result<WorkspaceSchemaKind> {
    let tables = user_tables(conn)?;
    if revision == 0 && tables.is_empty() {
        return Ok(WorkspaceSchemaKind::Fresh);
    }
    if revision != WORKSPACE_SNAPSHOT_REVISION {
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
                "remote",
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
    if table_has_columns(
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
        "CREATE TABLE IF NOT EXISTS workspace_remote_servers (
            space_id INTEGER PRIMARY KEY REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            server_id TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS workspace_spaces (
            id INTEGER PRIMARY KEY,
            remote_id TEXT UNIQUE,
            name TEXT NOT NULL,
            icon TEXT NOT NULL DEFAULT 'folder',
            color TEXT NOT NULL DEFAULT '#7AA2F7',
            tint_sidebar INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL UNIQUE,
            remote TEXT,
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
            cwd TEXT,
            initial_state TEXT NOT NULL DEFAULT '{}'
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
