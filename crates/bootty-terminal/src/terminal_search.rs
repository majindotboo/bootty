use std::ops::Range;

use regex::{Regex, RegexBuilder};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalSearchOptions {
    pub regex: bool,
    pub case_sensitive: bool,
}

#[derive(Debug)]
pub(crate) struct SearchPattern {
    regex: Regex,
    overlap: bool,
}

impl SearchPattern {
    pub(crate) fn new(query: &str, options: TerminalSearchOptions) -> Result<Self, regex::Error> {
        let expression = if options.regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        RegexBuilder::new(&expression)
            .case_insensitive(!options.case_sensitive)
            .build()
            .map(|regex| Self {
                regex,
                overlap: !options.regex,
            })
    }

    /// Character ranges keep UTF-8 byte offsets out of terminal cell coordinates.
    /// Empty matches have no cells to highlight; literal overlapping matches remain navigable.
    pub(crate) fn ranges(&self, logical: &[char]) -> Vec<Range<usize>> {
        let text: String = logical.iter().collect();
        let offsets: Vec<_> = text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([text.len()])
            .collect();
        let mut ranges = Vec::new();
        let mut offset = 0;
        while offset < text.len() {
            let Some(found) = self.regex.find_at(&text, offset) else {
                break;
            };
            let start = offsets.partition_point(|offset| *offset < found.start());
            let end = offsets.partition_point(|offset| *offset < found.end());
            if start != end {
                ranges.push(start..end);
            }
            let next_character = if self.overlap || start == end {
                start.saturating_add(1)
            } else {
                end
            };
            let Some(&next) = offsets.get(next_character) else {
                break;
            };
            offset = next;
        }
        ranges
    }
}
