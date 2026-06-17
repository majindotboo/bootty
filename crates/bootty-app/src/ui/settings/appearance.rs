use std::path::Path;

use eframe::egui;

use super::{SettingsWindow, color_row};

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::section(ui, palette, "THEME");

    let config_path = win.config_path.clone();
    let themes = win
        .theme_names
        .get_or_insert_with(|| available_themes(&config_path))
        .clone();

    egui::Grid::new("settings_theme_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Theme");
            let current = win.config.theme.clone().unwrap_or_default();
            let label = if current.is_empty() {
                "(default)".to_owned()
            } else {
                current.clone()
            };
            let mut options: Vec<&str> = vec!["(default)"];
            options.extend(themes.iter().map(String::as_str));
            let current_index = if current.is_empty() {
                Some(0)
            } else {
                themes
                    .iter()
                    .position(|theme| *theme == current)
                    .map(|i| i + 1)
            };
            if let Some(index) = super::searchable_combo(
                ui,
                win.palette,
                "settings_theme",
                &label,
                300.0,
                &options,
                current_index,
            ) {
                if index == 0 {
                    win.config.theme = None;
                    win.remove(&["theme"]);
                } else {
                    let chosen = themes[index - 1].clone();
                    win.config.theme = Some(chosen.clone());
                    win.set_str(&["theme"], &chosen);
                }
            }
            ui.end_row();
        });

    super::section(ui, palette, "COLOR OVERRIDES");
    ui.label(
        egui::RichText::new("Override individual terminal colors on top of the theme.")
            .color(palette.muted)
            .size(12.0),
    );
    ui.add_space(6.0);

    egui::Grid::new("settings_colors_grid")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            color_row(
                win,
                ui,
                "Background",
                &["colors", "background"],
                palette.base,
                |colors| &mut colors.background,
            );
            color_row(
                win,
                ui,
                "Foreground",
                &["colors", "foreground"],
                palette.text,
                |colors| &mut colors.foreground,
            );
            color_row(
                win,
                ui,
                "Cursor",
                &["colors", "cursor"],
                palette.primary,
                |colors| &mut colors.cursor,
            );
            color_row(
                win,
                ui,
                "Selection background",
                &["colors", "selection-background"],
                palette.hover,
                |colors| &mut colors.selection_background,
            );
            color_row(
                win,
                ui,
                "Selection foreground",
                &["colors", "selection-foreground"],
                palette.subtext,
                |colors| &mut colors.selection_foreground,
            );
        });
}

fn available_themes(config_path: &Path) -> Vec<String> {
    let mut names: Vec<String> = crate::config::builtin_theme_names()
        .map(str::to_owned)
        .collect();
    if let Some(dir) = config_path.parent().map(|parent| parent.join("themes"))
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                names.push(stem.to_owned());
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}
