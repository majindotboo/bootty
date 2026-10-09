//! Provider history is paged separately from the live transcript and durable attachment echoes.
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::native_protocol::{NativeSessionSnapshot, field};
use crate::native_session::lock;
use crate::{AgentKind, NativeAgentSession, NativeSessionStatus, NativeTranscriptItem};

pub const PAGE_SIZE: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeHistoryPage {
    pub transcript: Vec<NativeTranscriptItem>,
    pub has_older: bool,
    pub has_newer: bool,
    pub at_latest: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct HistoryPosition {
    pub(crate) older: Option<String>,
    pub(crate) newer: Option<String>,
    pub(crate) at_latest: bool,
    pub(crate) pi_leaf: Option<String>,
    pub(crate) codex_legacy: bool,
}

impl NativeAgentSession {
    /// Read adjacent provider-owned history without replacing the live turn or durable echoes.
    /// # Errors
    /// Rejects busy/stopped owners, unsupported providers, stale cursors and malformed pages.
    pub fn read_history(&self, direction: &str) -> Result<NativeHistoryPage, String> {
        let current = self.snapshot();
        let previous = lock(&self.history_window).clone();
        let (position, page) = self.history_page(&current, previous.as_ref(), direction)?;
        self.accept_history_page(&current, previous.as_ref(), position, page)
    }

    pub(crate) fn history_page(
        &self,
        current: &NativeSessionSnapshot,
        previous: Option<&HistoryPosition>,
        direction: &str,
    ) -> Result<(HistoryPosition, NativeHistoryPage), String> {
        if current.status != NativeSessionStatus::Idle {
            return Err("Wait for the current turn before reading earlier history".into());
        }
        if self.config.provider == AgentKind::Pi {
            return self.history_page_pi(current, previous, direction);
        }
        if self.config.provider != AgentKind::Codex {
            return Err("This provider has no paginated history transport".into());
        }
        let thread = current
            .session_id
            .as_deref()
            .ok_or("Agent has no native thread")?;
        if previous.is_some_and(|position| position.codex_legacy) {
            return self.read_legacy_codex_history(current, previous, direction);
        }
        let (cursor, descending) = adjacent_cursor(previous, direction)?;
        let mut limit = PAGE_SIZE;
        let (response, mut history) = loop {
            let response = match self.rpc(
                "thread/items/list",
                json!({
                    "threadId": thread, "cursor": cursor, "limit": limit,
                    "sortDirection": if descending { "desc" } else { "asc" },
                }),
            ) {
                Ok(response) => response,
                Err(error)
                    if serde_json::from_str::<serde_json::Value>(&error)
                        .is_ok_and(|error| field(&error, "code") == -32601) =>
                {
                    return self.read_legacy_codex_history(current, previous, direction);
                }
                Err(error) => return Err(error),
            };
            let data = field(&response, "data")
                .as_array()
                .ok_or("Provider history has no item page")?;
            if data.len() > limit {
                return Err("Provider exceeded the requested history page size".into());
            }
            if let Some(history) = project_page(data, descending)? {
                break (response, history);
            }
            if limit == 1 {
                return Err("This history item exceeds the bounded transcript view".into());
            }
            // Ask the provider for a smaller page so its returned cursor skips no items.
            limit /= 2;
        };
        let next = history_cursor(field(&response, "nextCursor"))?;
        let backwards = history_cursor(field(&response, "backwardsCursor"))?;
        if next.is_some()
            && (field(&response, "data")
                .as_array()
                .is_some_and(Vec::is_empty)
                || next == cursor)
        {
            return Err("Provider history did not advance its cursor".into());
        }
        history.restore_image_references(&current.transcript);
        let at_latest = (descending && cursor.is_none()) || (!descending && next.is_none());
        let position = HistoryPosition {
            older: if descending {
                next.clone()
            } else {
                backwards.clone()
            },
            newer: if at_latest {
                None
            } else if descending {
                backwards
            } else {
                next
            },
            at_latest,
            pi_leaf: None,
            codex_legacy: false,
        };
        let page = NativeHistoryPage {
            transcript: history.transcript,
            has_older: position.older.is_some(),
            has_newer: position.newer.is_some(),
            at_latest,
        };
        Ok((position, page))
    }

    fn read_legacy_codex_history(
        &self,
        current: &NativeSessionSnapshot,
        previous: Option<&HistoryPosition>,
        direction: &str,
    ) -> Result<(HistoryPosition, NativeHistoryPage), String> {
        // Legacy rollouts and fresh forks may not have a paginated store yet.
        // Re-read their provider-owned history within the 16-MiB RPC envelope;
        // switch to provider cursors once Codex admits them to its item store.
        let response = self.rpc(
            "thread/read",
            json!({
                "threadId": current.session_id, "includeTurns": true,
            }),
        )?;
        let thread = field(&response, "thread");
        if field(thread, "id").as_str() != current.session_id.as_deref() {
            return Err("Provider history belongs to a different thread".into());
        }
        let mut data = Vec::new();
        for turn in field(thread, "turns")
            .as_array()
            .ok_or("Provider history has no turns")?
        {
            for item in field(turn, "items")
                .as_array()
                .ok_or("History turn has no items")?
            {
                data.push(json!({
                    "item": item, "startedAtMs": field(turn, "startedAtMs"),
                    "completedAtMs": field(turn, "completedAtMs"),
                }));
            }
        }
        let descending = direction != "newer";
        let offset = match direction {
            "latest" => data.len(),
            "older" | "newer" => {
                let cursor = previous
                    .and_then(|position| {
                        if descending {
                            position.older.as_deref()
                        } else {
                            position.newer.as_deref()
                        }
                    })
                    .ok_or("There is no adjacent history page")?;
                legacy_history_offset(cursor, descending, &data)?
            }
            _ => return Err("History direction must be older, newer or latest".into()),
        };
        let mut limit = PAGE_SIZE;
        let (start, end, mut history) = loop {
            let (start, end) = if descending {
                (offset.saturating_sub(limit), offset)
            } else {
                (offset, offset.saturating_add(limit).min(data.len()))
            };
            let slice = data
                .get(start..end)
                .ok_or("The history page changed during the read")?;
            if let Some(history) = project_page(slice, false)? {
                break (start, end, history);
            }
            if limit == 1 {
                return Err("This history item exceeds the bounded transcript view".into());
            }
            limit /= 2;
        };
        history.restore_image_references(&current.transcript);
        let position = HistoryPosition {
            older: data
                .get(start)
                .filter(|_| start > 0)
                .map(|item| legacy_history_cursor(start, item)),
            newer: data
                .get(end.saturating_sub(1))
                .filter(|_| end < data.len())
                .map(|item| legacy_history_cursor(end, item)),
            at_latest: end == data.len(),
            pi_leaf: None,
            codex_legacy: true,
        };
        let page = NativeHistoryPage {
            transcript: history.transcript,
            has_older: position.older.is_some(),
            has_newer: position.newer.is_some(),
            at_latest: position.at_latest,
        };
        Ok((position, page))
    }

    pub(crate) fn accept_history_page(
        &self,
        current: &NativeSessionSnapshot,
        previous: Option<&HistoryPosition>,
        position: HistoryPosition,
        page: NativeHistoryPage,
    ) -> Result<NativeHistoryPage, String> {
        let current_after_read = lock(&self.snapshot);
        if current_after_read.status != NativeSessionStatus::Idle
            || current_after_read.session_id != current.session_id
        {
            return Err("The conversation changed while reading history".into());
        }
        let mut window = lock(&self.history_window);
        if window.as_ref() != previous {
            return Err("The displayed history page changed during the read".into());
        }
        // Keep the existing 256-item/1-MiB text bound for quotes across nearby pages.
        // Reload an evicted page to quote it; a larger review needs explicit quote pinning.
        let mut quotes = lock(&self.history_quotes);
        for item in page
            .transcript
            .iter()
            .filter(|item| item.role == "assistant")
        {
            quotes.message(
                item.id.clone(),
                &item.role,
                item.text.clone(),
                item.complete,
            );
        }
        *window = Some(position);
        drop(quotes);
        drop(window);
        drop(current_after_read);
        Ok(page)
    }

    pub(crate) fn validate_history_citation(
        &self,
        citation: &crate::NativeResponseCitation,
    ) -> Result<(), String> {
        let live = self.snapshot();
        if live
            .transcript
            .iter()
            .any(|item| item.id == citation.message_id)
        {
            return citation.validate(&live.transcript);
        }
        citation.validate(&lock(&self.history_quotes).transcript)
    }

    pub(crate) fn history_through(
        &self,
        response: &str,
    ) -> Result<Vec<NativeTranscriptItem>, String> {
        let current = self.snapshot();
        let mut previous = None;
        let mut transcript = Vec::new();
        let mut found = false;
        // Bound provider round trips independently of the 2-MiB copied-context limit.
        // Larger lookbacks need a provider-owned bounded range export.
        for _ in 0..64 {
            let (position, page) = self.history_page(
                &current,
                previous.as_ref(),
                if previous.is_none() {
                    "latest"
                } else {
                    "older"
                },
            )?;
            let mut prefix = page.transcript;
            if !found {
                if let Some(end) = prefix.iter().position(|item| {
                    item.id == response
                        && item.role == "assistant"
                        && item.complete
                        && !item.text.trim().is_empty()
                }) {
                    prefix.truncate(end.saturating_add(1));
                    found = true;
                } else if page.has_older {
                    previous = Some(position);
                    continue;
                } else {
                    return Err("The completed response is no longer in provider history".into());
                }
            }
            prefix.append(&mut transcript);
            transcript = prefix;
            if serde_json::to_vec(&transcript)
                .map_err(|error| error.to_string())?
                .len()
                > 2 * 1024 * 1024
            {
                return Err("The conversation exceeds the side-chat context limit".into());
            }
            if !page.has_older {
                let latest = self.snapshot();
                if latest.session_id != current.session_id
                    || latest.revision != current.revision
                    || latest.status != NativeSessionStatus::Idle
                {
                    return Err("The conversation changed while copying history".into());
                }
                return Ok(transcript);
            }
            previous = Some(position);
        }
        Err("The response exceeds the side-chat history lookback limit".into())
    }
}

fn legacy_history_cursor(offset: usize, entry: &serde_json::Value) -> String {
    format!(
        "{offset}:{}",
        field(field(entry, "item"), "id")
            .as_str()
            .unwrap_or_default()
    )
}

fn legacy_history_offset(
    cursor: &str,
    descending: bool,
    data: &[serde_json::Value],
) -> Result<usize, String> {
    let (offset, id) = cursor
        .split_once(':')
        .ok_or("Legacy history cursor is invalid")?;
    let offset = offset
        .parse::<usize>()
        .map_err(|_| "Legacy history cursor is invalid")?;
    let index = if descending {
        offset
    } else {
        offset
            .checked_sub(1)
            .ok_or("Legacy history cursor is invalid")?
    };
    if data
        .get(index)
        .is_none_or(|entry| field(field(entry, "item"), "id").as_str() != Some(id))
    {
        return Err("Provider history changed around the displayed page".into());
    }
    Ok(offset)
}

fn history_cursor(value: &serde_json::Value) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .filter(|cursor| {
            !cursor.is_empty() && cursor.len() <= 8192 && !cursor.chars().any(char::is_control)
        })
        .map(|cursor| Some(cursor.to_owned()))
        .ok_or_else(|| "Provider history cursor is invalid".into())
}

fn project_page(
    data: &[serde_json::Value],
    descending: bool,
) -> Result<Option<NativeSessionSnapshot>, String> {
    let mut history = NativeSessionSnapshot::new(AgentKind::Codex);
    let mut ids = std::collections::BTreeSet::new();
    let mut expected = std::collections::BTreeSet::new();
    for index in 0..data.len() {
        let entry = data
            .get(if descending {
                data.len().saturating_sub(1).saturating_sub(index)
            } else {
                index
            })
            .ok_or("Provider history index is invalid")?;
        let item = field(entry, "item");
        let id = field(item, "id")
            .as_str()
            .ok_or("History item has no identity")?;
        if id.is_empty() || id.len() > 8192 || !ids.insert(id) {
            return Err("History item identities are invalid or duplicated".into());
        }
        let mut single = NativeSessionSnapshot::new(AgentKind::Codex);
        single.history_item(item);
        expected.extend(single.transcript.into_iter().map(|item| item.id));
        history.with_message_times(
            (
                field(entry, "startedAtMs").as_i64(),
                field(entry, "completedAtMs").as_i64(),
            ),
            |history| history.history_item(item),
        );
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

fn adjacent_cursor(
    previous: Option<&HistoryPosition>,
    direction: &str,
) -> Result<(Option<String>, bool), String> {
    let (cursor, descending) = match direction {
        "latest" => (None, true),
        "older" => (previous.and_then(|position| position.older.clone()), true),
        "newer" => (previous.and_then(|position| position.newer.clone()), false),
        _ => return Err("History direction must be older, newer or latest".into()),
    };
    if direction != "latest" && cursor.is_none() {
        return Err("There is no adjacent history page".into());
    }
    Ok((cursor, descending))
}
