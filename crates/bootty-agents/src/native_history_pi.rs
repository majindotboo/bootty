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
        if current.status != NativeSessionStatus::Idle {
            return Err("Wait for the current Pi run before reading earlier history".into());
        }
        let mut reader = open_history(&self.config, current)?;
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

fn open_history(
    config: &crate::NativeSessionConfig,
    snapshot: &NativeSessionSnapshot,
) -> Result<BufReader<FileReader>, String> {
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
    let remote = config
        .remote
        .as_ref()
        .map(|remote| RemoteHost::new(remote.host.clone()));
    let request = FileRequest::OpenReader {
        root: account
            .join("sessions")
            .to_str()
            .ok_or("Pi account is not UTF-8")?
            .to_owned(),
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
    let FileResponse::Source(descriptor) = response else {
        return Err("Pi history requires its captured file descriptor".into());
    };
    FileReader::open(&descriptor, remote.as_ref())
        .map(BufReader::new)
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

fn read_line(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_LINE_BYTES + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LINE_BYTES
        || !bytes.is_empty() && bytes.last() != Some(&b'\n')
    {
        return Err("Pi history contains an oversized or incomplete record".into());
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
