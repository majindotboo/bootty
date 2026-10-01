use std::{collections::HashSet, path::Path};

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
    // u128 has more suffixes than any addressable set can contain.
    let mut suffix = 2_u128;
    loop {
        let suffixed_leaf = format!("{leaf}-{suffix}");
        let name = if group.is_empty() {
            suffixed_leaf
        } else {
            format!("{group}/{suffixed_leaf}")
        };
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

/// Make a generated label portable across exact backend targets, with room for a uniqueness suffix.
#[must_use]
pub fn portable_session_name(label: &str) -> String {
    let label = rmux_proto::SessionName::new(label.to_owned())
        .map_or_else(|_| "bootty".to_owned(), rmux_proto::SessionName::into_inner);
    // unique_session_name uses u128 suffixes: reserve the dash and at most 39 decimal digits.
    let limit = crate::workspace::SESSION_NAME_MAX_BYTES.saturating_sub(40);
    let mut name = String::new();
    for character in label.chars() {
        let character = if invalid_session_name_character(character) {
            '_'
        } else {
            character
        };
        // A leading slash would become an empty group in unique_session_name.
        if name.is_empty() && (reserved_session_name_start(character) || character == '/') {
            name.push('_');
        }
        if name.len().saturating_add(character.len_utf8()) > limit {
            break;
        }
        name.push(character);
    }
    if name.is_empty() {
        name.push_str("bootty");
    }
    name
}

pub(crate) const fn invalid_session_name_character(character: char) -> bool {
    character.is_control() || matches!(character, ':' | '.' | '\\' | '#')
}

pub(crate) const fn reserved_session_name_start(character: char) -> bool {
    matches!(character, '-' | '$' | '@' | '%' | '=')
}
