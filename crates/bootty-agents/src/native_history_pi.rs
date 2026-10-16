//! Pi history follows its captured version-3 session tree, including messages before compaction.
use crate::native_history::{HistoryPosition, PAGE_SIZE};
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
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

// A bounded metadata index avoids loading image bodies for the whole conversation.
// Revisit these bounds when Pi exposes a cursor-based branch history API.
const MAX_ENTRIES: usize = 16_384;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    parent_id: Option<String>,
    version: Option<u32>,
}
struct IndexedEntry {
    entry: Entry,
    offset: u64,
    bytes: usize,
}

impl NativeAgentSession {
    pub(crate) fn history_page_pi(
        &self,
        current: &NativeSessionSnapshot,
        previous: Option<&HistoryPosition>,
        direction: &str,
    ) -> Result<(HistoryPosition, NativeHistoryPage), String> {
        if !matches!(direction, "latest" | "older" | "newer") {
            return Err("History direction must be older, newer or latest".into());
        }
        self.verify_pi_identity()?;
        if !matches!(
            current.status,
            NativeSessionStatus::Idle | NativeSessionStatus::Working | NativeSessionStatus::Waiting
        ) {
            return Err("Resume Pi before reading earlier history".into());
        }
        // Pi writes its session file with the conversation's first entry; until then it has none.
        let Some(mut reader) = open_history(&self.config, current)? else {
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
            return Ok((position, page));
        };
        let size = reader.get_ref().len();
        if size > MAX_FILE_BYTES {
            return Err("Pi history exceeds the bounded regular-file reader".into());
        }
        let entries = read_index(
            &mut reader,
            size,
            current
                .session_id
                .as_deref()
                .ok_or("Pi has no captured session identity")?,
        )?;
        // Pi's supported RPC supplies the live leaf, which can differ from the file's last entry.
        let state = self.rpc(
            "get_entries",
            entries
                .last()
                .map_or_else(|| json!({}), |last| json!({"since":last.entry.id})),
        )?;
        let leaf = field(&state, "leafId").as_str().map(str::to_owned);
        if !field(&state, "leafId").is_null() && leaf.is_none() {
            return Err("Pi history has no valid live branch leaf".into());
        }
        if direction != "latest" && previous.is_none_or(|position| position.pi_leaf != leaf) {
            return Err("Pi changed its history branch; return to the latest turns".into());
        }
        let branch = history_branch(&entries, leaf.as_deref())?;
        let mut limit = PAGE_SIZE;
        let (start, end, mut history) = loop {
            let (start, end) = page_range(&branch, previous, direction, limit)?;
            if let Some(history) = project_branch(
                &mut reader,
                branch
                    .get(start..end)
                    .ok_or("Invalid Pi history page range")?,
            )? {
                break (start, end, history);
            }
            if limit == 1 {
                return Err("This Pi history entry exceeds the bounded transcript view".into());
            }
            limit /= 2;
        };
        reader
            .get_mut()
            .verify_source()
            .map_err(|error| error.to_string())?;
        self.verify_pi_identity()?;
        history.restore_image_references(&current.transcript);
        let position = HistoryPosition {
            older: (start > 0)
                .then(|| branch.get(start).map(|entry| entry.entry.id.clone()))
                .flatten(),
            newer: (end < branch.len())
                .then(|| {
                    branch
                        .get(end.saturating_sub(1))
                        .map(|entry| entry.entry.id.clone())
                })
                .flatten(),
            at_latest: end == branch.len(),
            pi_leaf: leaf,
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

/// The custom entry Pi's subagents extension appends to the parent session for each child.
const SUBAGENT_STATE: &str = "subagents:agent-v1";

/// A child of Pi's subagents extension, read from its own session file. The parent session names
/// that file in the child's latest state entry, inside the child's folder beside the parent.
/// The transcript is empty until the child has written to it.
pub fn subagent_transcript(
    config: &crate::NativeSessionConfig,
    snapshot: &NativeSessionSnapshot,
    agent: &str,
) -> Result<NativeSessionSnapshot, String> {
    let relative = agent
        .strip_prefix("/root/")
        .filter(|path| path.split('/').all(task_name))
        .map(|path| Path::new("subagents").join(path))
        .ok_or("Pi reported an invalid subagent identity")?;
    let parent = snapshot
        .session_file
        .as_deref()
        .ok_or("Pi has no captured session file")?;
    let empty = NativeSessionSnapshot::new(AgentKind::Pi);
    let Some(mut reader) = open_history(config, snapshot)? else {
        return Ok(empty);
    };
    let Some(file) = subagent_file(
        &mut reader,
        snapshot
            .session_id
            .as_deref()
            .ok_or("Pi has no captured session identity")?,
        agent,
    )?
    else {
        return Ok(empty);
    };
    let file = Path::new(&file);
    // Pi names a session file `<time>_<session id>.jsonl`. The extension may name it through a
    // resolved link; the reader confines it to the child's real folder.
    let session = file
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".jsonl"))
        .and_then(|name| name.rsplit_once('_'))
        .map(|(_, id)| id)
        .filter(|_| {
            file.parent()
                .is_some_and(|folder| folder.ends_with(&relative))
        })
        .ok_or("Pi named a subagent transcript outside its folder")?;
    let folder = Path::new(parent)
        .parent()
        .ok_or("Pi's session file has no folder")?
        .join(relative);
    let Some(mut reader) = open_file(config, &folder, file)? else {
        return Ok(empty);
    };
    let size = reader.get_ref().len();
    if size > MAX_FILE_BYTES {
        return Err("This subagent's transcript exceeds the bounded regular-file reader".into());
    }
    let entries = read_index(&mut reader, size, session)?;
    // A child doesn't branch, so its last entry is its leaf.
    let branch = history_branch(&entries, entries.last().map(|last| last.entry.id.as_str()))?;
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

/// One segment of a subagent's path, as the extension names its tasks.
fn task_name(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 128
        && segment.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// The session file the parent session last named for `agent`.
/// Limit: each read scans the whole parent log, which a running child's tab repeats every few
/// seconds. Index the state entries if parent logs near the bounded reader's size.
fn subagent_file(
    reader: &mut BufReader<FileReader>,
    session: &str,
    agent: &str,
) -> Result<Option<String>, String> {
    let header = read_line(reader)?;
    if header.is_empty() {
        return Ok(None);
    }
    let header: Entry = serde_json::from_slice(&header).map_err(|error| error.to_string())?;
    if header.kind != "session" || header.version != Some(3) || header.id != session {
        return Err("Pi history does not match the captured version-3 session".into());
    }
    let mut file = None;
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            return Ok(file);
        }
        // Most entries aren't state entries; skip them without parsing.
        if !line
            .windows(SUBAGENT_STATE.len())
            .any(|window| window == SUBAGENT_STATE.as_bytes())
        {
            continue;
        }
        let entry: Value = serde_json::from_slice(&line).map_err(|error| error.to_string())?;
        let state = field(field(&entry, "data"), "agent");
        if field(&entry, "type") == "custom"
            && field(&entry, "customType") == SUBAGENT_STATE
            && field(field(&entry, "data"), "version") == 1
            && field(state, "id") == agent
        {
            file = field(state, "transcriptFile").as_str().map(str::to_owned);
        }
    }
}

/// The conversation's session file, or `None` while Pi hasn't written it.
fn open_history(
    config: &crate::NativeSessionConfig,
    snapshot: &NativeSessionSnapshot,
) -> Result<Option<BufReader<FileReader>>, String> {
    let account = Path::new(
        config
            .account_directory
            .as_deref()
            .ok_or("Pi has no captured account")?,
    );
    let path = Path::new(
        snapshot
            .session_file
            .as_deref()
            .ok_or("Pi has no captured session file")?,
    );
    open_file(config, &account.join("sessions"), path)
}

/// A session file under `root`, or `None` while Pi hasn't written it.
fn open_file(
    config: &crate::NativeSessionConfig,
    root: &Path,
    path: &Path,
) -> Result<Option<BufReader<FileReader>>, String> {
    let remote = config
        .remote
        .as_ref()
        .map(|remote| RemoteHost::new(remote.host.clone()));
    let request = FileRequest::OpenReader {
        root: root.to_str().ok_or("Pi account is not UTF-8")?.to_owned(),
        path: path
            .to_str()
            .ok_or("Pi history path is not UTF-8")?
            .to_owned(),
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
        _ => return Err("Pi history requires its captured file descriptor".into()),
    };
    FileReader::open_log(&descriptor, remote.as_ref())
        .map(|reader| Some(BufReader::new(reader)))
        .map_err(|error| error.to_string())
}

fn page_range(
    branch: &[&IndexedEntry],
    previous: Option<&HistoryPosition>,
    direction: &str,
    limit: usize,
) -> Result<(usize, usize), String> {
    if direction == "latest" {
        return Ok((branch.len().saturating_sub(limit), branch.len()));
    }
    let position = previous.ok_or("No history page is loaded")?;
    let id = match direction {
        "older" => position.older.as_ref(),
        "newer" => position.newer.as_ref(),
        _ => return Err("Invalid Pi history direction".into()),
    }
    .ok_or("There is no adjacent history page")?;
    let offset = branch
        .iter()
        .position(|entry| entry.entry.id == *id)
        .ok_or("The Pi history cursor is no longer on this branch")?;
    Ok(if direction == "older" {
        (offset.saturating_sub(limit), offset)
    } else {
        let start = offset.saturating_add(1);
        (start, start.saturating_add(limit).min(branch.len()))
    })
}

fn read_index(
    reader: &mut BufReader<FileReader>,
    size: u64,
    session_id: &str,
) -> Result<Vec<IndexedEntry>, String> {
    let header = read_line(reader)?;
    // Pi hasn't finished writing its session header, so there is no history yet.
    if header.is_empty() {
        return Ok(Vec::new());
    }
    let header: Entry = serde_json::from_slice(&header).map_err(|error| error.to_string())?;
    if header.kind != "session" || header.version != Some(3) || header.id != session_id {
        return Err("Pi history does not match the captured version-3 session".into());
    }
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
        if entries.len() >= MAX_ENTRIES
            || entry.kind.len() > 128
            || entry.kind == "session"
            || !crate::terminal_history::valid_id(&entry.id)
            || entry
                .parent_id
                .as_deref()
                .is_some_and(|parent| !crate::terminal_history::valid_id(parent))
            || !ids.insert(entry.id.clone())
        {
            return Err("Pi history has invalid, repeated or excessive entry identities".into());
        }
        entries.push(IndexedEntry {
            entry,
            offset,
            bytes: bytes.len(),
        });
    }
    Ok(entries)
}

fn history_branch<'a>(
    entries: &'a [IndexedEntry],
    leaf: Option<&str>,
) -> Result<Vec<&'a IndexedEntry>, String> {
    let index = entries
        .iter()
        .map(|entry| (entry.entry.id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut branch = Vec::new();
    let mut cursor = leaf;
    let mut seen = BTreeSet::new();
    while let Some(id) = cursor {
        let entry = index
            .get(id)
            .ok_or("Pi history branch references a missing entry")?;
        if !seen.insert(entry.entry.id.as_str()) {
            return Err("Pi history branch contains a cycle".into());
        }
        if matches!(
            entry.entry.kind.as_str(),
            "message" | "custom_message" | "branch_summary" | "compaction"
        ) {
            branch.push(*entry);
        }
        cursor = entry.entry.parent_id.as_deref();
    }
    branch.reverse();
    Ok(branch)
}

fn project_branch(
    reader: &mut BufReader<FileReader>,
    entries: &[&IndexedEntry],
) -> Result<Option<NativeSessionSnapshot>, String> {
    let mut history = NativeSessionSnapshot::new(AgentKind::Pi);
    let mut expected = BTreeSet::new();
    for entry in entries {
        reader
            .seek(SeekFrom::Start(entry.offset))
            .map_err(|error| error.to_string())?;
        let bytes = read_line(reader)?;
        if bytes.len() != entry.bytes {
            return Err("Pi history changed during the read".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let mut single = NativeSessionSnapshot::new(AgentKind::Pi);
        project_entry(&mut single, &value);
        expected.extend(single.transcript.into_iter().map(|item| item.id));
        project_entry(&mut history, &value);
    }
    if expected
        .iter()
        .any(|id| !history.transcript.iter().any(|item| &item.id == id))
        || serde_json::to_vec(&history.transcript)
            .map_err(|error| error.to_string())?
            .len()
            > 512 * 1024
    {
        return Ok(None);
    }
    Ok(Some(history))
}

pub fn read_line(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_LINE_BYTES + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LINE_BYTES {
        return Err("Provider history contains an oversized record".into());
    }
    // The log is read at the length it had when it opened. A last record without its newline is
    // one the agent is still writing, so the log ends before it.
    if bytes.last() != Some(&b'\n') {
        bytes.clear();
    }
    Ok(bytes)
}

fn project_entry(history: &mut NativeSessionSnapshot, entry: &Value) {
    if field(entry, "type") == "message" {
        crate::native_pi::message_item(history, field(entry, "message"), true);
    } else {
        let timestamp = field(entry, "timestamp")
            .as_str()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .map(|time| time.timestamp_millis());
        history.with_message_times((timestamp, timestamp), |history| {
            if field(entry, "type") == "custom_message" && field(entry, "display") == true {
                history.message(
                    format!(
                        "pi-entry-{}",
                        field(entry, "id").as_str().unwrap_or_default()
                    ),
                    "custom",
                    crate::native_protocol::content_text(field(entry, "content")),
                    true,
                );
            } else if let Some(summary) = field(entry, "summary").as_str() {
                history.message(
                    format!(
                        "pi-entry-{}",
                        field(entry, "id").as_str().unwrap_or_default()
                    ),
                    "notice",
                    summary.to_owned(),
                    true,
                );
            }
        });
    }
}
