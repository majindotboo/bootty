//! Claude history reads the captured session file. Its entries chain by `parentUuid`, so the
//! conversation is the walk back from the newest entry on the main thread.
use crate::native_history::{HistoryPosition, PAGE_SIZE};
use crate::native_history_pi::read_line;
use crate::native_protocol::field;
use crate::{
    AgentKind, NativeAgentSession, NativeHistoryPage, NativeSessionSnapshot, NativeSessionStatus,
};
use bootty_host::{
    SystemCommandRunner,
    file_reader::FileReader,
    files::{FileRequest, FileResponse},
    remote::RemoteHost,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufReader, Seek, SeekFrom},
    path::Path,
};

// A bounded metadata index avoids loading every message body. Revisit these bounds when Claude
// exposes a cursor-based history API.
const MAX_ENTRIES: usize = 65_536;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    uuid: Option<String>,
    parent_uuid: Option<String>,
    /// A compacted conversation starts a new chain that names the one it replaced.
    logical_parent_uuid: Option<String>,
    #[serde(default)]
    is_sidechain: bool,
}

struct IndexedEntry {
    entry: Entry,
    offset: u64,
    bytes: usize,
}

impl NativeAgentSession {
    pub(crate) fn history_page_claude(
        &self,
        current: &NativeSessionSnapshot,
        previous: Option<&HistoryPosition>,
        direction: &str,
    ) -> Result<(HistoryPosition, NativeHistoryPage), String> {
        if !matches!(direction, "latest" | "older" | "newer") {
            return Err("History direction must be older, newer or latest".into());
        }
        if !matches!(
            current.status,
            NativeSessionStatus::Idle | NativeSessionStatus::Working | NativeSessionStatus::Waiting
        ) {
            return Err("Resume Claude before reading earlier history".into());
        }
        // A resumed conversation knows its identity before Claude's first turn reports it. One
        // without any identity hasn't run a turn, so it has no history yet.
        let Some(session) = self.history_identity(current) else {
            return Ok(empty_history());
        };
        if !crate::terminal_history::valid_id(session) {
            return Err("Claude has an invalid session identity".into());
        }
        // Claude writes the file with the conversation's first record; until then it has none.
        let Some(mut reader) = open_history(&self.config, &format!("{session}.jsonl"))? else {
            return Ok(empty_history());
        };
        let size = reader.get_ref().len();
        if size > MAX_FILE_BYTES {
            return Err("Claude history exceeds the bounded regular-file reader".into());
        }
        let entries = read_index(&mut reader, size)?;
        let branch = history_branch(&entries, false)?;
        let ids = branch
            .iter()
            .filter_map(|entry| entry.entry.uuid.as_deref())
            .collect::<Vec<_>>();
        let mut limit = PAGE_SIZE;
        let (start, end, history) = loop {
            let (start, end) = page_range(&ids, previous, direction, limit)?;
            if let Some(history) = project_branch(
                &mut reader,
                branch
                    .get(start..end)
                    .ok_or("Invalid Claude history page range")?,
            )? {
                break (start, end, history);
            }
            if limit == 1 {
                return Err("This Claude history entry exceeds the bounded transcript view".into());
            }
            limit /= 2;
        };
        reader
            .get_mut()
            .verify_source()
            .map_err(|error| error.to_string())?;
        let position = HistoryPosition {
            older: (start > 0)
                .then(|| ids.get(start).map(|id| (*id).to_owned()))
                .flatten(),
            newer: (end < ids.len())
                .then(|| ids.get(end.saturating_sub(1)).map(|id| (*id).to_owned()))
                .flatten(),
            at_latest: end == ids.len(),
            pi_leaf: None,
            codex_legacy: false,
        };
        let page = NativeHistoryPage {
            transcript: history.transcript,
            has_older: position.older.is_some(),
            has_newer: position.newer.is_some(),
            at_latest: position.at_latest,
        };
        Ok((position, page))
    }
}

/// The history of a conversation that hasn't written any yet.
fn empty_history() -> (HistoryPosition, NativeHistoryPage) {
    let position = HistoryPosition {
        at_latest: true,
        ..HistoryPosition::default()
    };
    let page = NativeHistoryPage {
        transcript: Vec::new(),
        has_older: false,
        has_newer: false,
        at_latest: true,
    };
    (position, page)
}

/// Claude names a project's session folder after its directory, with every character that isn't
/// a letter or digit replaced by `-`.
fn project_folder(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// The latest page of a subagent's transcript. Claude keeps each subagent in its own file, named
/// for the task it reports.
pub fn subagent_transcript(
    config: &crate::NativeSessionConfig,
    session: &str,
    agent: &str,
) -> Result<NativeSessionSnapshot, String> {
    if ![session, agent]
        .into_iter()
        .all(crate::terminal_history::valid_id)
    {
        return Err("Claude reported an invalid subagent identity".into());
    }
    let Some(mut reader) =
        open_history(config, &format!("{session}/subagents/agent-{agent}.jsonl"))?
    else {
        // The subagent hasn't written its first record yet.
        return Ok(NativeSessionSnapshot::new(AgentKind::Claude));
    };
    let size = reader.get_ref().len();
    if size > MAX_FILE_BYTES {
        return Err("This subagent's transcript exceeds the bounded regular-file reader".into());
    }
    let entries = read_index(&mut reader, size)?;
    let branch = history_branch(&entries, true)?;
    let mut limit = PAGE_SIZE;
    loop {
        let start = branch.len().saturating_sub(limit);
        if let Some(history) = project_branch(&mut reader, branch.get(start..).unwrap_or_default())?
        {
            reader
                .get_mut()
                .verify_source()
                .map_err(|error| error.to_string())?;
            return Ok(history);
        }
        if limit == 1 {
            return Err("This subagent entry exceeds the bounded transcript view".into());
        }
        limit /= 2;
    }
}

/// Opens a file under the project's session folder in the conversation's account, or `None`
/// while Claude hasn't written it.
fn open_history(
    config: &crate::NativeSessionConfig,
    file: &str,
) -> Result<Option<BufReader<FileReader>>, String> {
    let projects = Path::new(
        config
            .account_directory
            .as_deref()
            .ok_or("Claude has no captured account")?,
    )
    .join("projects");
    // Claude runs in the resolved directory, so a local link like /tmp names /private/tmp.
    let cwd = if config.remote.is_none() {
        std::fs::canonicalize(&config.cwd).unwrap_or_else(|_| config.cwd.clone())
    } else {
        config.cwd.clone()
    };
    let path = projects.join(project_folder(&cwd)).join(file);
    open_log(
        config,
        projects.to_str().ok_or("Claude account is not UTF-8")?,
        path.to_str().ok_or("Claude history path is not UTF-8")?,
    )
    .map(|reader| reader.map(BufReader::new))
}

/// Opens a log Claude writes at `path`, inside `root`, on the conversation's host. A log Claude
/// hasn't written yet is none.
fn open_log(
    config: &crate::NativeSessionConfig,
    root: &str,
    path: &str,
) -> Result<Option<FileReader>, String> {
    let remote = config
        .remote
        .as_ref()
        .map(|remote| RemoteHost::new(remote.host.clone()));
    let request = FileRequest::OpenReader {
        root: root.to_owned(),
        path: path.to_owned(),
    };
    let response = remote
        .as_ref()
        .map_or_else(
            || request.execute(),
            |remote| request.execute_remote(remote, SystemCommandRunner),
        )
        .map_err(|error| error.to_string())?;
    let descriptor = match response {
        FileResponse::Source(descriptor) => descriptor,
        FileResponse::Missing => return Ok(None),
        _ => return Err("Claude history requires its captured file descriptor".into()),
    };
    FileReader::open_log(&descriptor, remote.as_ref())
        .map(Some)
        .map_err(|error| error.to_string())
}

/// The most of a background command's output its tab shows: the end, where it's still writing.
const MAX_COMMAND_OUTPUT: u64 = 64 * 1024;

/// A Claude background command as a conversation: the command, the end of its output so far,
/// and Claude's summary once it has ended.
/// # Errors
/// Returns an output file that can't be read.
pub fn command_transcript(
    config: &crate::NativeSessionConfig,
    agent: &crate::NativeSubagent,
    path: &str,
    summary: &str,
) -> Result<Vec<crate::NativeTranscriptItem>, String> {
    let root = Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .ok_or("The command's output has no folder")?;
    let output = match open_log(config, root, path)? {
        Some(mut reader) => {
            let start = reader.len().saturating_sub(MAX_COMMAND_OUTPUT);
            reader
                .seek(SeekFrom::Start(start))
                .map_err(|error| error.to_string())?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(
                &mut std::io::Read::take(&mut reader, MAX_COMMAND_OUTPUT),
                &mut bytes,
            )
            .map_err(|error| error.to_string())?;
            let text = String::from_utf8_lossy(&bytes);
            // A cut start falls mid-line; show from the next whole line.
            if start > 0 {
                text.split_once('\n')
                    .map_or("", |(_, rest)| rest)
                    .to_owned()
            } else {
                text.into_owned()
            }
        }
        None => String::new(),
    };
    let mut snapshot = NativeSessionSnapshot::new(AgentKind::Claude);
    let id = &agent.id;
    for (key, role, text) in [
        ("prompt", "user", agent.prompt.clone()),
        ("output", "assistant", fenced(&output)),
        ("result", "assistant", summary.to_owned()),
    ] {
        if !text.trim().is_empty() {
            snapshot.message(format!("subagent-{key}:{id}"), role, text, true);
        }
    }
    Ok(snapshot.transcript)
}

/// `text` in a code block whose fence no run of backticks in it can close.
fn fenced(text: &str) -> String {
    let text = text.trim_end();
    if text.is_empty() {
        return String::new();
    }
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2).saturating_add(1));
    format!("{fence}text\n{text}\n{fence}")
}

fn read_index(reader: &mut BufReader<FileReader>, size: u64) -> Result<Vec<IndexedEntry>, String> {
    let mut entries = Vec::new();
    let mut ids = BTreeSet::new();
    loop {
        let offset = reader
            .stream_position()
            .map_err(|error| error.to_string())?;
        if offset >= size {
            break;
        }
        let bytes = read_line(reader)?;
        if bytes.is_empty() {
            break;
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let entry: Entry = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        // Queue and prompt bookkeeping carries no identity and no conversation content.
        let Some(id) = entry.uuid.as_deref() else {
            continue;
        };
        if entries.len() >= MAX_ENTRIES
            || entry.kind.len() > 128
            || !crate::terminal_history::valid_id(id)
            || [&entry.parent_uuid, &entry.logical_parent_uuid]
                .into_iter()
                .flatten()
                .any(|parent| !crate::terminal_history::valid_id(parent))
            || !ids.insert(id.to_owned())
        {
            return Err(
                "Claude history has invalid, repeated or excessive entry identities".into(),
            );
        }
        entries.push(IndexedEntry {
            entry,
            offset,
            bytes: bytes.len(),
        });
    }
    Ok(entries)
}

/// The conversation's messages, oldest first, walked back from the newest entry: on the main
/// thread, or in a subagent's file, where every entry is a sidechain.
fn history_branch(entries: &[IndexedEntry], sidechain: bool) -> Result<Vec<&IndexedEntry>, String> {
    let index = entries
        .iter()
        .filter_map(|entry| Some((entry.entry.uuid.as_deref()?, entry)))
        .collect::<BTreeMap<_, _>>();
    let mut cursor = entries
        .iter()
        .rev()
        .find(|entry| entry.entry.is_sidechain == sidechain)
        .and_then(|entry| entry.entry.uuid.as_deref());
    let mut branch = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(id) = cursor {
        let Some(entry) = index.get(id) else {
            // A chain can name an entry Claude didn't keep; the history starts after it.
            break;
        };
        if !seen.insert(id) {
            return Err("Claude history branch contains a cycle".into());
        }
        if matches!(entry.entry.kind.as_str(), "user" | "assistant") {
            branch.push(*entry);
        }
        cursor = entry
            .entry
            .parent_uuid
            .as_deref()
            .or(entry.entry.logical_parent_uuid.as_deref());
    }
    branch.reverse();
    Ok(branch)
}

fn page_range(
    ids: &[&str],
    previous: Option<&HistoryPosition>,
    direction: &str,
    limit: usize,
) -> Result<(usize, usize), String> {
    if direction == "latest" {
        return Ok((ids.len().saturating_sub(limit), ids.len()));
    }
    let position = previous.ok_or("No history page is loaded")?;
    let id = match direction {
        "older" => position.older.as_deref(),
        "newer" => position.newer.as_deref(),
        _ => return Err("Invalid Claude history direction".into()),
    }
    .ok_or("There is no adjacent history page")?;
    let offset = ids
        .iter()
        .position(|candidate| *candidate == id)
        .ok_or("The Claude history cursor is no longer on this conversation")?;
    Ok(if direction == "older" {
        (offset.saturating_sub(limit), offset)
    } else {
        let start = offset.saturating_add(1);
        (start, start.saturating_add(limit).min(ids.len()))
    })
}

fn project_branch(
    reader: &mut BufReader<FileReader>,
    entries: &[&IndexedEntry],
) -> Result<Option<NativeSessionSnapshot>, String> {
    let mut history = NativeSessionSnapshot::new(AgentKind::Claude);
    let mut blocks = BTreeMap::new();
    for entry in entries {
        reader
            .seek(SeekFrom::Start(entry.offset))
            .map_err(|error| error.to_string())?;
        let bytes = read_line(reader)?;
        if bytes.len() != entry.bytes {
            return Err("Claude history changed during the read".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        project_entry(&mut history, &value, &mut blocks);
    }
    if serde_json::to_vec(&history.transcript)
        .map_err(|error| error.to_string())?
        .len()
        > 512 * 1024
    {
        return Ok(None);
    }
    Ok(Some(history))
}

/// Projects one entry with the live reducers, so history and live rows share their ids.
fn project_entry(
    history: &mut NativeSessionSnapshot,
    entry: &Value,
    blocks: &mut BTreeMap<String, usize>,
) {
    let timestamp = field(entry, "timestamp")
        .as_str()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| time.timestamp_millis());
    let message = field(entry, "message");
    history.with_message_times((timestamp, timestamp), |history| {
        if field(entry, "type") == "assistant" {
            let Some(id) = field(message, "id").as_str() else {
                return;
            };
            // Claude saves each content block as its own entry under one message id.
            for block in field(message, "content").as_array().into_iter().flatten() {
                let index = blocks.entry(id.to_owned()).or_insert(0_usize);
                crate::native_claude::content_block(
                    history,
                    format!("claude-{id}-{index}"),
                    block,
                    true,
                );
                *index = index.saturating_add(1);
            }
            return;
        }
        if field(entry, "isMeta") == true {
            return;
        }
        let content = field(message, "content");
        let text = content.as_str().map_or_else(
            || {
                content
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|block| field(block, "type") == "text")
                    .filter_map(|block| field(block, "text").as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            str::to_owned,
        );
        if let Some(id) = field(entry, "uuid").as_str()
            && !text.trim().is_empty()
        {
            history.message(id.to_owned(), "user", text, true);
        }
        crate::native_claude::tool_results(history, entry);
    });
}
