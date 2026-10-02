use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AgentKind;

const WINDOW_BYTES: u64 = 256 * 1024;
const SCAN_BYTES: u64 = 16 * 1024 * 1024;
const SCAN_ENTRIES: usize = 8192;
const SCAN_FILES: usize = 128;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSessionHistory {
    pub provider: AgentKind,
    pub id: String,
    pub cwd: PathBuf,
    pub title: String,
    pub history_path: PathBuf,
    /// Filesystem modification time in Unix milliseconds.
    pub updated_at: u64,
    pub usage: Option<TerminalSessionUsage>,
}

/// Token counts from the latest observed response, not an account quota or billing total.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalSessionUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// Resolve only the provider transcript directory. This never opens credential stores.
#[must_use]
pub fn terminal_history_root(provider: AgentKind) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let (variable, suffix, default) = match provider {
        AgentKind::Codex => ("CODEX_HOME", "sessions", ".codex"),
        AgentKind::Claude => ("CLAUDE_CONFIG_DIR", "projects", ".claude"),
        AgentKind::Pi => ("PI_CODING_AGENT_DIR", "sessions", ".pi/agent"),
    };
    let override_path = if provider == AgentKind::Pi {
        std::env::var_os("PI_CODING_AGENT_SESSION_DIR").filter(|path| !path.is_empty())
    } else {
        None
    };
    let path = override_path.or_else(|| {
        std::env::var_os(variable)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(path).join(suffix).into_os_string())
    });
    let path = path.map_or_else(
        || {
            home.as_ref()
                .map(|home| PathBuf::from(home).join(default).join(suffix))
        },
        |path| Some(PathBuf::from(path)),
    )?;
    let path = if let Ok(suffix) = path.strip_prefix("~") {
        PathBuf::from(home?).join(suffix)
    } else {
        path
    };
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

/// Discover bounded, read-only metadata suitable for provider-native resume and fork.
///
/// Missing roots are empty; unreadable roots return an error. Unsupported or incomplete
/// records are skipped. The transcript scan reads at most 16 MiB across 128 recent files and 8192
/// directory entries. Codex titles read an additional 512 KiB of its native index.
/// Add provider queries when these limits need to grow.
///
/// # Errors
/// Returns an error when the root is unreadable, a symlink, or not a directory.
pub fn discover_terminal_history(
    provider: AgentKind,
    root: &Path,
) -> Result<Vec<TerminalSessionHistory>, String> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Cannot read agent history directory: {error}")),
    };
    if !metadata.is_dir() || metadata.is_symlink() {
        return Err("Agent history root must be a directory, not a symlink".to_owned());
    }
    let depth: u8 = if provider == AgentKind::Codex { 3 } else { 1 };
    let mut directories = vec![(root.to_path_buf(), depth)];
    let mut candidates = Vec::new();
    let mut visited = 0_usize;
    while let Some((directory, depth)) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if directory == root => return Err(error.to_string()),
            Err(_) => continue,
        };
        for entry in entries {
            visited = visited.saturating_add(1);
            if visited > SCAN_ENTRIES {
                break;
            }
            let Ok(entry) = entry else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && depth > 0 {
                directories.push((path, depth.saturating_sub(1)));
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
                let updated_at = modified
                    .and_then(|time| u64::try_from(time.as_millis()).ok())
                    .unwrap_or_default();
                candidates.push((updated_at, path));
            }
        }
        if visited > SCAN_ENTRIES {
            break;
        }
    }
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    let mut remaining = SCAN_BYTES;
    let mut identities = BTreeSet::new();
    let mut sessions = Vec::new();
    for (updated_at, path) in candidates.into_iter().take(SCAN_FILES) {
        if remaining == 0 {
            break;
        }
        if let Some(session) = read_session(provider, &path, updated_at, &mut remaining)
            && identities.insert(session.id.clone())
        {
            sessions.push(session);
        }
    }
    if provider == AgentKind::Codex {
        indexed_titles(root, &mut sessions);
    }
    Ok(sessions)
}

// Saved titles can be outside a transcript's bounded head/tail windows.
fn indexed_titles(root: &Path, sessions: &mut [TerminalSessionHistory]) {
    let Some(parent) = root.parent() else { return };
    let path = parent.join("session_index.jsonl");
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return;
    };
    if !metadata.is_file() || metadata.is_symlink() {
        return;
    }
    let Ok(mut file) = File::open(path) else {
        return;
    };
    // Read the latest 512 KiB; use a provider query if its index grows beyond this window.
    let offset = metadata.len().saturating_sub(512 * 1024);
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return;
    }
    let mut bytes = Vec::new();
    if file.take(512 * 1024).read_to_end(&mut bytes).is_err() {
        return;
    }
    let mut lines = bytes.split(|byte| *byte == b'\n');
    if offset != 0 {
        let _ = lines.next();
    }
    for line in lines {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(title) = value
            .get("thread_name")
            .and_then(Value::as_str)
            .and_then(short_title)
        else {
            continue;
        };
        if let Some(session) = sessions.iter_mut().find(|session| session.id == id) {
            session.title = title;
        }
    }
}

fn read_session(
    provider: AgentKind,
    path: &Path,
    updated_at: u64,
    remaining: &mut u64,
) -> Option<TerminalSessionHistory> {
    // Recheck the file after enumeration; never intentionally traverse transcript symlinks.
    if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let count = WINDOW_BYTES.min(length).min(*remaining);
    let mut head = Vec::new();
    file.by_ref().take(count).read_to_end(&mut head).ok()?;
    *remaining = remaining.saturating_sub(count);
    let mut record = ParsedSession::default();
    parse_window(provider, &head, &mut record);
    if length > count && *remaining > 0 {
        let count = WINDOW_BYTES
            .min(length.saturating_sub(count))
            .min(*remaining);
        file.seek(SeekFrom::Start(length.saturating_sub(count)))
            .ok()?;
        let mut tail = Vec::new();
        file.take(count).read_to_end(&mut tail).ok()?;
        *remaining = remaining.saturating_sub(count);
        // The first tail line may start in the middle of a JSON record.
        if let Some(start) = tail.iter().position(|byte| *byte == b'\n') {
            parse_window(provider, tail.get(start.saturating_add(1)..)?, &mut record);
        }
    }
    if record.sidechain {
        return None;
    }
    let id = record.id?;
    uuid::Uuid::try_parse(&id).ok()?;
    let cwd = PathBuf::from(record.cwd?);
    if !cwd.is_absolute() {
        return None;
    }
    let title = record
        .title
        .or(record.prompt)
        .unwrap_or_else(|| provider.default_program().to_owned());
    Some(TerminalSessionHistory {
        provider,
        id,
        cwd,
        title,
        history_path: path.to_path_buf(),
        updated_at,
        usage: record.usage,
    })
}

#[derive(Default)]
struct ParsedSession {
    id: Option<String>,
    cwd: Option<String>,
    title: Option<String>,
    prompt: Option<String>,
    usage: Option<TerminalSessionUsage>,
    sidechain: bool,
}

fn parse_window(provider: AgentKind, bytes: &[u8], record: &mut ParsedSession) {
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload = value.get("payload").unwrap_or(&Value::Null);
        match provider {
            AgentKind::Codex => {
                if kind == "session_meta" {
                    identity(record, payload, "id");
                } else if kind == "event_msg" {
                    if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                        first_prompt(record, payload.get("message"));
                    } else if let Some(usage) = payload
                        .get("info")
                        .and_then(|info| info.get("last_token_usage"))
                    {
                        record.usage = usage_counts(
                            usage,
                            "input_tokens",
                            "output_tokens",
                            "cached_input_tokens",
                            "total_tokens",
                        );
                    }
                }
            }
            AgentKind::Claude => {
                identity(record, &value, "sessionId");
                record.sidechain |= value.get("isSidechain").and_then(Value::as_bool) == Some(true);
                if kind == "custom-title" {
                    record.title = value
                        .get("customTitle")
                        .and_then(Value::as_str)
                        .and_then(short_title);
                }
                message(record, value.get("message"), provider);
            }
            AgentKind::Pi => {
                if kind == "session" && value.get("version").and_then(Value::as_u64) == Some(3) {
                    identity(record, &value, "id");
                } else if kind == "session_info" {
                    record.title = value
                        .get("name")
                        .and_then(Value::as_str)
                        .and_then(short_title);
                } else if kind == "message" {
                    message(record, value.get("message"), provider);
                }
            }
        }
    }
}

fn identity(record: &mut ParsedSession, value: &Value, id_key: &str) {
    if record.id.is_none() {
        record.id = value
            .get(id_key)
            .and_then(Value::as_str)
            .filter(|id| id.len() <= 64)
            .map(str::to_owned);
    }
    if record.cwd.is_none() {
        record.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| cwd.len() <= 4096)
            .map(str::to_owned);
    }
}

fn message(record: &mut ParsedSession, message: Option<&Value>, provider: AgentKind) {
    let Some(message) = message else { return };
    if message.get("role").and_then(Value::as_str) == Some("user") {
        first_prompt(record, message.get("content"));
    } else if let Some(usage) = message.get("usage") {
        record.usage = if provider == AgentKind::Pi {
            usage_counts(usage, "input", "output", "cacheRead", "totalTokens")
        } else {
            usage_counts(
                usage,
                "input_tokens",
                "output_tokens",
                "cache_read_input_tokens",
                "total_tokens",
            )
        };
    }
}

fn first_prompt(record: &mut ParsedSession, content: Option<&Value>) {
    if record.prompt.is_some() {
        return;
    }
    let Some(content) = content else { return };
    record.prompt = content.as_str().and_then(short_title).or_else(|| {
        content
            .as_array()?
            .iter()
            .find_map(|part| part.get("text")?.as_str().and_then(short_title))
    });
}

fn short_title(text: &str) -> Option<String> {
    let title: String = text
        .chars()
        .filter(|character| !character.is_control())
        .take(96)
        .collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

fn usage_counts(
    value: &Value,
    input: &str,
    output: &str,
    cached: &str,
    total: &str,
) -> Option<TerminalSessionUsage> {
    let counts = TerminalSessionUsage {
        input_tokens: value.get(input).and_then(Value::as_u64),
        output_tokens: value.get(output).and_then(Value::as_u64),
        cached_input_tokens: value.get(cached).and_then(Value::as_u64),
        total_tokens: value.get(total).and_then(Value::as_u64),
    };
    (counts.input_tokens.is_some()
        || counts.output_tokens.is_some()
        || counts.total_tokens.is_some())
    .then_some(counts)
}
