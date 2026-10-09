use std::{collections::HashSet, path::Path};

pub const SESSION_NAME_MAX_BYTES: usize = 256;

/// Keep generated names valid across native, rmux, and tmux backends.
#[cfg(feature = "terminal-runtime")]
pub(crate) fn generated_session_name(candidate: &str) -> String {
    let mut name: String = candidate
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, ':' | '.' | '\\' | '#') {
                '_'
            } else {
                character
            }
        })
        .collect();
    if name.starts_with(['-', '$', '@', '%', '=']) {
        name.insert(0, '_');
    }
    truncate_session_name(&mut name, SESSION_NAME_MAX_BYTES);
    if name.is_empty() {
        "bootty".clone_into(&mut name);
    }
    name
}

fn truncate_session_name(name: &mut String, max_bytes: usize) {
    let mut end = name.len().min(max_bytes);
    while !name.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    name.truncate(end);
}

/// Derive the default session name for a local path.
#[must_use]
pub fn session_name_for_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bootty")
        .trim_end_matches(".git")
        .to_owned()
}

/// Derive the default session name for a path reported by a remote host.
#[must_use]
pub fn session_name_for_remote_path(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|name| !name.is_empty() && !name.ends_with(':'))
        .unwrap_or("bootty")
        .trim_end_matches(".git")
        .to_owned()
}

/// Choose a session name that is unique among the names already in use.
pub fn unique_session_name<'a, I>(candidate: &str, existing: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let existing = existing.into_iter().collect::<HashSet<_>>();
    if !existing.contains(candidate) {
        return candidate.to_owned();
    }

    let (group, leaf) = candidate.rsplit_once('/').unwrap_or(("", candidate));
    let base = if group.is_empty() { leaf } else { candidate };
    // u128 has more suffixes than any addressable set can contain.
    let mut suffix = 2_u128;
    loop {
        let tail = format!("-{suffix}");
        let mut name = base.to_owned();
        truncate_session_name(&mut name, SESSION_NAME_MAX_BYTES.saturating_sub(tail.len()));
        name.push_str(&tail);
        if !existing.contains(name.as_str()) {
            return name;
        }
        suffix = suffix.saturating_add(1);
    }
}

/// Whether `name` is the base name or a numeric uniqueness suffix of `base`.
#[must_use]
pub fn is_uniquified_session_name(name: &str, base: &str) -> bool {
    if name == base {
        return true;
    }
    let (group, leaf) = base.rsplit_once('/').unwrap_or(("", base));
    let Some(candidate_leaf) = name
        .strip_prefix(group)
        .and_then(|rest| rest.strip_prefix(if group.is_empty() { "" } else { "/" }))
    else {
        return false;
    };
    candidate_leaf
        .strip_prefix(leaf)
        .and_then(|suffix| suffix.strip_prefix('-'))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.chars().all(|char| char.is_ascii_digit())
        })
}
