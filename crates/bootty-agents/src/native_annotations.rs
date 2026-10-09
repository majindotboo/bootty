//! Response annotation envelopes are presentation data, never agent instructions.
use markdown::{ParseOptions, mdast::Node};
use serde::Deserialize;
use std::fmt::Write as _;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct NativeResponseAnnotation {
    pub text: String,
    pub annotation: String,
}

/// Read only the bounded annotation envelope carried by a user message.
#[must_use]
pub fn response_annotations(text: &str) -> Vec<NativeResponseAnnotation> {
    annotation_envelope(text).map_or_default(|(_, _, annotations)| annotations)
}

fn annotation_envelope(text: &str) -> Option<(usize, usize, Vec<NativeResponseAnnotation>)> {
    const OPEN: &str = "<response-annotations>";
    const CLOSE: &str = "</response-annotations>";
    let start = text.find(OPEN)?;
    let content_start = start.saturating_add(OPEN.len());
    let content = text.get(content_start..)?;
    let length = content.find(CLOSE)?;
    if length > crate::MAX_NATIVE_PROMPT_TEXT_BYTES {
        return None;
    }
    let annotations: Vec<NativeResponseAnnotation> =
        serde_json::from_str(content.get(..length)?).ok()?;
    if annotations.is_empty() || annotations.len() > 16 {
        return None;
    }
    Some((
        start,
        content_start
            .saturating_add(length)
            .saturating_add(CLOSE.len()),
        annotations,
    ))
}

pub fn display_user_annotations(text: String) -> String {
    let Some((start, end, annotations)) = annotation_envelope(&text) else {
        return text;
    };
    let mut display = String::new();
    for (ix, annotation) in annotations.iter().enumerate() {
        _ = write!(display, "\n\n**Annotation {}**\n\n", ix.saturating_add(1));
        for line in annotation.text.lines() {
            display.push_str("> ");
            display.push_str(line);
            display.push('\n');
        }
        display.push('\n');
        display.push_str(&annotation.annotation);
    }
    let mut text = text;
    text.replace_range(start..end, &display);
    text
}

pub fn display_annotation_references(text: String) -> String {
    const DIRECTIVE: &str = ":codex-annotation{index=\"";
    if !text.contains(DIRECTIVE) {
        return text;
    }
    let Ok(root) = markdown::to_mdast(&text, &ParseOptions::gfm()) else {
        return text;
    };
    let mut replacements = Vec::new();
    collect_references(&root, &text, &mut replacements);
    let mut text = text;
    for (range, ix) in replacements.into_iter().rev() {
        text.replace_range(range, &format!("[Annotation {ix}](bootty-annotation:{ix})"));
    }
    text
}

fn collect_references(
    node: &Node,
    text: &str,
    replacements: &mut Vec<(std::ops::Range<usize>, usize)>,
) {
    const DIRECTIVE: &str = ":codex-annotation{index=\"";
    if matches!(
        node,
        Node::Link(_) | Node::LinkReference(_) | Node::Image(_) | Node::ImageReference(_)
    ) {
        return;
    }
    if let Node::Text(node) = node
        && let Some(position) = &node.position
        && let Some(source) = text.get(position.start.offset..position.end.offset)
    {
        for (start, _) in source.match_indices(DIRECTIVE) {
            // CommonMark escapes stay literal, even though the parsed text node drops the slash.
            let escapes = text
                .get(..position.start.offset.saturating_add(start))
                .unwrap_or_default()
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'\\')
                .count();
            if escapes % 2 != 0 {
                continue;
            }
            let digits_start = start.saturating_add(DIRECTIVE.len());
            let tail = source.get(digits_start..).unwrap_or_default();
            let length = tail.bytes().take_while(u8::is_ascii_digit).count();
            if !tail
                .get(length..)
                .is_some_and(|rest| rest.starts_with("\"}"))
            {
                continue;
            }
            if let Some(ix) = tail
                .get(..length)
                .and_then(|digits| digits.parse::<usize>().ok())
                .filter(|ix| (1..=16).contains(ix))
            {
                replacements.push((
                    position.start.offset.saturating_add(start)
                        ..position
                            .start
                            .offset
                            .saturating_add(digits_start)
                            .saturating_add(length)
                            .saturating_add(2),
                    ix,
                ));
            }
        }
    } else if let Some(children) = node.children() {
        for child in children {
            collect_references(child, text, replacements);
        }
    }
}
