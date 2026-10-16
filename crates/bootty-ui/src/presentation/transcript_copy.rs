//! Keyboard copy mode over a conversation's text: a cursor that moves the way vim's does, and
//! selections by character, line or column, read from the text the transcript shows.

use std::ops::Range;

/// Where the cursor is, as a byte offset, and the column up and down keep aiming for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyCursor {
    pub offset: usize,
    pub column: usize,
}

/// How a selection grows from its anchor to the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopySelection {
    Character,
    Line,
    /// The same columns of every line between them.
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMotion {
    Left,
    Right,
    Up,
    Down,
    /// The next word's start, as `w`.
    WordStart,
    /// This or the previous word's start, as `b`.
    WordBack,
    /// This or the next word's end, as `e`.
    WordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    TextStart,
    TextEnd,
    /// The character on this line, as `f`, `F`, `t` and `T`: forward or back, and on it or next
    /// to it.
    Find {
        target: char,
        forward: bool,
        till: bool,
    },
}

/// A cursor at `offset` of `text`, aiming for its own column.
#[must_use]
pub fn cursor_at(text: &str, offset: usize) -> CopyCursor {
    let offset = offset.min(text.len());
    CopyCursor {
        offset,
        column: column(text, offset),
    }
}

/// The cursor after `motion`, repeated `count` times, in `text`.
#[must_use]
pub fn apply_motion(
    text: &str,
    cursor: CopyCursor,
    motion: CopyMotion,
    count: usize,
) -> CopyCursor {
    let mut offset = cursor.offset.min(text.len());
    let vertical = matches!(motion, CopyMotion::Up | CopyMotion::Down);
    for _ in 0..count.max(1) {
        let next = step(text, offset, cursor.column, motion);
        if next == offset {
            break;
        }
        offset = next;
    }
    CopyCursor {
        offset,
        column: if vertical {
            cursor.column
        } else {
            column(text, offset)
        },
    }
}

fn step(text: &str, offset: usize, aim: usize, motion: CopyMotion) -> usize {
    let start = line_start(text, offset);
    match motion {
        CopyMotion::Left => previous_char(text, offset)
            .filter(|at| *at >= start)
            .unwrap_or(offset),
        CopyMotion::Right => next_char(text, offset)
            .filter(|at| *at < line_end(text, offset))
            .unwrap_or(offset),
        CopyMotion::Down => {
            let end = line_end(text, offset);
            if end >= text.len() {
                offset
            } else {
                at_column(text, end.saturating_add(1), aim)
            }
        }
        CopyMotion::Up => start.checked_sub(1).map_or(offset, |previous| {
            at_column(text, line_start(text, previous), aim)
        }),
        CopyMotion::WordStart => word_start(text, offset),
        CopyMotion::WordBack => word_back(text, offset),
        CopyMotion::WordEnd => word_end(text, offset),
        CopyMotion::LineStart => start,
        CopyMotion::FirstNonBlank => text
            .get(start..line_end(text, offset))
            .and_then(|line| line.find(|c: char| !c.is_whitespace()))
            .map_or(start, |at| start.saturating_add(at)),
        CopyMotion::LineEnd => last_char(text, start, line_end(text, offset)),
        CopyMotion::TextStart => 0,
        CopyMotion::TextEnd => line_start(text, text.len()),
        CopyMotion::Find {
            target,
            forward,
            till,
        } => find_on_line(text, offset, target, forward, till).unwrap_or(offset),
    }
}

fn find_on_line(
    text: &str,
    offset: usize,
    target: char,
    forward: bool,
    till: bool,
) -> Option<usize> {
    let (start, end) = (line_start(text, offset), line_end(text, offset));
    if forward {
        // Till skips a match right next to the cursor, so repeating it moves on.
        let from = next_char(text, offset)?;
        let from = if till {
            next_char(text, from).unwrap_or(from)
        } else {
            from
        };
        let found = from.saturating_add(text.get(from..end)?.find(target)?);
        if till {
            previous_char(text, found)
        } else {
            Some(found)
        }
    } else {
        let to = previous_char(text, offset).filter(|at| *at >= start)?;
        let to = if till {
            previous_char(text, to)
                .filter(|at| *at >= start)
                .unwrap_or(to)
        } else {
            to
        };
        let found = start.saturating_add(text.get(start..next_char(text, to)?)?.rfind(target)?);
        if till {
            next_char(text, found)
        } else {
            Some(found)
        }
    }
}

/// The ranges a selection from `anchor` to `cursor` covers: one for characters and lines, one
/// per line for a block.
#[must_use]
pub fn selected_ranges(
    text: &str,
    anchor: usize,
    cursor: usize,
    selection: CopySelection,
) -> Vec<Range<usize>> {
    let (from, to) = (
        anchor.min(cursor).min(text.len()),
        anchor.max(cursor).min(text.len()),
    );
    match selection {
        CopySelection::Character => {
            std::iter::once(from..next_char(text, to).unwrap_or(to)).collect()
        }
        CopySelection::Line => {
            std::iter::once(line_start(text, from)..line_end(text, to)).collect()
        }
        CopySelection::Block => {
            let (left, right) = {
                let (a, b) = (column(text, anchor), column(text, cursor));
                (a.min(b), a.max(b))
            };
            let mut ranges = Vec::new();
            let mut start = line_start(text, from);
            loop {
                let end = line_end(text, start);
                let line = text.get(start..end).unwrap_or_default();
                let offset_of = |column: usize| {
                    line.char_indices()
                        .nth(column)
                        .map_or(end, |(at, _)| start.saturating_add(at))
                };
                ranges.push(offset_of(left)..offset_of(right.saturating_add(1)));
                if end >= line_end(text, to) || end >= text.len() {
                    break;
                }
                start = end.saturating_add(1);
            }
            ranges
        }
    }
}

/// The text a selection copies; a block's lines apart by line breaks.
#[must_use]
pub fn selected_text(text: &str, ranges: &[Range<usize>]) -> String {
    ranges
        .iter()
        .filter_map(|range| text.get(range.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_start(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .and_then(|before| before.rfind('\n'))
        .map_or(0, |at| at.saturating_add(1))
}

fn line_end(text: &str, offset: usize) -> usize {
    text.get(offset..)
        .and_then(|after| after.find('\n'))
        .map_or(text.len(), |at| offset.saturating_add(at))
}

fn column(text: &str, offset: usize) -> usize {
    text.get(line_start(text, offset)..offset)
        .map_or(0, |before| before.chars().count())
}

/// The offset `column` characters into the line starting at `start`, at most its last character.
fn at_column(text: &str, start: usize, column: usize) -> usize {
    let end = line_end(text, start);
    text.get(start..end)
        .and_then(|line| line.char_indices().nth(column))
        .map_or_else(
            || last_char(text, start, end),
            |(at, _)| start.saturating_add(at),
        )
}

/// The last character of the line `start..end`, or its start when it is empty.
fn last_char(text: &str, start: usize, end: usize) -> usize {
    previous_char(text, end)
        .filter(|at| *at >= start)
        .unwrap_or(start)
}

fn next_char(text: &str, offset: usize) -> Option<usize> {
    let c = text.get(offset..)?.chars().next()?;
    Some(offset.saturating_add(c.len_utf8()))
}

fn previous_char(text: &str, offset: usize) -> Option<usize> {
    let c = text.get(..offset)?.chars().next_back()?;
    Some(offset.saturating_sub(c.len_utf8()))
}

fn char_at(text: &str, offset: usize) -> Option<char> {
    text.get(offset..)?.chars().next()
}

/// Words are runs of letters, digits and underscores, or runs of other visible characters.
#[derive(PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Mark,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Mark
    }
}

fn word_start(text: &str, offset: usize) -> usize {
    let Some(here) = char_at(text, offset).map(class) else {
        return offset;
    };
    let mut at = offset;
    if here != Class::Space {
        while let Some(c) = char_at(text, at).filter(|c| class(*c) == here) {
            at = at.saturating_add(c.len_utf8());
        }
    }
    while let Some(c) = char_at(text, at).filter(|c| c.is_whitespace()) {
        at = at.saturating_add(c.len_utf8());
    }
    if at >= text.len() { offset } else { at }
}

fn word_back(text: &str, offset: usize) -> usize {
    let mut at = offset;
    while let Some(previous) =
        previous_char(text, at).filter(|p| char_at(text, *p).is_some_and(char::is_whitespace))
    {
        at = previous;
    }
    let Some(kind) = previous_char(text, at)
        .and_then(|p| char_at(text, p))
        .map(class)
    else {
        return at;
    };
    while let Some(previous) =
        previous_char(text, at).filter(|p| char_at(text, *p).is_some_and(|c| class(c) == kind))
    {
        at = previous;
    }
    at
}

fn word_end(text: &str, offset: usize) -> usize {
    let Some(mut at) = next_char(text, offset).filter(|at| *at < text.len()) else {
        return offset;
    };
    while let Some(c) = char_at(text, at).filter(|c| c.is_whitespace()) {
        at = at.saturating_add(c.len_utf8());
    }
    let Some(kind) = char_at(text, at).map(class) else {
        return offset;
    };
    while let Some(next) =
        next_char(text, at).filter(|n| char_at(text, *n).is_some_and(|c| class(c) == kind))
    {
        at = next;
    }
    at
}
