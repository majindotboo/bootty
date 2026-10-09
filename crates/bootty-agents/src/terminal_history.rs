use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::AgentKind;

/// Worker-only history query. The caller supplies the account and project scope.
pub struct TerminalHistoryQuery<'a> {
    pub provider: AgentKind,
    pub program: &'a str,
    pub account_directory: &'a Path,
    pub cwd: Option<&'a Path>,
    pub limit: usize,
}

/// Provider-owned saved identity and safe metadata; timestamps are UTC epoch milliseconds.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TerminalHistoryEntry {
    pub provider: AgentKind,
    pub session_id: String,
    pub title: Option<String>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub cwd: PathBuf,
    pub account_directory: PathBuf,
    /// Codex/Claude use the exact provider ID; Pi uses the canonical saved file path.
    pub resume_id: String,
}

pub fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
}

pub fn matches_project(saved: &Path, requested: Option<&Path>) -> bool {
    requested.is_none_or(|requested| {
        requested == saved
            || match (requested.canonicalize(), saved.canonicalize()) {
                (Ok(requested), Ok(saved)) => requested == saved,
                _ => false,
            }
    })
}

pub fn title(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_owned()).filter(|value| {
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
    })
}

pub fn timestamp(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis())
}

/// Read saved provider conversations without resuming or changing a session.
///
/// Codex lists non-archived interactive threads from its state database. Claude/Pi
/// read their standard account-local stores. Custom Pi `--session-dir` stores are
/// unsupported until the caller can supply their provider-owned scope explicitly.
///
/// # Errors
/// Returns invalid scope, unsupported metadata, bounded scan or provider query errors.
pub fn terminal_provider_history(
    query: &TerminalHistoryQuery<'_>,
) -> Result<Vec<TerminalHistoryEntry>, String> {
    if !valid_path(query.account_directory) || query.cwd.is_some_and(|cwd| !valid_path(cwd)) {
        return Err("Provider history requires absolute account and project paths".to_owned());
    }
    if query.limit == 0 || query.limit > 200 {
        return Err("Provider history limit must be between 1 and 200".to_owned());
    }
    let mut entries = match query.provider {
        AgentKind::Codex => crate::terminal_history_codex::query(query)?,
        AgentKind::Claude | AgentKind::Pi => crate::terminal_history_files::query(query)?,
    };
    let mut identities = BTreeSet::new();
    for entry in &entries {
        if !valid_id(&entry.session_id)
            || !valid_path(&entry.cwd)
            || !matches_project(&entry.cwd, query.cwd)
            || !identities.insert(entry.session_id.clone())
        {
            return Err(
                "Provider history contains invalid or ambiguous session metadata".to_owned(),
            );
        }
    }
    entries.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    entries.truncate(query.limit);
    Ok(entries)
}
