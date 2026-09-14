//! Bounded recognition over published terminal cells; this module performs no host I/O.
use crate::{
    geometry::GridPoint,
    selection::{SelectionPoint, TerminalSelection},
    terminal_frame::{FrameSelection, RenderFrame},
};
use std::fmt::Write as _;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation as _;

const MAX_WRAP_ROWS: u16 = 32;
const MAX_LOGICAL_CHARS: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Url(String),
    File {
        path: String,
        line: Option<u32>,
        column: Option<u32>,
    },
}

impl LinkTarget {
    #[must_use]
    pub fn location(&self) -> String {
        match self {
            Self::Url(url) => url.clone(),
            Self::File { path, line, column } => {
                let mut value = path.clone();
                if let Some(line) = line {
                    let _ = write!(value, ":{line}");
                }
                if let Some(column) = column {
                    let _ = write!(value, ":{column}");
                }
                value
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameLink {
    pub target: LinkTarget,
    pub segments: Vec<FrameSelection>,
}

struct LogicalLine {
    chars: Vec<char>,
    positions: Vec<(u16, u16, u16)>,
    hyperlinks: Vec<Option<String>>,
    clicked: usize,
}

impl LogicalLine {
    fn at(frame: &RenderFrame, point: GridPoint) -> Option<Self> {
        if point.x >= frame.cols || point.y >= frame.rows {
            return None;
        }
        let wraps = |row: u16| {
            frame
                .row_wraps
                .get(usize::from(row))
                .copied()
                .unwrap_or(false)
        };
        let mut start = point.y;
        while start > 0
            && point.y.saturating_sub(start) < MAX_WRAP_ROWS
            && wraps(start.saturating_sub(1))
        {
            start = start.saturating_sub(1);
        }
        let mut end = point.y;
        while end.saturating_add(1) < frame.rows
            && end.saturating_sub(start) < MAX_WRAP_ROWS
            && wraps(end)
        {
            end = end.saturating_add(1);
        }
        if (start > 0 && wraps(start.saturating_sub(1)))
            || (end.saturating_add(1) < frame.rows && wraps(end))
        {
            return None;
        }
        let mut rows = vec![
            vec![None; usize::from(frame.cols)];
            usize::from(end.saturating_sub(start).saturating_add(1))
        ];
        for cell in &frame.cells {
            if cell.y >= start
                && cell.y <= end
                && cell.x < frame.cols
                && !cell.style.invisible
                && cell.text_len > 0
            {
                *rows
                    .get_mut(usize::from(cell.y.checked_sub(start)?))?
                    .get_mut(usize::from(cell.x))? = Some(cell);
            }
        }
        let mut line = Self {
            chars: Vec::new(),
            positions: Vec::new(),
            hyperlinks: Vec::new(),
            clicked: usize::MAX,
        };
        for (offset, row) in rows.iter().enumerate() {
            let y = start.checked_add(u16::try_from(offset).ok()?)?;
            let mut col = 0;
            while col < row.len() {
                let cell = row.get(col)?;
                let chars = cell.map_or(&[' '][..], |cell| frame.cell_text(cell));
                let (_, width) = libghostty_vt::unicode::grapheme_width(chars);
                let width = usize::from(width.max(1));
                let last = col
                    .saturating_add(width)
                    .saturating_sub(1)
                    .min(row.len().saturating_sub(1));
                if y == point.y && usize::from(point.x) >= col && usize::from(point.x) <= last {
                    line.clicked = line.chars.len();
                }
                for ch in chars {
                    if line.chars.len() >= MAX_LOGICAL_CHARS {
                        return None;
                    }
                    line.chars.push(*ch);
                    line.positions
                        .push((y, u16::try_from(col).ok()?, u16::try_from(last).ok()?));
                    line.hyperlinks
                        .push(cell.and_then(|cell| cell.hyperlink.clone()));
                }
                col = col.saturating_add(width);
            }
        }
        (line.clicked < line.chars.len()).then_some(line)
    }

    fn segments(&self, range: Range<usize>) -> Vec<FrameSelection> {
        let mut segments: Vec<FrameSelection> = Vec::new();
        for &(row, start_col, end_col) in self.positions.get(range).unwrap_or_default() {
            if let Some(last) = segments.last_mut()
                && last.row == row
                && start_col <= last.end_col.saturating_add(1)
            {
                last.end_col = last.end_col.max(end_col);
            } else {
                segments.push(FrameSelection {
                    row,
                    start_col,
                    end_col,
                });
            }
        }
        segments
    }

    fn explicit(&self) -> Option<(String, Range<usize>)> {
        let url = self.hyperlinks.get(self.clicked)?.as_ref()?;
        let mut start = self.clicked;
        let mut end = start.saturating_add(1);
        while start > 0 && self.hyperlinks.get(start.saturating_sub(1))?.as_ref() == Some(url) {
            start = start.saturating_sub(1);
        }
        while end < self.chars.len() && self.hyperlinks.get(end)?.as_ref() == Some(url) {
            end = end.saturating_add(1);
        }
        Some((url.clone(), start..end))
    }

    fn detected(&self) -> Option<(LinkTarget, Range<usize>)> {
        let mut start = self.clicked;
        let mut end = start.saturating_add(1);
        let boundary = |c: char| c.is_whitespace() || "\"'<>`".contains(c);
        while start > 0 && !boundary(*self.chars.get(start.saturating_sub(1))?) {
            start = start.saturating_sub(1);
        }
        while end < self.chars.len() && !boundary(*self.chars.get(end)?) {
            end = end.saturating_add(1);
        }
        while start < end && "([{\"'".contains(*self.chars.get(start)?) {
            start = start.saturating_add(1);
        }
        while end > start {
            let ch = *self.chars.get(end.saturating_sub(1))?;
            let unbalanced = match ch {
                ')' => unmatched_close(self.chars.get(start..end)?, '(', ')'),
                ']' => unmatched_close(self.chars.get(start..end)?, '[', ']'),
                '}' => unmatched_close(self.chars.get(start..end)?, '{', '}'),
                _ => false,
            };
            if ",;.!?".contains(ch) || unbalanced {
                end = end.saturating_sub(1);
            } else {
                break;
            }
        }
        if !(start..end).contains(&self.clicked) {
            return None;
        }
        let candidate = parse_location(&self.chars.get(start..end)?.iter().collect::<String>());
        if let Some(target @ LinkTarget::Url(_)) = candidate {
            return Some((target, start..end));
        }
        // Quoted paths may contain spaces; URLs retain their own balanced parentheses.
        if let Some(range) = pair_range(&self.chars, self.clicked)
            && let Some(target) =
                parse_location(&self.chars.get(range.clone())?.iter().collect::<String>())
        {
            return Some((target, range));
        }
        Some((candidate?, start..end))
    }
}

fn unmatched_close(chars: &[char], open: char, close: char) -> bool {
    chars.iter().filter(|&&c| c == close).count() > chars.iter().filter(|&&c| c == open).count()
}

/// A location printed by a program. Host-specific path normalization happens on that host.
pub fn parse_location(value: &str) -> Option<LinkTarget> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    if value.starts_with("http://") || value.starts_with("https://") || value.starts_with("file://")
    {
        return Some(LinkTarget::Url(value.to_owned()));
    }
    let mut path = value;
    let mut line = None;
    let mut column = None;
    if let Some((base, location)) = value
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once('('))
    {
        if let Some((row, col)) = location.split_once(',') {
            line = positive(row);
            column = positive(col);
            if line.is_some() && column.is_some() {
                path = base;
            } else {
                line = None;
                column = None;
            }
        } else if let Some(row) = positive(location) {
            path = base;
            line = Some(row);
        }
    } else if let Some((base, last)) = value.rsplit_once(':')
        && let Some(number) = positive(last)
    {
        path = base;
        line = Some(number);
        if let Some((base, row)) = base.rsplit_once(':')
            && let Some(row) = positive(row)
        {
            path = base;
            column = line;
            line = Some(row);
        }
    }
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let path_like = path.starts_with(['/', '~'])
        || path.contains(['/', '\\'])
        || file_name.rsplit_once('.').is_some_and(|(name, extension)| {
            !name.is_empty()
                && !extension.is_empty()
                && extension.chars().all(char::is_alphanumeric)
        })
        || line.is_some();
    if !path_like || path.contains("://") || path.is_empty() || path.contains('@') {
        return None;
    }
    Some(LinkTarget::File {
        path: path.to_owned(),
        line,
        column,
    })
}

fn positive(value: &str) -> Option<u32> {
    value.parse().ok().filter(|&n| n > 0)
}

#[must_use]
pub fn link_at(frame: &RenderFrame, point: GridPoint) -> Option<FrameLink> {
    let line = LogicalLine::at(frame, point)?;
    let (target, range) = if let Some((url, range)) = line.explicit() {
        (LinkTarget::Url(url), range)
    } else {
        line.detected()?
    };
    Some(FrameLink {
        target,
        segments: line.segments(range),
    })
}

/// Semantic double-click range. Ordinary words keep Ghostty's native selection behavior.
#[must_use]
pub fn semantic_selection_at(frame: &RenderFrame, point: GridPoint) -> Option<TerminalSelection> {
    let line = LogicalLine::at(frame, point)?;
    let range = if let Some((_, range)) = line.explicit() {
        range
    } else if let Some((_, range)) = line.detected() {
        range
    } else if let Some(range) = pair_range(&line.chars, line.clicked) {
        range
    } else {
        let mut start = line.clicked;
        let mut end = start.saturating_add(1);
        while start > 0 && !line.chars.get(start.saturating_sub(1))?.is_whitespace() {
            start = start.saturating_sub(1);
        }
        while end < line.chars.len() && !line.chars.get(end)?.is_whitespace() {
            end = end.saturating_add(1);
        }
        while end > start && ",;.!?:".contains(*line.chars.get(end.saturating_sub(1))?) {
            end = end.saturating_sub(1);
        }
        let token = line.chars.get(start..end)?.iter().collect::<String>();
        if token.contains('@')
            && token
                .split_once('@')
                .is_some_and(|(name, host)| !name.is_empty() && host.contains('.'))
        {
            start..end
        } else if is_cjk(*line.chars.get(line.clicked)?) {
            let text = line.chars.iter().collect::<String>();
            let byte = line
                .chars
                .get(..line.clicked)?
                .iter()
                .map(|c| c.len_utf8())
                .sum::<usize>();
            let (offset, word) = text.unicode_word_indices().find(|(offset, word)| {
                (*offset..offset.saturating_add(word.len())).contains(&byte)
            })?;
            let start = text.get(..offset)?.chars().count();
            start..start.saturating_add(word.chars().count())
        } else {
            return None;
        }
    };
    if range.is_empty() {
        return None;
    }
    let positions = line.positions.get(range)?;
    let &(y, x, _) = positions.first()?;
    let &(end_y, _, end_x) = positions.last()?;
    Some(TerminalSelection::new(
        SelectionPoint::new(x, y),
        SelectionPoint::new(end_x, end_y),
    ))
}

fn is_cjk(c: char) -> bool {
    matches!(u32::from(c),0x1100..=0x11FF|0x2E80..=0x9FFF|0xAC00..=0xD7AF|0xF900..=0xFAFF|0x20000..=0x3134F)
}

fn pair_range(chars: &[char], clicked: usize) -> Option<Range<usize>> {
    let pairs = [
        ('(', ')'),
        ('[', ']'),
        ('{', '}'),
        ('<', '>'),
        ('（', '）'),
        ('「', '」'),
        ('『', '』'),
        ('【', '】'),
        ('“', '”'),
        ('‘', '’'),
        ('\"', '\"'),
        ('\'', '\''),
        ('`', '`'),
    ];
    let mut best: Option<Range<usize>> = None;
    for (open, close) in pairs {
        let mut stack: Vec<usize> = Vec::new();
        for (index, &ch) in chars.iter().enumerate() {
            if ch == close
                && let Some(start) = stack.pop()
            {
                if start <= clicked && clicked <= index && index > start.saturating_add(1) {
                    let range = start.saturating_add(1)..index;
                    if best.as_ref().is_none_or(|best| range.len() < best.len()) {
                        best = Some(range);
                    }
                }
            } else if ch == open {
                stack.push(index);
            }
        }
    }
    best
}
