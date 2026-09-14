//! Search index for the copied Zed settings page model.
//!
//! Directly adapted from Zed `settings_ui.rs` at commit 1662f5f3f6, lines 993-1009 and
//! 2255-2488. Bootty keeps the synchronous exact/prefix half of Zed's index because its projected
//! catalog is small; the page/header/item lookup and filter-table behavior are unchanged.

use super::model::{SettingsPage, SettingsPageItem, SettingsRow, StatusSegmentsSnapshot};

#[derive(Clone, Debug)]
struct SearchDocument {
    id: usize,
    words: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
struct SearchKeyLutEntry {
    page: usize,
    header: usize,
    item: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct SearchIndex {
    documents: Vec<SearchDocument>,
    key_lut: Vec<SearchKeyLutEntry>,
}

#[derive(Clone, Debug)]
pub(super) struct SearchMatches {
    pub(super) table: Vec<Vec<bool>>,
    pub(super) has_query: bool,
    pub(super) best_page: Option<usize>,
}

impl SearchIndex {
    pub(super) fn build(pages: &[SettingsPage]) -> Self {
        let mut documents = Vec::new();
        let mut key_lut = Vec::new();

        for (page_index, page) in pages.iter().enumerate() {
            let mut header_index = 0;
            let mut header_title = String::new();
            let mut header_terms = String::new();

            for (item_index, item) in page.items.iter().enumerate() {
                if let SettingsPageItem::SectionHeader {
                    title,
                    search_terms,
                    ..
                } = item
                {
                    header_index = item_index;
                    header_title.clone_from(title);
                    header_terms.clone_from(search_terms);
                }

                let mut parts = vec![
                    page.title.clone(),
                    page.search_terms.clone(),
                    header_title.clone(),
                    header_terms.clone(),
                ];
                parts.extend(item_search_parts(item));
                documents.push(SearchDocument {
                    id: key_lut.len(),
                    words: split_into_words(parts.iter().map(String::as_str)),
                });
                key_lut.push(SearchKeyLutEntry {
                    page: page_index,
                    header: header_index,
                    item: item_index,
                });
            }
        }

        Self { documents, key_lut }
    }

    pub(super) fn matches(&self, pages: &[SettingsPage], query: &str) -> SearchMatches {
        let query_words = split_into_words(std::iter::once(query));
        if query_words.is_empty() {
            return SearchMatches {
                table: pages
                    .iter()
                    .map(|page| vec![true; page.items.len()])
                    .collect(),
                has_query: false,
                best_page: None,
            };
        }

        let mut table: Vec<Vec<bool>> = pages
            .iter()
            .map(|page| vec![false; page.items.len()])
            .collect();
        for document in &self.documents {
            let matches = query_words.iter().all(|query_word| {
                document
                    .words
                    .iter()
                    .any(|word| word.starts_with(query_word) || word.contains(query_word))
            });
            if !matches {
                continue;
            }
            let Some(entry) = self.key_lut.get(document.id) else {
                continue;
            };
            if let Some(page) = table.get_mut(entry.page) {
                if let Some(header) = page.get_mut(entry.header) {
                    *header = true;
                }
                if let Some(item) = page.get_mut(entry.item) {
                    *item = true;
                }
            }
        }

        let best_page = table
            .iter()
            .enumerate()
            .max_by_key(|(_, page)| page.iter().filter(|visible| **visible).count())
            .and_then(|(index, page)| page.iter().any(|visible| *visible).then_some(index));

        SearchMatches {
            table,
            has_query: true,
            best_page,
        }
    }
}

fn split_into_words<'a>(parts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    parts
        .into_iter()
        .flat_map(|part| {
            part.split(|character: char| !character.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(str::to_lowercase)
        })
        .collect()
}

fn item_search_parts(item: &SettingsPageItem) -> Vec<String> {
    match item {
        SettingsPageItem::SectionHeader {
            id,
            title,
            search_terms,
        } => vec![id.clone(), title.clone(), search_terms.clone()],
        SettingsPageItem::Setting(row) => row_search_parts(row),
        SettingsPageItem::Dependent { parent, children } => {
            // Keep Zed's one-document/one-filter-entry DynamicItem invariant. Only children the
            // host projected as active contribute terms, and a child match reveals the group.
            let mut parts = row_search_parts(parent);
            parts.extend(children.iter().flat_map(row_search_parts));
            parts
        }
    }
}

fn row_search_parts(row: &SettingsRow) -> Vec<String> {
    match row {
        SettingsRow::Section(label) => vec![label.clone()],
        SettingsRow::Notice { text, .. } => vec![text.clone()],
        SettingsRow::Value {
            id, label, help, ..
        }
        | SettingsRow::Action {
            id, label, help, ..
        }
        | SettingsRow::StringList {
            id, label, help, ..
        }
        | SettingsRow::ModifierRemaps {
            id, label, help, ..
        }
        | SettingsRow::Environment {
            id, label, help, ..
        }
        | SettingsRow::FontFeatures {
            id, label, help, ..
        }
        | SettingsRow::StatusSegments(StatusSegmentsSnapshot {
            id, label, help, ..
        }) => vec![id.clone(), label.clone(), help.clone()],
        SettingsRow::AnsiPalette { label, help, .. } => vec![label.clone(), help.clone()],
        SettingsRow::ModuleIntegrations(editor) => vec![editor.identity.clone()],
        SettingsRow::Remote(remote) => {
            let mut parts = vec![
                remote.id.clone(),
                remote.label.clone(),
                remote.detail.clone(),
            ];
            if let Some(profile) = &remote.profile {
                for field in &profile.fields {
                    parts.extend([field.id.clone(), field.label.clone(), field.value.clone()]);
                }
            }
            parts
        }
    }
}
