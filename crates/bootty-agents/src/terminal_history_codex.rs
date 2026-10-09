use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    terminal_history::{TerminalHistoryEntry, TerminalHistoryQuery, title, valid_id, valid_path},
    terminal_process,
};

// Thread lists include provider previews on the wire. Discard them after bounded decoding.
const MAX_RESPONSE: u64 = 1024 * 1024;
const MAX_PAGES: usize = 8;

#[derive(Deserialize)]
struct Response {
    id: Option<u64>,
    result: Option<Value>,
    error: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    id: String,
    name: Option<String>,
    created_at: Option<i64>,
    updated_at: Option<i64>,
    cwd: PathBuf,
    ephemeral: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    data: Vec<Thread>,
    next_cursor: Option<String>,
}

fn send(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value)
        .map_err(|_| "Codex history request could not be written")?;
    writer
        .write_all(b"\n")
        .and_then(|()| writer.flush())
        .map_err(|_| "Codex history transport failed".to_owned())
}

fn response(reader: &mut impl BufRead, id: u64) -> Result<Value, String> {
    let mut total = 0_usize;
    loop {
        let mut bytes = Vec::new();
        let count = Read::take(&mut *reader, MAX_RESPONSE + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "Codex history transport failed")?;
        total = total.saturating_add(count);
        if count == 0 {
            return Err("Codex exited before reporting saved history".to_owned());
        }
        if u64::try_from(total).unwrap_or(u64::MAX) > MAX_RESPONSE {
            return Err("Codex history response exceeds 1 MiB".to_owned());
        }
        let response: Response =
            serde_json::from_slice(&bytes).map_err(|_| "Codex history response is malformed")?;
        if response.id != Some(id) {
            continue;
        }
        if response.error.is_some() {
            return Err(
                "Codex could not list saved history with the supported read-only protocol"
                    .to_owned(),
            );
        }
        return response
            .result
            .ok_or_else(|| "Codex history response has no result".to_owned());
    }
}

fn epoch_millis(value: Option<i64>) -> Result<Option<i64>, String> {
    value
        .map(|seconds| {
            if seconds < 0 {
                return Err("Codex history timestamp is invalid".to_owned());
            }
            seconds
                .checked_mul(1000)
                .ok_or_else(|| "Codex history timestamp is invalid".to_owned())
        })
        .transpose()
}

fn list(
    query: &TerminalHistoryQuery<'_>,
    writer: &mut impl Write,
    reader: &mut impl BufRead,
) -> Result<Vec<TerminalHistoryEntry>, String> {
    send(
        writer,
        &json!({"id":1,"method":"initialize","params":{
            "clientInfo":{"name":"bootty_history","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":false}
        }}),
    )?;
    response(reader, 1)?;
    send(writer, &json!({"method":"initialized"}))?;
    let mut cursor: Option<String> = None;
    let mut entries = Vec::new();
    for page_index in 0..MAX_PAGES {
        let id = u64::try_from(page_index)
            .map_err(|_| "Codex history page is invalid")?
            .saturating_add(2);
        send(
            writer,
            &json!({"id":id,"method":"thread/list","params":{
                "cursor":cursor,
                "limit":query.limit.saturating_sub(entries.len()),
                "sortKey":"updated_at",
                "sortDirection":"desc",
                "sourceKinds":["cli","vscode","appServer"],
                "archived":false,
                "cwd":query.cwd,
                "useStateDbOnly":true
            }}),
        )?;
        let page: Page = serde_json::from_value(response(reader, id)?)
            .map_err(|_| "Codex history metadata is malformed")?;
        if page.data.len() > query.limit.saturating_sub(entries.len()) {
            return Err("Codex history response exceeds the requested limit".to_owned());
        }
        for saved in page.data {
            if saved.ephemeral {
                continue;
            }
            if !valid_id(&saved.id)
                || !valid_path(&saved.cwd)
                || !crate::terminal_history::matches_project(&saved.cwd, query.cwd)
            {
                return Err(
                    "Codex history reported a session outside the requested scope".to_owned(),
                );
            }
            entries.push(TerminalHistoryEntry {
                provider: query.provider,
                session_id: saved.id.clone(),
                title: title(saved.name),
                created_at: epoch_millis(saved.created_at)?,
                updated_at: epoch_millis(saved.updated_at)?,
                cwd: saved.cwd,
                account_directory: query.account_directory.to_owned(),
                resume_id: saved.id,
            });
        }
        if entries.len() == query.limit || page.next_cursor.is_none() {
            return Ok(entries);
        }
        if page.next_cursor == cursor
            || page
                .next_cursor
                .as_ref()
                .is_some_and(|value| value.len() > 4096)
        {
            return Err("Codex history cursor is invalid".to_owned());
        }
        cursor = page.next_cursor;
    }
    Err("Codex history listing exceeds eight pages".to_owned())
}

pub fn query(query: &TerminalHistoryQuery<'_>) -> Result<Vec<TerminalHistoryEntry>, String> {
    let mut command = Command::new(query.program);
    terminal_process::configure_group(&mut command);
    command
        .args(["app-server", "--listen", "stdio://"])
        .env("CODEX_HOME", query.account_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = query.cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Codex history executable could not be started")?;
    let pipes = child.stdin.take().zip(child.stdout.take());
    let result = if let Some((mut writer, stdout)) = pipes {
        let provider = query.provider;
        let account_directory = query.account_directory.to_owned();
        let cwd = query.cwd.map(std::path::Path::to_owned);
        let program = query.program.to_owned();
        let limit = query.limit;
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let query = TerminalHistoryQuery {
                provider,
                program: &program,
                account_directory: &account_directory,
                cwd: cwd.as_deref(),
                limit,
            };
            let result = list(&query, &mut writer, &mut BufReader::new(stdout));
            let _ = sender.send(result);
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "Codex history query timed out".to_owned())
            .and_then(std::convert::identity)
    } else {
        Err("Codex history transport is unavailable".to_owned())
    };
    terminal_process::terminate_group(&mut child);
    result
}
