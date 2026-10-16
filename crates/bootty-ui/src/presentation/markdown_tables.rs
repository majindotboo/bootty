//! The tables in a reply's markdown, as tab-separated text that spreadsheets paste into cells.

use markdown::{ParseOptions, mdast::Node};

/// Whether `text` has a markdown table: a line of only pipes, dashes, colons and spaces under its
/// header. Cheap enough to check while drawing.
#[must_use]
pub fn has_table(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line.contains('|')
            && line.contains("---")
            && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    })
}

/// Every table in `text`, cells apart by tabs and rows by lines, tables apart by a blank line.
/// Cells keep their words without formatting.
#[must_use]
pub fn tables_tsv(text: &str) -> Option<String> {
    let root = markdown::to_mdast(text, &ParseOptions::gfm()).ok()?;
    let mut tables = Vec::new();
    collect(&root, &mut tables);
    (!tables.is_empty()).then(|| tables.join("\n\n"))
}

fn collect(node: &Node, tables: &mut Vec<String>) {
    if let Node::Table(table) = node {
        let rows = table
            .children
            .iter()
            .map(|row| {
                row.children()
                    .into_iter()
                    .flatten()
                    .map(|cell| cell.to_string().replace(['\t', '\n'], " "))
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>();
        tables.push(rows.join("\n"));
        return;
    }
    for child in node.children().into_iter().flatten() {
        collect(child, tables);
    }
}
