//! Bounded shell-owned history readers and deterministic, metadata-aware ranking.
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
};

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 5000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub command: String,
    pub cwd: Option<String>,
    pub timestamp: Option<u64>,
    pub exit_code: Option<i32>,
    pub frequency: u32,
}
impl HistoryEntry {
    const fn new(command: String, timestamp: Option<u64>) -> Self {
        Self {
            command,
            timestamp,
            cwd: None,
            exit_code: None,
            frequency: 1,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryRequest {
    pub shell: String,
    pub path: String,
    pub query: String,
    pub cwd: String,
    pub recent: Vec<HistoryEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryResult {
    pub entries: Vec<HistoryEntry>,
    pub truncated: bool,
}
impl HistoryRequest {
    /// # Errors
    /// Returns invalid request, unsupported shell, history file, or bounded input errors.
    pub fn execute(&self) -> Result<HistoryResult> {
        if !matches!(self.shell.as_str(), "bash" | "zsh" | "fish") {
            bail!("History requires a supported shell report");
        }
        if self.query.len() > 4096
            || self.recent.len() > 128
            || serde_json::to_vec(self)?.len() > 256 * 1024
        {
            bail!("History request exceeds its bound");
        }
        let mut entries = Vec::new();
        let mut truncated = false;
        if !self.path.is_empty() {
            let path = if let Some(relative) = self.path.strip_prefix("~/") {
                Path::new(
                    &std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                        .context("History host has no home directory")?,
                )
                .join(relative)
            } else {
                self.path.clone().into()
            };
            if !path.is_absolute() {
                bail!("History path must be absolute on its host");
            }
            if std::fs::metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
                bail!("History must be a regular file");
            }
            match File::open(&path) {
                Ok(mut file) => {
                    let metadata = file.metadata()?;
                    if !metadata.is_file() {
                        bail!("History must be a regular file");
                    }
                    let start = metadata.len().saturating_sub(u64::try_from(MAX_BYTES)?);
                    file.seek(SeekFrom::Start(start))?;
                    let mut bytes = Vec::new();
                    file.take(u64::try_from(MAX_BYTES + 1)?)
                        .read_to_end(&mut bytes)?;
                    truncated = start > 0 || bytes.len() > MAX_BYTES;
                    bytes.truncate(MAX_BYTES);
                    // A tail read starts at a record boundary, never half a UTF-8 character.
                    let begin = if start > 0 {
                        bytes
                            .iter()
                            .position(|byte| *byte == b'\n')
                            .map_or(bytes.len(), |index| index.saturating_add(1))
                    } else {
                        0
                    };
                    entries = parse_history_bytes(
                        &self.shell,
                        bytes.get(begin..).context("history tail boundary")?,
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("read shell history"),
            }
        }
        truncated |= entries.len() >= MAX_ENTRIES;
        let start = entries.len().saturating_sub(MAX_ENTRIES);
        entries.drain(..start);
        entries.extend(self.recent.iter().cloned());
        let entries = rank_history(entries, &self.query, &self.cwd);
        // A search result is bounded independently from the scanned file.
        Ok(HistoryResult {
            entries: entries.into_iter().take(100).collect(),
            truncated,
        })
    }
    /// # Errors
    /// Returns request limit, remote execution, or response decoding errors.
    pub fn execute_remote(
        &self,
        remote: &crate::remote::RemoteHost,
        runner: impl crate::CommandRunner,
    ) -> Result<HistoryResult> {
        use crate::CommandRunner as _;
        let input = serde_json::to_vec(self)?;
        if input.len() > 256 * 1024 {
            bail!("History request exceeds its bound");
        }
        let output = crate::remote::RemoteCommandRunner::new(remote.clone(), runner)
            .run_with_input(
                crate::REMOTE_DAEMON_PROGRAM,
                &["shell-history".to_owned()],
                input,
            )?;
        if !output.success {
            bail!("Remote history failed: {}", output.stderr.trim());
        }
        if output.stdout.len() > 512 * 1024 {
            bail!("History response exceeds its bound");
        }
        serde_json::from_str(&output.stdout).context("decode remote history")
    }
}
/// # Errors
/// Returns input, payload limit, decoding, or history read errors.
pub fn receive(mut input: impl std::io::Read) -> Result<HistoryResult> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 256 * 1024 {
        bail!("History request exceeds its bound");
    }
    serde_json::from_slice::<HistoryRequest>(&bytes)?.execute()
}

#[must_use]
pub fn parse_history_bytes(shell: &str, bytes: &[u8]) -> Vec<HistoryEntry> {
    if shell != "zsh" {
        return parse_history(shell, &String::from_utf8_lossy(bytes));
    }
    // Zsh's Meta byte quotes otherwise special bytes, including UTF-8 octets.
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut bytes = bytes.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == 0x83 {
            if let Some(next) = bytes.next() {
                decoded.push(next ^ 32);
            }
        } else {
            decoded.push(byte);
        }
    }
    parse_history(shell, &String::from_utf8_lossy(&decoded))
}

#[must_use]
pub fn parse_history(shell: &str, content: &str) -> Vec<HistoryEntry> {
    let mut entries: VecDeque<HistoryEntry> = VecDeque::new();
    let mut timestamp = None;
    let mut continued = false;
    for line in content.lines() {
        while entries.len() > MAX_ENTRIES {
            entries.pop_front();
        }
        if shell == "fish" {
            if let Some(command) = line.strip_prefix("- cmd: ") {
                let mut text = String::new();
                let mut chars = command.chars();
                while let Some(ch) = chars.next() {
                    if ch == '\\' {
                        match chars.next() {
                            Some('n') => text.push('\n'),
                            Some('\\') | None => text.push('\\'),
                            Some(other) => {
                                text.push('\\');
                                text.push(other);
                            }
                        }
                    } else {
                        text.push(ch);
                    }
                }
                entries.push_back(HistoryEntry::new(text, None));
            } else if let Some(when) = line.strip_prefix("  when: ")
                && let Some(entry) = entries.back_mut()
            {
                entry.timestamp = when.parse().ok();
            }
            continue;
        }
        if continued && let Some(entry) = entries.back_mut() {
            entry.command.push('\n');
            entry.command.push_str(line);
        } else if shell == "zsh"
            && let Some((metadata, command)) = line
                .strip_prefix(": ")
                .and_then(|line| line.split_once(';'))
            && let Some((when, _duration)) = metadata.split_once(':')
        {
            entries.push_back(HistoryEntry::new(command.to_owned(), when.parse().ok()));
        } else if shell == "bash"
            && let Some(when) = line
                .strip_prefix('#')
                .filter(|when| !when.is_empty() && when.bytes().all(|byte| byte.is_ascii_digit()))
        {
            timestamp = when.parse().ok();
            continue;
        } else if shell == "bash"
            && timestamp.is_none()
            && let Some(entry) = entries.back_mut().filter(|entry| entry.timestamp.is_some())
        {
            entry.command.push('\n');
            entry.command.push_str(line);
        } else {
            entries.push_back(HistoryEntry::new(line.to_owned(), timestamp.take()));
        }
        continued = shell == "zsh"
            && line.ends_with('\\')
            && line
                .as_bytes()
                .iter()
                .rev()
                .take_while(|byte| **byte == b'\\')
                .count()
                % 2
                == 1;
        if continued && let Some(entry) = entries.back_mut() {
            entry.command.pop();
        }
    }
    while entries.len() > MAX_ENTRIES {
        entries.pop_front();
    }
    entries.into()
}

pub fn rank_history(entries: Vec<HistoryEntry>, query: &str, cwd: &str) -> Vec<HistoryEntry> {
    let mut unique: BTreeMap<String, (usize, HistoryEntry, bool)> = BTreeMap::new();
    for (index, mut entry) in entries.into_iter().enumerate() {
        if entry.command.is_empty()
            || entry.command.len() > 4096
            || entry.command.starts_with(char::is_whitespace)
            || entry.command.contains(['\0', '\x1b'])
        {
            continue;
        }
        let mut same_cwd = entry.cwd.as_deref() == Some(cwd);
        if let Some((_, previous, matched_cwd)) = unique.remove(&entry.command) {
            entry.frequency = previous.frequency.saturating_add(entry.frequency);
            same_cwd |= matched_cwd;
            if entry.timestamp.is_none() {
                entry.timestamp = previous.timestamp;
            }
            if entry.exit_code.is_none() {
                entry.exit_code = previous.exit_code;
            }
            if entry.cwd.is_none() {
                entry.cwd = previous.cwd;
            }
        }
        unique.insert(entry.command.clone(), (index, entry, same_cwd));
    }
    let query = query.to_lowercase();
    let mut ranked: Vec<_> = unique
        .into_values()
        .filter_map(|(index, entry, same_cwd)| {
            let lower = entry.command.to_lowercase();
            let mut query_chars = query.chars();
            let mut next = query_chars.next();
            for ch in lower.chars() {
                if next == Some(ch) {
                    next = query_chars.next();
                }
            }
            if next.is_some() {
                return None;
            }
            let score = usize::from(lower.starts_with(&query))
                .saturating_mul(20_000)
                .saturating_add(usize::from(same_cwd).saturating_mul(10_000))
                .saturating_add(
                    usize::try_from(entry.frequency.min(100))
                        .unwrap_or(100)
                        .saturating_mul(100),
                )
                .saturating_add(index);
            Some((score, index, entry))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let mut bytes = 0_usize;
    ranked
        .into_iter()
        .map(|(_, _, entry)| entry)
        .take_while(|entry| {
            bytes = bytes
                .saturating_add(entry.command.len())
                .saturating_add(entry.cwd.as_ref().map_or(0, String::len))
                .saturating_add(256);
            bytes <= 64 * 1024
        })
        .collect()
}
