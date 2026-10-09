use super::{SpaceId, WorkspaceRepository, WorkspaceResult, open_db};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Defaults for new work in a project; `icon_path` is a local presentation asset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectSettings {
    pub name: String,
    pub icon: String,
    pub icon_path: Option<String>,
    pub provider: String,
    pub isolated: bool,
    pub branch_prefix: String,
    pub start_ref: String,
}

impl ProjectSettings {
    /// # Errors
    /// Rejects unbounded labels, unsupported providers and malformed Git defaults.
    pub fn validate(&self) -> WorkspaceResult<()> {
        if [
            &self.name,
            &self.icon,
            &self.provider,
            &self.branch_prefix,
            &self.start_ref,
        ]
        .into_iter()
        .any(|value| value.len() > 256 || value.chars().any(char::is_control))
            || !matches!(self.provider.as_str(), "" | "codex" | "claude" | "pi")
            || !matches!(
                self.icon.as_str(),
                "" | "folder"
                    | "code"
                    | "terminal"
                    | "bot"
                    | "globe"
                    | "book"
                    | "rocket"
                    | "server"
                    | "gamepad"
            )
            || self.start_ref.starts_with('-')
            || self.branch_prefix.starts_with('-')
            || [&self.branch_prefix, &self.start_ref]
                .into_iter()
                .any(|value| {
                    value.contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
                        || value.contains("..")
                        || value.contains("@{")
                })
        {
            return Err(super::WorkspacePersistenceError::operation(
                "Choose valid project labels, provider and Git defaults",
            ));
        }
        if let Some(path) = &self.icon_path {
            validate_path(path)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RegisteredProject {
    pub scope: SpaceId,
    pub cwd: String,
    pub collapsed: bool,
    pub settings: ProjectSettings,
}

impl WorkspaceRepository {
    /// # Errors
    /// Returns a workspace persistence failure.
    pub fn registered_projects(&self) -> WorkspaceResult<Vec<RegisteredProject>> {
        let conn = open_db(&self.path)
            .map_err(|error| self.database_error("open project registry", error))?;
        let mut statement = conn
            .prepare(
                "SELECT space_id, cwd, collapsed, settings FROM workspace_projects ORDER BY space_id, cwd",
            )
            .map_err(|error| self.database_error("read project registry", error))?;
        let projects = statement
            .query_map([], |row| {
                Ok(RegisteredProject {
                    scope: SpaceId::from_persistence(row.get(0)?),
                    cwd: row.get(1)?,
                    collapsed: row.get(2)?,
                    settings: serde_json::from_str(&row.get::<_, String>(3)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                })
            })
            .and_then(Iterator::collect)
            .map_err(|error| self.database_error("read project registry", error))?;
        Ok(projects)
    }

    /// Registration records a host path; opening the project validates its directory on that host.
    /// # Errors
    /// Rejects malformed host paths and persistence failures.
    pub fn register_project(&self, scope: SpaceId, cwd: &str) -> WorkspaceResult<()> {
        validate_path(cwd)?;
        let conn = open_db(&self.path)
            .map_err(|error| self.database_error("open project registry", error))?;
        conn.execute("INSERT INTO workspace_projects (space_id, cwd) VALUES (?1, ?2) ON CONFLICT(space_id, cwd) DO NOTHING", params![scope.persistence_value(), cwd])
            .map_err(|error| self.database_error("register project", error))?;
        Ok(())
    }

    /// # Errors
    /// Validates the complete settings record before committing it atomically.
    pub fn configure_project(
        &self,
        scope: SpaceId,
        cwd: &str,
        settings: &ProjectSettings,
    ) -> WorkspaceResult<()> {
        validate_path(cwd)?;
        settings.validate()?;
        let encoded = serde_json::to_string(settings)
            .map_err(|error| super::WorkspacePersistenceError::operation(error.to_string()))?;
        let conn = open_db(&self.path)
            .map_err(|error| self.database_error("open project registry", error))?;
        conn.execute("INSERT INTO workspace_projects (space_id, cwd, settings) VALUES (?1, ?2, ?3) ON CONFLICT(space_id, cwd) DO UPDATE SET settings = excluded.settings", params![scope.persistence_value(), cwd, encoded])
            .map_err(|error| self.database_error("configure project", error))?;
        Ok(())
    }

    /// # Errors
    /// Registers an observed project and toggles disclosure in one commit.
    pub fn toggle_project_collapsed(&self, scope: SpaceId, cwd: &str) -> WorkspaceResult<()> {
        validate_path(cwd)?;
        let conn = open_db(&self.path)
            .map_err(|error| self.database_error("open project registry", error))?;
        conn.execute("INSERT INTO workspace_projects (space_id, cwd, collapsed) VALUES (?1, ?2, 1) ON CONFLICT(space_id, cwd) DO UPDATE SET collapsed = NOT collapsed", params![scope.persistence_value(), cwd])
            .map_err(|error| self.database_error("collapse project", error))?;
        Ok(())
    }
}

fn validate_path(cwd: &str) -> WorkspaceResult<()> {
    let absolute = cwd.starts_with('/')
        || cwd.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && cwd
                .as_bytes()
                .get(1..3)
                .is_some_and(|root| root == b":\\" || root == b":/")
        || cwd.starts_with("\\\\");
    if !absolute || cwd.len() > 4096 || cwd.chars().any(char::is_control) {
        return Err(super::WorkspacePersistenceError::operation(
            "Choose an absolute project path without control characters",
        ));
    }
    Ok(())
}
