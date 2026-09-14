//! Host-neutral keybinding help state.

use super::searchable::{SearchableEntry, SearchableIntent, SearchableList};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybindHelpRow {
    pub action: String,
    pub chord: String,
    pub action_matches: Vec<usize>,
    pub chord_matches: Vec<usize>,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybindHelpModel {
    list: SearchableList<(String, String)>,
}

impl KeybindHelpModel {
    #[must_use]
    pub fn from_raw(raw_bindings: &[String]) -> Self {
        let mut bindings = raw_bindings
            .iter()
            .filter_map(|raw| parse_keybind(raw))
            .collect::<Vec<_>>();
        bindings.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let entries = bindings
            .into_iter()
            .map(|(chord, action)| {
                let mut entry = SearchableEntry::new((chord.clone(), action.clone()), action);
                entry.trailing = Some(chord);
                entry
            })
            .collect();
        Self {
            list: SearchableList::new(entries),
        }
    }

    #[must_use]
    pub fn filter(&self) -> &str {
        self.list.filter()
    }

    #[must_use]
    pub const fn total(&self) -> usize {
        self.list.total()
    }

    #[must_use]
    pub fn rows(&self) -> Vec<KeybindHelpRow> {
        self.list
            .rows()
            .into_iter()
            .map(|row| KeybindHelpRow {
                action: row.primary.to_owned(),
                chord: row.trailing.unwrap_or_default().to_owned(),
                action_matches: row.primary_matches,
                chord_matches: row.trailing_matches,
                selected: row.selected,
            })
            .collect()
    }

    pub fn apply(&mut self, intent: SearchableIntent) {
        self.list.apply(intent);
    }
}

/// Parse a configured `chord=action` binding for presentation.
#[must_use]
pub fn parse_keybind(raw: &str) -> Option<(String, String)> {
    let (mut chord, action) = raw.rsplit_once('=')?;
    chord = chord.trim();
    while let Some(("all" | "global" | "unconsumed" | "performable", rest)) = chord.split_once(':')
    {
        chord = rest;
    }
    let action = action.trim();
    (!chord.is_empty() && !action.is_empty()).then(|| (chord.to_owned(), action.to_owned()))
}
