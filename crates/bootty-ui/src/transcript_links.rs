//! A reply's inline code that names a file becomes a link, so `src/main.rs:12` opens like a path
//! in the terminal. The inserted link marks are recorded, so a selection still maps to the reply's
//! own text.

use std::ops::Range;

use bootty_terminal::terminal_links::{LinkTarget, parse_location};

/// Where link marks were inserted: each entry is a start in the linked text and its length.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LinkMarks(Vec<(usize, usize)>);

impl LinkMarks {
    /// The reply's own byte range for a range of the linked text. An edge inside a link mark moves
    /// to the mark's place in the reply.
    #[must_use]
    pub fn source_range(&self, range: Range<usize>) -> Range<usize> {
        self.source_offset(range.start)..self.source_offset(range.end)
    }

    fn source_offset(&self, offset: usize) -> usize {
        let mut removed = 0_usize;
        for &(start, length) in &self.0 {
            if offset <= start {
                break;
            }
            removed = removed.saturating_add(length.min(offset.saturating_sub(start)));
        }
        offset.saturating_sub(removed)
    }
}

/// Extensions that make bare inline code a file name; others (`self.record`) stay code.
const FILE_EXTENSIONS: &[&str] = &[
    "bash", "c", "cc", "cfg", "cjs", "conf", "cpp", "cs", "css", "csv", "fish", "ftl", "go",
    "gradle", "graphql", "h", "hpp", "html", "ini", "java", "js", "json", "jsonl", "jsx", "kt",
    "lock", "lua", "m", "md", "mdx", "mjs", "mm", "php", "proto", "py", "rb", "rs", "scss", "sh",
    "sql", "svelte", "swift", "toml", "ts", "tsx", "txt", "vue", "xml", "yaml", "yml", "zig",
    "zsh",
];

/// Links each single-backtick code span that names a file. Fenced code stays as written.
#[must_use]
pub fn link_code_paths(markdown: &str) -> (String, LinkMarks) {
    let mut linked = String::with_capacity(markdown.len());
    let mut marks = Vec::new();
    if !markdown.contains('`') {
        linked.push_str(markdown);
        return (linked, LinkMarks(marks));
    }
    let mut fence: Option<&str> = None;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            linked.push_str(line);
        } else if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = trimmed.get(..3);
            linked.push_str(line);
        } else {
            link_line(line, &mut linked, &mut marks);
        }
    }
    (linked, LinkMarks(marks))
}

fn link_line(line: &str, linked: &mut String, marks: &mut Vec<(usize, usize)>) {
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let (before, from_tick) = rest.split_at(open);
        linked.push_str(before);
        let after_ticks = from_tick.trim_start_matches('`');
        let ticks = from_tick.len().saturating_sub(after_ticks.len());
        if ticks > 1 {
            // A longer code span ends at the same run; copy it whole.
            let run = from_tick.get(..ticks).unwrap_or_default();
            let end = after_ticks
                .find(run)
                .map_or(after_ticks.len(), |end| end.saturating_add(ticks));
            let (span, after) = after_ticks.split_at(end);
            linked.push_str(run);
            linked.push_str(span);
            rest = after;
            continue;
        }
        let Some((code, after)) = after_ticks.split_once('`') else {
            linked.push_str(from_tick);
            return;
        };
        if !(before.ends_with('[') && after.starts_with("](")) && names_file(code) {
            marks.push((linked.len(), 1));
            linked.push_str("[`");
            linked.push_str(code);
            linked.push('`');
            let destination = format!("](<{code}>)");
            marks.push((linked.len(), destination.len()));
            linked.push_str(&destination);
        } else {
            linked.push('`');
            linked.push_str(code);
            linked.push('`');
        }
        rest = after;
    }
    linked.push_str(rest);
}

/// Whether inline code is a file location: it has a directory, or a file extension that is
/// either well known or followed by a line number.
fn names_file(code: &str) -> bool {
    if code.len() > 512
        || code.contains(char::is_whitespace)
        || code.contains(['<', '>', '(', ')', '[', ']', '`'])
    {
        return false;
    }
    let Some(LinkTarget::File { path, line, .. }) = parse_location(code) else {
        return false;
    };
    let file_name = path.rsplit('/').next().unwrap_or(&path);
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| extension.chars().any(char::is_alphabetic));
    path.contains('/')
        || extension.is_some_and(|extension| {
            line.is_some() || FILE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        })
}
