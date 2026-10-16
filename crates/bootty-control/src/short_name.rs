//! Short names people can write for sessions and conversations, like `bootty-7c`.
//!
//! A name is the project's name and the start of a hash of the identity. Any unambiguous start
//! of the hash names the same thing, as a short Git commit id does, so a written name keeps
//! working as others appear.

use std::path::Path;

/// Hash digits a written name needs, at least.
const MINIMUM_DIGITS: usize = 2;
/// Hash digits in a full name.
const DIGITS: usize = 8;
/// Characters of the project's name kept in a short name.
const PROJECT_CHARS: usize = 20;

/// The full name of `identity` in `project`: the project folder's name and eight hex digits of
/// the identity's hash.
#[must_use]
pub fn full_name(project: &Path, identity: &str) -> String {
    let folder = project
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().to_lowercase());
    let mut slug = String::new();
    for character in folder.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(PROJECT_CHARS).collect();
    let slug = slug.trim_end_matches('-');
    format!(
        "{}-{:08x}",
        if slug.is_empty() { "session" } else { slug },
        fnv1a(identity)
    )
}

/// `name` cut to the fewest hash digits, at least two, that no other of `names` starts with.
#[must_use]
pub fn shortest(name: &str, names: &[String]) -> String {
    let Some((project, hash)) = split(name) else {
        return name.to_owned();
    };
    (MINIMUM_DIGITS..DIGITS)
        .map(|digits| hash.get(..digits).unwrap_or(hash))
        .find(|prefix| {
            names.iter().filter(|other| *other != name).all(|other| {
                split(other).is_none_or(|(other_project, other_hash)| {
                    other_project != project || !other_hash.starts_with(prefix)
                })
            })
        })
        .map_or_else(|| name.to_owned(), |prefix| format!("{project}-{prefix}"))
}

/// Whether `written` names the thing called `name`: its project and a start of its hash.
#[must_use]
pub fn names(written: &str, name: &str) -> bool {
    match (split(written), split(name)) {
        (Some((project, prefix)), Some((name_project, hash))) => {
            project == name_project && hash.starts_with(prefix)
        }
        _ => false,
    }
}

/// Whether `text` is written like a short name rather than an issued handle.
#[must_use]
pub fn is_short_name(text: &str) -> bool {
    split(text).is_some()
}

/// The project and hash digits of a name: lowercase letters, digits and hyphens, ending in a
/// hyphen and two to eight hex digits.
fn split(name: &str) -> Option<(&str, &str)> {
    let (project, hash) = name.rsplit_once('-')?;
    let valid = !project.is_empty()
        && project.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && (MINIMUM_DIGITS..=DIGITS).contains(&hash.len())
        && hash
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character));
    valid.then_some((project, hash))
}

/// A hash that stays the same across runs and builds, unlike the standard library's.
fn fnv1a(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}
