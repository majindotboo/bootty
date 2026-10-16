//! Finding conversations by what was said in them, and quoting them in another's prompt.
use super::{CommandTarget, NativeAgentService, lock};
use std::fmt::Write as _;

/// A conversation whose words match a search, with the line that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSearchMatch {
    pub target: CommandTarget,
    /// The saved session the conversation belongs to, when it has one.
    pub task_identity: Option<String>,
    pub snippet: String,
}

/// How much of a matching line a result shows, in characters.
const SNIPPET_CHARS: usize = 80;
/// How much of one mentioned conversation a prompt quotes, in bytes.
const MENTION_BYTES: usize = 8 * 1024;
/// Conversations one prompt may mention.
pub const MAX_MENTIONED_CONVERSATIONS: usize = 4;

impl NativeAgentService {
    /// Conversations whose prompts or replies mention `query`, ignoring case, each with the first
    /// line that does.
    // Searches the recent transcript each conversation keeps in memory, not its full provider
    // history; reading provider logs per keystroke would cost more than a search should.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<NativeSearchMatch> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let store = lock(&self.store);
        store
            .records
            .iter()
            .filter_map(|record| {
                let snippet = record
                    .snapshot
                    .transcript
                    .iter()
                    .filter(|item| matches!(item.role.as_str(), "user" | "assistant"))
                    .find_map(|item| matching_line(&item.text, &needle))?;
                Some(NativeSearchMatch {
                    target: record.target(),
                    task_identity: record.task_identity.clone(),
                    snippet,
                })
            })
            .collect()
    }
}

impl NativeAgentService {
    /// The recent words of each conversation `targets` mention, for the provider to read with
    /// the message that mentions them: its title and short name, then its latest prompts and
    /// replies, oldest first, up to 8 KiB each.
    /// # Errors
    /// Rejects more than four mentions, and a mentioned conversation that changed since.
    pub fn conversation_context(&self, targets: &[CommandTarget]) -> Result<String, String> {
        if targets.len() > MAX_MENTIONED_CONVERSATIONS {
            return Err("Mention at most four conversations in one message".to_owned());
        }
        let store = lock(&self.store);
        let names: Vec<String> = store
            .records
            .iter()
            .map(super::NativeSessionRecord::full_name)
            .collect();
        let mut context = String::new();
        for target in targets {
            let (record, name) = store
                .records
                .iter()
                .zip(&names)
                .find(|(record, _)| record.target() == *target)
                .ok_or("A mentioned conversation changed; mention it again")?;
            let mut lines = Vec::new();
            let mut bytes = 0_usize;
            let said = record
                .snapshot
                .transcript
                .iter()
                .rev()
                .filter(|item| matches!(item.role.as_str(), "user" | "assistant"));
            for item in said {
                let speaker = if item.role == "user" {
                    "User"
                } else {
                    "Assistant"
                };
                let line = format!("{speaker}: {}", item.text.trim());
                let total = bytes.saturating_add(line.len());
                if total > MENTION_BYTES {
                    // The latest message alone may be longer than the quote; keep its end.
                    if lines.is_empty() {
                        let start =
                            line.ceil_char_boundary(line.len().saturating_sub(MENTION_BYTES));
                        lines.push(format!(
                            "{speaker}: …{}",
                            line.get(start..).unwrap_or_default()
                        ));
                    }
                    break;
                }
                bytes = total;
                lines.push(line);
            }
            lines.reverse();
            let short = bootty_control::short_name::shortest(name, &names);
            if !context.is_empty() {
                context.push_str("\n\n");
            }
            _ = write!(
                context,
                "The conversation “{}” (@{short}), mentioned above, said, oldest first:\n{}",
                record.title,
                lines.join("\n")
            );
        }
        drop(store);
        Ok(context)
    }
}

/// The line of `text` holding `needle`, which is lowercase, cut to about `SNIPPET_CHARS`
/// characters around it.
fn matching_line(text: &str, needle: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let lower = line.to_lowercase();
        let at = lower.find(needle)?;
        let before = lower.get(..at).map_or(0, |prefix| prefix.chars().count());
        let length = line.chars().count();
        if length <= SNIPPET_CHARS {
            return Some(line.to_owned());
        }
        // Keep some words before the match so it reads as a sentence.
        let start = before
            .saturating_sub(SNIPPET_CHARS / 4)
            .min(length.saturating_sub(SNIPPET_CHARS));
        let end = start.saturating_add(SNIPPET_CHARS).min(length);
        let window: String = line
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        // A cut never leaves part of a word.
        let window = if start > 0 {
            window
                .split_once(char::is_whitespace)
                .map_or("", |(_, rest)| rest)
        } else {
            &window
        };
        let window = if end < length {
            window
                .rsplit_once(char::is_whitespace)
                .map_or("", |(kept, _)| kept)
        } else {
            window
        };
        Some(format!(
            "{}{}{}",
            if start > 0 { "…" } else { "" },
            window.trim(),
            if end < length { "…" } else { "" },
        ))
    })
}
