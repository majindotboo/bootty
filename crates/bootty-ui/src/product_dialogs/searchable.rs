//! Generic state for a searchable, selectable dialog list.

/// One accepted item supplied by the product owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableEntry<T> {
    pub value: T,
    pub primary: String,
    pub secondary: Option<String>,
    pub trailing: Option<String>,
    pub keywords: Vec<String>,
    pub enabled: bool,
}

impl<T> SearchableEntry<T> {
    #[must_use]
    pub fn new(value: T, primary: impl Into<String>) -> Self {
        Self {
            value,
            primary: primary.into(),
            secondary: None,
            trailing: None,
            keywords: Vec::new(),
            enabled: true,
        }
    }
}

/// Match metadata needed by any host renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableRow<'a, T> {
    pub source_index: usize,
    pub value: &'a T,
    pub primary: &'a str,
    pub secondary: Option<&'a str>,
    pub trailing: Option<&'a str>,
    pub primary_matches: Vec<usize>,
    pub secondary_matches: Vec<usize>,
    pub trailing_matches: Vec<usize>,
    pub enabled: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchableIntent {
    SetFilter(String),
    Select(usize),
    MoveNext,
    MovePrevious,
}

/// Filter and keyboard-selection state. The accepted entries remain immutable until replaced by
/// their owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableList<T> {
    entries: Vec<SearchableEntry<T>>,
    filter: String,
    selected: usize,
}

impl<T> SearchableList<T> {
    #[must_use]
    pub fn new(entries: Vec<SearchableEntry<T>>) -> Self {
        let mut list = Self {
            entries,
            filter: String::new(),
            selected: 0,
        };
        list.clamp_selection();
        list
    }

    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    #[must_use]
    pub const fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn replace_entries(&mut self, entries: Vec<SearchableEntry<T>>) {
        self.entries = entries;
        self.clamp_selection();
    }

    pub fn apply(&mut self, intent: SearchableIntent) {
        match intent {
            SearchableIntent::SetFilter(filter) => {
                self.filter = filter;
                // Filtering can reorder fuzzy matches. A fresh filter always
                // selects the first enabled visible item, matching the host
                // picker and making Enter deterministic.
                self.selected = 0;
                self.clamp_selection();
            }
            SearchableIntent::Select(index) => {
                let matches = self.matches();
                if matches
                    .get(index)
                    .is_some_and(|matched| matched.entry.enabled)
                {
                    self.selected = index;
                }
            }
            SearchableIntent::MoveNext => self.move_selection(true),
            SearchableIntent::MovePrevious => self.move_selection(false),
        }
    }

    #[must_use]
    pub fn rows(&self) -> Vec<SearchableRow<'_, T>> {
        self.matches()
            .into_iter()
            .enumerate()
            .map(|(visible_index, matched)| {
                let entry = matched.entry;
                SearchableRow {
                    source_index: matched.source_index,
                    value: &entry.value,
                    primary: &entry.primary,
                    secondary: entry.secondary.as_deref(),
                    trailing: entry.trailing.as_deref(),
                    primary_matches: matched.primary_indices,
                    secondary_matches: matched.secondary_indices,
                    trailing_matches: matched.trailing_indices,
                    enabled: entry.enabled,
                    selected: visible_index == self.selected,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn selected_value(&self) -> Option<&T> {
        let entry = self.matches().get(self.selected)?.entry;
        entry.enabled.then_some(&entry.value)
    }

    fn clamp_selection(&mut self) {
        let matches = self.matches();
        let selected = self.selected.min(matches.len().saturating_sub(1));
        self.selected = first_enabled(&matches, selected).unwrap_or(0);
    }

    fn move_selection(&mut self, forward: bool) {
        let matches = self.matches();
        if matches.is_empty() {
            self.selected = 0;
            return;
        }
        let last = matches.len().saturating_sub(1);
        let mut next = self.selected.min(last);
        for _ in &matches {
            next = if forward {
                if next == last {
                    0
                } else {
                    next.saturating_add(1)
                }
            } else {
                next.checked_sub(1).unwrap_or(last)
            };
            if matches
                .get(next)
                .is_some_and(|matched| matched.entry.enabled)
            {
                self.selected = next;
                return;
            }
        }
    }

    fn matches(&self) -> Vec<MatchedEntry<'_, T>> {
        let filter = self.filter.trim();
        let mut matches = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(source_index, entry)| match_entry(source_index, entry, filter))
            .collect::<Vec<_>>();
        matches.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.source_index.cmp(&b.source_index))
        });
        matches
    }
}

fn first_enabled<T>(matches: &[MatchedEntry<'_, T>], start: usize) -> Option<usize> {
    matches
        .iter()
        .enumerate()
        .skip(start)
        .chain(matches.iter().enumerate().take(start))
        .find_map(|(index, matched)| matched.entry.enabled.then_some(index))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchedEntry<'a, T> {
    entry: &'a SearchableEntry<T>,
    source_index: usize,
    score: i32,
    primary_indices: Vec<usize>,
    secondary_indices: Vec<usize>,
    trailing_indices: Vec<usize>,
}

fn match_entry<'a, T>(
    source_index: usize,
    entry: &'a SearchableEntry<T>,
    filter: &str,
) -> Option<MatchedEntry<'a, T>> {
    if filter.is_empty() {
        return Some(MatchedEntry {
            entry,
            source_index,
            score: 0,
            primary_indices: Vec::new(),
            secondary_indices: Vec::new(),
            trailing_indices: Vec::new(),
        });
    }
    let primary = fuzzy_match(&entry.primary, filter);
    let secondary = entry
        .secondary
        .as_deref()
        .and_then(|value| fuzzy_match(value, filter));
    let trailing = entry
        .trailing
        .as_deref()
        .and_then(|value| fuzzy_match(value, filter));
    let keyword_score = entry
        .keywords
        .iter()
        .filter_map(|value| {
            fuzzy_match(value, filter).map(|matched| matched.score.saturating_add(3_000))
        })
        .max();
    let score = primary
        .as_ref()
        .map(|matched| matched.score.saturating_add(5_000))
        .into_iter()
        .chain(
            secondary
                .as_ref()
                .map(|matched| matched.score.saturating_add(1_000)),
        )
        .chain(
            trailing
                .as_ref()
                .map(|matched| matched.score.saturating_add(3_000)),
        )
        .chain(keyword_score)
        .max()?;
    Some(MatchedEntry {
        entry,
        source_index,
        score,
        primary_indices: primary.map_or_else(Vec::new, |matched| matched.indices),
        secondary_indices: secondary.map_or_else(Vec::new, |matched| matched.indices),
        trailing_indices: trailing.map_or_else(Vec::new, |matched| matched.indices),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    pub indices: Vec<usize>,
}

/// Case-insensitive subsequence matching shared by host-neutral dialog models.
#[must_use]
pub fn fuzzy_matches(candidate: &str, pattern: &str) -> bool {
    fuzzy_match(candidate, pattern).is_some()
}

#[must_use]
pub fn fuzzy_match(candidate: &str, pattern: &str) -> Option<FuzzyMatch> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }
    let candidate_chars = candidate.chars().collect::<Vec<_>>();
    let candidate_lower = candidate.to_ascii_lowercase();
    let pattern_lower = pattern.to_ascii_lowercase();
    let mut indices = Vec::with_capacity(pattern_lower.chars().count());
    let mut search_from = 0;
    for needle in pattern_lower.chars() {
        let found = candidate_chars
            .iter()
            .enumerate()
            .skip(search_from)
            .find_map(|(index, character)| {
                character
                    .to_lowercase()
                    .any(|candidate| candidate == needle)
                    .then_some(index)
            })?;
        indices.push(found);
        search_from = found.saturating_add(1);
    }
    let first = i32::try_from(*indices.first()?).unwrap_or(i32::MAX);
    let last = i32::try_from(*indices.last()?).unwrap_or(i32::MAX);
    let gaps = indices
        .array_windows::<2>()
        .fold(0_usize, |total, [first, last]| {
            total.saturating_add(last.saturating_sub(*first).saturating_sub(1))
        });
    let gaps = i32::try_from(gaps).unwrap_or(i32::MAX);
    let mut score = 10_000_i32
        .saturating_sub(first.saturating_mul(25))
        .saturating_sub(gaps.saturating_mul(12))
        .saturating_sub(last.saturating_sub(first).saturating_mul(4));
    if let Some(byte_index) = candidate_lower.find(&pattern_lower) {
        let char_index =
            i32::try_from(candidate_lower.get(..byte_index)?.chars().count()).unwrap_or(i32::MAX);
        score = score.saturating_add(5_000_i32.saturating_sub(char_index.saturating_mul(20)));
    }
    if candidate_lower
        .match_indices(&pattern_lower)
        .any(|(index, _)| {
            index == 0
                || candidate_lower
                    .as_bytes()
                    .get(index.saturating_sub(1))
                    .is_none_or(|byte| !byte.is_ascii_alphanumeric())
        })
    {
        score = score.saturating_add(1_500);
    }
    if candidate_lower == pattern_lower {
        score = score.saturating_add(10_000);
    }
    Some(FuzzyMatch { score, indices })
}
