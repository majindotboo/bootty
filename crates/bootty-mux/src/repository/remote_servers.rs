use super::{SpaceId, WorkspaceRepository, open_db, sqlite_path};
use anyhow::{Result, ensure};
use rusqlite::params;
use std::path::Path;

impl WorkspaceRepository {
    /// Trust-on-first-SSH-bootstrap, committed before any remote launch configuration.
    /// A route change cannot silently repoint an existing Space at another server.
    /// # Errors
    /// Rejects a missing Space, a different server identity, or a failed commit.
    pub fn pin_remote_server(config: &Path, scope: SpaceId, server_id: &str) -> Result<()> {
        ensure!(
            !server_id.is_empty(),
            "remote mux omitted its stable server identity"
        );
        let mut conn = open_db(&sqlite_path(config))?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO workspace_remote_servers (space_id, server_id) VALUES (?1, ?2)",
            params![scope.persistence_value(), server_id],
        )?;
        let expected: String = tx.query_row(
            "SELECT server_id FROM workspace_remote_servers WHERE space_id = ?1",
            [scope.persistence_value()],
            |row| row.get(0),
        )?;
        ensure!(
            expected == server_id,
            "remote mux server identity mismatch: expected {expected}, received {server_id}"
        );
        tx.commit()?;
        Ok(())
    }
}
