use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

use crate::{
    AgentKind,
    terminal_history::{
        TerminalHistoryEntry, TerminalHistoryQuery, matches_project, timestamp, title, valid_id,
        valid_path,
    },
};

// Read only bounded metadata windows. Large transcript bodies never enter the catalog.
// Revisit the 64 KiB windows if providers stop appending their current title metadata.
const WINDOW: u64 = 64 * 1024;
const MAX_CANDIDATES: usize = 4096;
const MAX_READ_FILES: usize = 512;

struct Candidate {
    path: PathBuf,
    modified: Option<i64>,
}

fn modified(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
}

fn candidates(root: &Path) -> Result<Vec<Candidate>, String> {
    let mut result = Vec::new();
    let projects = match fs::read_dir(root) {
        Ok(projects) => projects,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(_) => return Err("Provider history store could not be read".to_owned()),
    };
    let mut visited = 0_usize;
    for project in projects {
        visited = visited.saturating_add(1);
        if visited > MAX_CANDIDATES {
            return Err("Provider history scan exceeds 4096 entries".to_owned());
        }
        let project = project.map_err(|_| "Provider history project could not be read")?;
        let kind = project
            .file_type()
            .map_err(|_| "Provider history project type is unavailable")?;
        // Provider-managed subagents and symlinked stores do not become main sessions.
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        for file in fs::read_dir(project.path())
            .map_err(|_| "Provider history project could not be opened")?
        {
            visited = visited.saturating_add(1);
            if visited > MAX_CANDIDATES {
                return Err("Provider history scan exceeds 4096 entries".to_owned());
            }
            let file = file.map_err(|_| "Provider history session could not be read")?;
            let kind = file
                .file_type()
                .map_err(|_| "Provider history session type is unavailable")?;
            if !kind.is_file()
                || kind.is_symlink()
                || file
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let metadata = file
                .metadata()
                .map_err(|_| "Provider history session metadata is unavailable")?;
            result.push(Candidate {
                path: file.path(),
                modified: metadata.modified().ok().and_then(modified),
            });
        }
    }
    result.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(result)
}

fn windows(path: &Path) -> Result<(Vec<u8>, Vec<u8>, bool), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Provider history session is unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Provider history session is not a regular account-local file".to_owned());
    }
    let mut file = File::open(path).map_err(|_| "Provider history session could not be opened")?;
    let size = metadata.len();
    let complete = size <= WINDOW * 2;
    let mut first = Vec::new();
    Read::by_ref(&mut file)
        .take(if complete { WINDOW * 2 } else { WINDOW })
        .read_to_end(&mut first)
        .map_err(|_| "Provider history metadata could not be read")?;
    if complete {
        return Ok((first, Vec::new(), true));
    }
    if let Some(end) = first.iter().rposition(|byte| *byte == b'\n') {
        first.truncate(end.saturating_add(1));
    } else {
        first.clear();
    }
    file.seek(SeekFrom::Start(size.saturating_sub(WINDOW)))
        .map_err(|_| "Provider history metadata could not be positioned")?;
    let mut last = Vec::new();
    file.take(WINDOW)
        .read_to_end(&mut last)
        .map_err(|_| "Provider history metadata could not be read")?;
    // A tail window starts inside an arbitrary record; never interpret that fragment.
    if let Some(start) = last.iter().position(|byte| *byte == b'\n') {
        last.drain(..=start);
    } else {
        last.clear();
    }
    Ok((first, last, false))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    session_id: Option<String>,
    version: Option<u32>,
    cwd: Option<PathBuf>,
    timestamp: Option<String>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    summary: Option<String>,
    name: Option<String>,
    is_sidechain: Option<bool>,
    relocated_cwd: Option<PathBuf>,
    message: Option<serde_json::Value>,
}

fn records(bytes: &[u8]) -> impl Iterator<Item = Record> + '_ {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Record>(line).ok())
}

fn parse(
    query: &TerminalHistoryQuery<'_>,
    candidate: &Candidate,
) -> Result<Option<TerminalHistoryEntry>, String> {
    let (first, last, complete) = windows(&candidate.path)?;
    let mut id = None;
    let mut cwd = None;
    let mut created_at = None;
    let mut stored_title = None;
    let mut summary = None;
    let mut sidechain = false;
    for record in records(&first) {
        match query.provider {
            AgentKind::Pi if record.kind == "session" => {
                if record
                    .version
                    .is_none_or(|version| !(1..=3).contains(&version))
                {
                    return Err("Pi history session version is unsupported".to_owned());
                }
                id = record.id;
                cwd = record.cwd;
                created_at = timestamp(record.timestamp.as_deref());
                break;
            }
            AgentKind::Claude => {
                if id.is_none() {
                    id = record.session_id;
                }
                if cwd.is_none() {
                    cwd = record.cwd;
                }
                if created_at.is_none() {
                    created_at = timestamp(record.timestamp.as_deref());
                }
                sidechain |= record.is_sidechain == Some(true);
            }
            _ => {}
        }
    }
    if sidechain {
        return Ok(None);
    }
    let id = id.ok_or("Provider history session has no supported identity metadata")?;
    let mut cwd = cwd.ok_or("Provider history session has no project metadata")?;
    if !valid_id(&id) || !valid_path(&cwd) {
        return Err("Provider history session identity or project is invalid".to_owned());
    }
    if query.provider == AgentKind::Claude
        && candidate.path.file_stem().and_then(|stem| stem.to_str()) != Some(id.as_str())
    {
        return Err("Claude history identity does not match its saved file".to_owned());
    }
    // In large files only the tail can prove the current stored title. A title in
    // the prefix may have been renamed or cleared in the unsampled transcript.
    for record in records(if complete { &first } else { &last }) {
        match (query.provider, record.kind.as_str()) {
            (AgentKind::Claude, "custom-title")
                if record.session_id.as_deref() == Some(id.as_str()) =>
            {
                stored_title = title(record.custom_title);
            }
            (AgentKind::Claude, "summary") => summary = title(record.summary),
            (AgentKind::Claude, "ai-title")
                if record.session_id.as_deref() == Some(id.as_str()) =>
            {
                summary = title(record.ai_title);
            }
            (AgentKind::Claude, "relocated")
                if record.session_id.as_deref() == Some(id.as_str()) =>
            {
                if let Some(relocated) = record.relocated_cwd {
                    if !valid_path(&relocated) {
                        return Err("Claude history relocated project is invalid".to_owned());
                    }
                    cwd = relocated;
                }
            }
            (AgentKind::Pi, "session_info") => stored_title = title(record.name),
            _ => {}
        }
    }
    if !matches_project(&cwd, query.cwd) {
        return Ok(None);
    }
    let resume_id = if query.provider == AgentKind::Pi {
        candidate
            .path
            .to_str()
            .ok_or("Pi history session path is not valid UTF-8")?
            .to_owned()
    } else {
        id.clone()
    };
    Ok(Some(TerminalHistoryEntry {
        provider: query.provider,
        title: stored_title.or(summary).or_else(|| {
            records(&first).find_map(|record| first_user_title(record, query.provider, &id))
        }),
        session_id: id,
        created_at,
        updated_at: candidate.modified,
        cwd,
        account_directory: query.account_directory.to_owned(),
        resume_id,
    }))
}

fn first_user_title(record: Record, provider: AgentKind, session_id: &str) -> Option<String> {
    let owns_message = match provider {
        AgentKind::Pi => record.kind == "message",
        AgentKind::Claude => {
            record.kind == "user" && record.session_id.as_deref() == Some(session_id)
        }
        AgentKind::Codex => false,
    };
    if !owns_message {
        return None;
    }
    let message = record.message?;
    if message.get("role")?.as_str()? != "user" {
        return None;
    }
    let content = message.get("content")?;
    let text = content.as_str().or_else(|| {
        content.as_array()?.iter().find_map(|block| {
            (block.get("type")?.as_str()? == "text")
                .then(|| block.get("text")?.as_str())
                .flatten()
        })
    })?;
    let preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
    title(Some(preview.chars().take(120).collect()))
}

pub fn query(query: &TerminalHistoryQuery<'_>) -> Result<Vec<TerminalHistoryEntry>, String> {
    let account = match query.account_directory.canonicalize() {
        Ok(account) => account,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Provider history account directory is unavailable".to_owned()),
    };
    let root = account.join(match query.provider {
        AgentKind::Claude => "projects",
        _ => "sessions",
    });
    if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(
            "Provider history store cannot leave its selected account directory".to_owned(),
        );
    }
    let mut result = Vec::new();
    for (index, candidate) in candidates(&root)?.iter().enumerate() {
        if index >= MAX_READ_FILES {
            return Err("Provider history metadata scan exceeds 512 saved files".to_owned());
        }
        if let Some(entry) = parse(query, candidate)? {
            result.push(entry);
            if result.len() == query.limit {
                break;
            }
        }
    }
    Ok(result)
}
