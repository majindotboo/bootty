use std::path::Path;

use eframe::egui;

use super::{SettingsWindow, color_row};
use crate::color::Color;

/// Standard xterm ANSI 16-color palette, used to seed the palette editor.
const ANSI_16: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x80, 0x00, 0x00],
    [0x00, 0x80, 0x00],
    [0x80, 0x80, 0x00],
    [0x00, 0x00, 0x80],
    [0x80, 0x00, 0x80],
    [0x00, 0x80, 0x80],
    [0xc0, 0xc0, 0xc0],
    [0x80, 0x80, 0x80],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x00, 0x00, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

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
                "Cursor text",
                &["colors", "cursor-text"],
                palette.base,
                |colors| &mut colors.cursor_text,
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
            color_row(
                win,
                ui,
                "Highlight background",
                &["colors", "highlight-background"],
                palette.hover,
                |colors| &mut colors.highlight_background,
            );
            color_row(
                win,
                ui,
                "Highlight foreground",
                &["colors", "highlight-foreground"],
                palette.text,
                |colors| &mut colors.highlight_foreground,
            );
            color_row(
                win,
                ui,
                "Pointer foreground",
                &["colors", "pointer-foreground"],
                palette.base,
                |colors| &mut colors.pointer_foreground,
            );
            color_row(
                win,
                ui,
                "Pointer background",
                &["colors", "pointer-background"],
                palette.text,
                |colors| &mut colors.pointer_background,
            );
            color_row(
                win,
                ui,
                "Tektronix foreground",
                &["colors", "tektronix-foreground"],
                palette.text,
                |colors| &mut colors.tektronix_foreground,
            );
            color_row(
                win,
                ui,
                "Tektronix background",
                &["colors", "tektronix-background"],
                palette.base,
                |colors| &mut colors.tektronix_background,
            );
            color_row(
                win,
                ui,
                "Tektronix cursor",
                &["colors", "tektronix-cursor"],
                palette.primary,
                |colors| &mut colors.tektronix_cursor,
            );
        });

    sidebar_colors_section(win, ui);
    palette_section(win, ui);
}

/// Sidebar color overrides from `[sidebar]`. Each slot layers on top of the theme; `fullscreen`
/// background only applies when the sidebar extends into the notch/menu-bar area.
fn sidebar_colors_section(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::section(ui, palette, "SIDEBAR");
    ui.label(
        egui::RichText::new(
            "Override sidebar colors on top of the theme. Hover, selected, and border tints fall \
             back to a blend of the background and foreground when unset.",
        )
        .color(palette.muted)
        .size(12.0),
    );
    ui.add_space(6.0);

    egui::Grid::new("settings_sidebar_colors_grid")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            sidebar_color_row(
                win,
                ui,
                "Background",
                &["sidebar", "background"],
                palette.base,
                |sidebar| &mut sidebar.background,
            );
            sidebar_color_row(
                win,
                ui,
                "Fullscreen background",
                &["sidebar", "fullscreen-background"],
                palette.base,
                |sidebar| &mut sidebar.fullscreen_background,
            );
            sidebar_color_row(
                win,
                ui,
                "Foreground",
                &["sidebar", "foreground"],
                palette.text,
                |sidebar| &mut sidebar.foreground,
            );
            sidebar_color_row(
                win,
                ui,
                "Selected item",
                &["sidebar", "selected"],
                palette.hover,
                |sidebar| &mut sidebar.selected,
            );
            sidebar_color_row(
                win,
                ui,
                "Hover",
                &["sidebar", "hover"],
                palette.hover,
                |sidebar| &mut sidebar.hover,
            );
            sidebar_color_row(
                win,
                ui,
                "Border",
                &["sidebar", "border"],
                palette.border,
                |sidebar| &mut sidebar.border,
            );
        });
}

/// Sidebar variant of [`super::color_row`]: projects an override slot on `SidebarConfig`.
fn sidebar_color_row(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    label: &str,
    path: &[&str],
    seed: egui::Color32,
    field: fn(&mut crate::config::SidebarConfig) -> &mut Option<Color>,
) {
    ui.label(label);
    let current = *field(&mut win.config.sidebar);
    ui.horizontal(|ui| {
        let mut rgb = current.map_or([seed.r(), seed.g(), seed.b()], |color| {
            [color.r, color.g, color.b]
        });
        if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
            *field(&mut win.config.sidebar) = Some(Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
            });
            win.set_color(path, rgb);
        }
        if current.is_some() && ui.small_button("Reset").clicked() {
            *field(&mut win.config.sidebar) = None;
            win.remove(path);
        }
    });
    ui.end_row();
}

/// The ANSI palette editor: optional `palette-generate`/`palette-harmonious` toggles plus an
/// editable list of color slots that override ANSI indices 0..N.
fn palette_section(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::section(ui, palette, "ANSI PALETTE");
    ui.label(
        egui::RichText::new(
            "Override the 16 ANSI colors. Generate fills the 256-color cube from these; \
             harmonious blends them toward the theme.",
        )
        .color(palette.muted)
        .size(12.0),
    );
    ui.add_space(6.0);

    egui::Grid::new("settings_palette_flags")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label("Generate 256-color cube");
            let mut generate = win.config.colors.palette_generate;
            if ui.checkbox(&mut generate, "").changed() {
                win.config.colors.palette_generate = generate;
                win.set_bool(&["colors", "palette-generate"], generate);
            }
            ui.end_row();

            ui.label("Harmonious blend");
            let mut harmonious = win.config.colors.palette_harmonious;
            if ui.checkbox(&mut harmonious, "").changed() {
                win.config.colors.palette_harmonious = harmonious;
                win.set_bool(&["colors", "palette-harmonious"], harmonious);
            }
            ui.end_row();
        });

    ui.add_space(8.0);

    let mut colors = win.config.colors.palette.clone();
    let mut changed = false;
    if colors.is_empty() {
        ui.label(
            egui::RichText::new("No palette override set; ANSI colors come from the theme.")
                .color(palette.muted)
                .size(12.0),
        );
        ui.add_space(4.0);
        if ui.button("Populate 16 ANSI colors").clicked() {
            colors = ANSI_16
                .iter()
                .map(|[r, g, b]| Color {
                    r: *r,
                    g: *g,
                    b: *b,
                })
                .collect();
            changed = true;
        }
    } else {
        ui.horizontal_wrapped(|ui| {
            for (index, color) in colors.iter_mut().enumerate() {
                let mut rgb = [color.r, color.g, color.b];
                let response = egui::color_picker::color_edit_button_srgb(ui, &mut rgb)
                    .on_hover_text(format!("ANSI {index}"));
                if response.changed() {
                    *color = Color {
                        r: rgb[0],
                        g: rgb[1],
                        b: rgb[2],
                    };
                    changed = true;
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("+ Add color").clicked() {
                colors.push(Color { r: 0, g: 0, b: 0 });
                changed = true;
            }
            if ui.button("Remove last").clicked() {
                colors.pop();
                changed = true;
            }
            if ui.button("Clear palette").clicked() {
                colors.clear();
                changed = true;
            }
        });
    }

    if changed {
        win.config.colors.palette = colors.clone();
        if colors.is_empty() {
            win.remove(&["colors", "palette"]);
        } else {
            let hex: Vec<String> = colors
                .iter()
                .map(|color| format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b))
                .collect();
            win.set_strings(&["colors", "palette"], &hex);
        }
    }
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
