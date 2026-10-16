//! Source line anchors shared by local changes and pull request reviews.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DiffSide {
    Left,
    Right,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiffLine {
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

impl DiffLine {
    #[must_use]
    pub const fn number(&self, side: DiffSide) -> Option<u32> {
        match side {
            DiffSide::Left => self.old_line,
            DiffSide::Right => self.new_line,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

/// A selection names source lines, never a rendered row or Git diff position.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiffAnchor {
    pub path: String,
    pub side: DiffSide,
    pub start_line: u32,
    pub line: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub previous_path: Option<String>,
    pub hunks: Vec<DiffHunk>,
    /// A missing hosted patch cannot be presented as an empty text file.
    pub patch_unavailable: bool,
}

impl FileDiff {
    /// Decode a unified patch whose filename was supplied separately by the Git owner.
    /// # Errors
    /// Rejects invalid filenames, oversized patches and malformed hunk line counts.
    pub fn parse(
        path: String,
        previous_path: Option<String>,
        contents: Option<&str>,
    ) -> Result<Self, String> {
        validate_path(&path)?;
        if let Some(previous) = &previous_path {
            validate_path(previous)?;
        }
        let mut result = Self {
            path,
            previous_path,
            hunks: Vec::new(),
            patch_unavailable: contents.is_none(),
        };
        let Some(contents) = contents else {
            return Ok(result);
        };
        if contents.len() > 8 * 1024 * 1024 {
            return Err("Diff exceeds 8 MB".into());
        }
        let mut remaining = (0_u32, 0_u32);
        let mut numbers = (0_u32, 0_u32);
        for line in contents.lines() {
            if line.starts_with("@@ ") {
                if remaining != (0, 0) {
                    return Err("Incomplete diff hunk".into());
                }
                let mut fields = line.split_whitespace();
                let _marker = fields.next();
                let old = range(fields.next().ok_or("Missing old hunk range")?, '-')?;
                let new = range(fields.next().ok_or("Missing new hunk range")?, '+')?;
                if fields.next() != Some("@@") {
                    return Err("Invalid diff hunk header".into());
                }
                numbers = (old.0, new.0);
                remaining = (old.1, new.1);
                result.hunks.push(DiffHunk {
                    header: line.into(),
                    lines: Vec::new(),
                });
                continue;
            }
            if line.starts_with("\\ No newline at end of file") {
                continue;
            }
            let Some(hunk) = result.hunks.last_mut() else {
                continue;
            };
            if remaining == (0, 0) {
                continue;
            }
            let (old, new) = match line.as_bytes().first() {
                Some(b' ') => (true, true),
                Some(b'-') => (true, false),
                Some(b'+') => (false, true),
                _ => return Err("Invalid diff hunk line".into()),
            };
            if (old && remaining.0 == 0) || (new && remaining.1 == 0) {
                return Err("Diff hunk line counts do not match".into());
            }
            hunk.lines.push(DiffLine {
                old_line: old.then_some(numbers.0),
                new_line: new.then_some(numbers.1),
                text: line.get(1..).ok_or("Invalid diff line")?.into(),
            });
            if old {
                remaining.0 = remaining.0.checked_sub(1).ok_or("Invalid old hunk count")?;
                numbers.0 = numbers.0.checked_add(1).ok_or("Diff line overflow")?;
            }
            if new {
                remaining.1 = remaining.1.checked_sub(1).ok_or("Invalid new hunk count")?;
                numbers.1 = numbers.1.checked_add(1).ok_or("Diff line overflow")?;
            }
        }
        if remaining != (0, 0) {
            return Err("Incomplete diff hunk".into());
        }
        Ok(result)
    }

    /// Each file's patch in a Git diff of several. Files start at their `diff --git` line, which
    /// no hunk line can look like; a file whose patch doesn't parse is left out.
    #[must_use]
    pub fn parse_all(contents: &str) -> Vec<Self> {
        let mut starts = contents
            .match_indices("diff --git ")
            .map(|(at, _)| at)
            .filter(|at| {
                *at == 0
                    || contents
                        .get(..*at)
                        .is_some_and(|before| before.ends_with('\n'))
            })
            .collect::<Vec<_>>();
        if starts.is_empty() {
            starts.push(0);
        }
        starts
            .iter()
            .enumerate()
            .filter_map(|(index, start)| {
                let end = starts
                    .get(index.saturating_add(1))
                    .copied()
                    .unwrap_or(contents.len());
                let section = contents.get(*start..end)?;
                let header = section.split("\n@@ ").next().unwrap_or(section);
                let named = |prefix: &str| {
                    header
                        .lines()
                        .find_map(|line| line.strip_prefix(prefix))
                        .filter(|path| *path != "/dev/null")
                };
                let path = named("+++ ").or_else(|| named("--- "))?;
                let path = serde_json::from_str::<String>(path).unwrap_or_else(|_| path.to_owned());
                let path = path
                    .strip_prefix("b/")
                    .or_else(|| path.strip_prefix("a/"))
                    .unwrap_or(&path)
                    .to_owned();
                Self::parse(path, None, Some(section)).ok()
            })
            .collect()
    }

    /// A patch of this file's hunks that touch the selected lines, to stage or unstage them.
    #[must_use]
    pub fn hunk_patch(&self, anchor: &DiffAnchor) -> Option<String> {
        if anchor.path != self.path {
            return None;
        }
        let hunks = self
            .hunks
            .iter()
            .filter(|hunk| {
                hunk.lines.iter().any(|line| {
                    line.number(anchor.side)
                        .is_some_and(|number| (anchor.start_line..=anchor.line).contains(&number))
                })
            })
            .collect::<Vec<_>>();
        if hunks.is_empty() {
            return None;
        }
        let mut patch = format!(
            "--- a/{}\n+++ b/{}\n",
            self.previous_path.as_deref().unwrap_or(&self.path),
            self.path
        );
        for hunk in hunks {
            patch.push_str(&hunk.header);
            patch.push('\n');
            for line in &hunk.lines {
                patch.push(match (line.old_line, line.new_line) {
                    (None, _) => '+',
                    (_, None) => '-',
                    _ => ' ',
                });
                patch.push_str(&line.text);
                patch.push('\n');
            }
        }
        Some(patch)
    }

    /// Return exact selected source text, refusing a stale, missing or cross-hunk anchor.
    /// # Errors
    /// The selection must name consecutive lines on one side of one current hunk.
    pub fn quote(&self, anchor: &DiffAnchor) -> Result<String, String> {
        if anchor.path != self.path || anchor.start_line == 0 || anchor.line < anchor.start_line {
            return Err("Invalid code selection".into());
        }
        for hunk in &self.hunks {
            let lines = hunk
                .lines
                .iter()
                .filter_map(|line| line.number(anchor.side).map(|number| (number, line)));
            let selected = lines
                .filter(|(number, _)| (anchor.start_line..=anchor.line).contains(number))
                .collect::<Vec<_>>();
            if selected
                .first()
                .is_some_and(|(number, _)| *number == anchor.start_line)
                && selected
                    .last()
                    .is_some_and(|(number, _)| *number == anchor.line)
                && u32::try_from(selected.len()).ok()
                    == anchor
                        .line
                        .checked_sub(anchor.start_line)
                        .and_then(|count| count.checked_add(1))
            {
                return Ok(selected
                    .into_iter()
                    .map(|(_, line)| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"));
            }
        }
        Err("Selected lines are no longer in this diff".into())
    }
}

fn range(value: &str, prefix: char) -> Result<(u32, u32), String> {
    let value = value.strip_prefix(prefix).ok_or("Invalid diff range")?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start = start.parse().map_err(|_| "Invalid diff range start")?;
    let count = count.parse().map_err(|_| "Invalid diff range count")?;
    if count > 0 && start == 0 {
        return Err("Invalid source line zero".into());
    }
    Ok((start, count))
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 8192
        || path.contains('\0')
        || path.starts_with('/')
        || path.split('/').any(|part| part == "..")
    {
        return Err("Invalid repository file path".into());
    }
    Ok(())
}
