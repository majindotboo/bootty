//! Case-insensitive fuzzy matching shared by host-owned file search and UI pickers.

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
