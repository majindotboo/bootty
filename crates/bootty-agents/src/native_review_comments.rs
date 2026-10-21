//! File review context travels with a prompt while its chip stays compact in the editor.
use bootty_git::diff::DiffAnchor;
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeReviewComment {
    pub id: String,
    pub anchor: DiffAnchor,
    pub quote: String,
    pub body: String,
    pub prompt_range: Range<usize>,
}

impl NativeReviewComment {
    #[must_use]
    pub fn label(&self) -> String {
        let name = self
            .anchor
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&self.anchor.path);
        if self.anchor.start_line == self.anchor.line {
            format!("{name}:{}", self.anchor.line)
        } else {
            format!("{name}:{}–{}", self.anchor.start_line, self.anchor.line)
        }
    }
}

pub fn validate(text: &str, comments: &[NativeReviewComment]) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    if comments.len() > 16 {
        return Err("Attach at most 16 review comments".into());
    }
    for comment in comments {
        let anchor = &comment.anchor;
        if comment.id.is_empty()
            || comment.id.len() > 128
            || !ids.insert(&comment.id)
            || anchor.path.is_empty()
            || anchor.path.len() > 4096
            || anchor.path.chars().any(char::is_control)
            || std::path::Path::new(&anchor.path)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            || anchor.start_line == 0
            || anchor.line < anchor.start_line
            || comment.quote.len() > 8000
            || comment.body.trim().is_empty()
            || comment.body.len() > 8000
            || text.get(comment.prompt_range.clone()) != Some("[review]")
        {
            return Err("Invalid review comment, source range or inline reference".into());
        }
    }
    let mut ranges = comments
        .iter()
        .map(|comment| &comment.prompt_range)
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| range.start);
    if ranges
        .windows(2)
        .any(|pair| matches!(pair, [left, right] if left.end > right.start))
    {
        return Err("Review comment references overlap".into());
    }
    Ok(())
}

pub fn append_context(
    message: &mut String,
    comments: &[NativeReviewComment],
) -> Result<(), String> {
    if !comments.is_empty() {
        message.push_str("\n\n<review_comments>\n");
        message.push_str(&serde_json::to_string(comments).map_err(|error| error.to_string())?);
        message.push_str("\n</review_comments>");
    }
    Ok(())
}
