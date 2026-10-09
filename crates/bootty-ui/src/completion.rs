//! Composer trigger semantics, shared by new-session and conversation editors.
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Command,
    Skill,
    Mention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionTrigger {
    pub kind: CompletionKind,
    pub range: Range<usize>,
    pub query: String,
}

impl CompletionTrigger {
    /// Names support fuzzy matching; descriptions only support contiguous search.
    /// Higher scores win. An empty query preserves the provider's catalog order.
    #[must_use]
    pub fn score(&self, name: &str, description: &str) -> Option<i32> {
        if self.query.is_empty() {
            return Some(0);
        }
        let name = name.trim_start_matches(['/', '$', '@']);
        let query = self.query.to_lowercase();
        let name = name.to_lowercase();
        let primary =
            crate::product_dialogs::searchable::fuzzy_match(&name, &query).map(|matched| {
                matched
                    .score
                    .saturating_add(if name.contains(&query) { 20_000 } else { 0 })
            });
        let secondary = description
            .to_lowercase()
            .contains(&query)
            .then_some(20_000);
        primary.into_iter().chain(secondary).max()
    }

    /// Read a trigger at a UTF-8 source cursor; selections, emails and completed tokens do not open it.
    #[must_use]
    pub fn at(text: &str, selection: Range<usize>) -> Option<Self> {
        if !selection.is_empty() {
            return None;
        }
        let before = text.get(..selection.start)?;
        let after = text.get(selection.start..)?;
        let start = before
            .char_indices()
            .rfind(|(_, character)| character.is_whitespace())
            .map_or(0, |(start, character)| {
                start.saturating_add(character.len_utf8())
            });
        let token = before.get(start..)?;
        let kind = match token.as_bytes().first()? {
            b'/' if !before.get(..start)?.contains('\n')
                && before.get(..start)?.trim().is_empty() =>
            {
                CompletionKind::Command
            }
            b'$' => CompletionKind::Skill,
            b'@' => CompletionKind::Mention,
            _ => return None,
        };
        let query = token.get(1..)?;
        if query.starts_with(['/', '$', '@']) {
            return None;
        }
        let end = selection
            .start
            .saturating_add(after.find(char::is_whitespace).unwrap_or(after.len()));
        Some(Self {
            kind,
            range: start..end,
            query: query.into(),
        })
    }
}
