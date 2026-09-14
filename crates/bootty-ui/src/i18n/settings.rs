use super::{Localizer, presentation_key};
use crate::gpui::{SettingsControl, SettingsPage, SettingsPageItem, SettingsRow};

/// Translate only presentation fields after structural IDs and dependency groups are resolved.
/// User content, choice tokens, theme names and diagnostics keep their original bytes.
pub fn localize_settings(pages: &mut [SettingsPage], localizer: &Localizer) {
    for page in pages {
        page.title = localizer.text(
            &presentation_key("page", page.category.id(), "title"),
            &page.title,
        );
        page.search_terms.push(' ');
        page.search_terms.push_str(&page.title);
        for item in &mut page.items {
            match item {
                SettingsPageItem::SectionHeader {
                    id,
                    title,
                    search_terms,
                } => {
                    *title = localizer.text(&presentation_key("section", id, "title"), title);
                    search_terms.push(' ');
                    search_terms.push_str(title);
                }
                SettingsPageItem::Setting(row) => localize_row(row, localizer),
                SettingsPageItem::Dependent { parent, children } => {
                    localize_row(parent, localizer);
                    for child in children {
                        localize_row(child, localizer);
                    }
                }
            }
        }
    }
}

fn localize_row(row: &mut SettingsRow, localizer: &Localizer) {
    let (SettingsRow::Value {
        id, label, help, ..
    }
    | SettingsRow::AnsiPalette {
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
    }) = row
    else {
        return;
    };
    *label = localizer.text(&presentation_key("setting", id, "label"), label);
    *help = localizer.text(&presentation_key("setting", id, "help"), help);
    let id = id.clone();
    match row {
        SettingsRow::Action { button, .. } => {
            *button = localizer.text(&presentation_key("setting", &id, "button"), button);
        }
        SettingsRow::StringList { add_label, .. } => {
            *add_label = localizer.text(&presentation_key("setting", &id, "add"), add_label);
        }
        SettingsRow::Value {
            control: SettingsControl::Choice(options),
            ..
        } => {
            for option in options {
                let prefix = presentation_key("setting", &id, "choice");
                option.label = localizer.text(
                    &presentation_key(&prefix, &option.token, "label"),
                    &option.label,
                );
                if let Some(description) = &mut option.description {
                    *description = localizer.text(
                        &presentation_key(&prefix, &option.token, "description"),
                        description,
                    );
                }
            }
        }
        _ => {}
    }
}
