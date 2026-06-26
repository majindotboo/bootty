use std::path::Path;

use eframe::egui;
use libghostty_vt::style::RgbColor;

use super::SettingsWindow;
use crate::color::Color;

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "THEME");
    theme_row(win, ui);

    super::section(ui, palette, "TERMINAL COLORS");
    terminal_color_row(
        win,
        ui,
        "Background",
        "Terminal background override.",
        &["colors", "background"],
        palette.base,
        |colors| &mut colors.background,
    );
    terminal_color_row(
        win,
        ui,
        "Foreground",
        "Primary terminal text override.",
        &["colors", "foreground"],
        palette.text,
        |colors| &mut colors.foreground,
    );
    terminal_color_row(
        win,
        ui,
        "Cursor",
        "Cursor fill color.",
        &["colors", "cursor"],
        palette.primary,
        |colors| &mut colors.cursor,
    );
    terminal_color_row(
        win,
        ui,
        "Cursor text",
        "Text drawn under the cursor.",
        &["colors", "cursor-text"],
        palette.base,
        |colors| &mut colors.cursor_text,
    );
    terminal_color_row(
        win,
        ui,
        "Selection",
        "Selection background and foreground.",
        &["colors", "selection-background"],
        palette.hover,
        |colors| &mut colors.selection_background,
    );
    terminal_color_row(
        win,
        ui,
        "Selection text",
        "Selected text foreground.",
        &["colors", "selection-foreground"],
        palette.subtext,
        |colors| &mut colors.selection_foreground,
    );
    terminal_color_row(
        win,
        ui,
        "Highlight",
        "Search or match highlight background.",
        &["colors", "highlight-background"],
        palette.hover,
        |colors| &mut colors.highlight_background,
    );
    terminal_color_row(
        win,
        ui,
        "Highlight text",
        "Search or match highlight foreground.",
        &["colors", "highlight-foreground"],
        palette.text,
        |colors| &mut colors.highlight_foreground,
    );
    terminal_color_row(
        win,
        ui,
        "Pointer foreground",
        "Pointer text/foreground override.",
        &["colors", "pointer-foreground"],
        palette.base,
        |colors| &mut colors.pointer_foreground,
    );
    terminal_color_row(
        win,
        ui,
        "Pointer background",
        "Pointer background override.",
        &["colors", "pointer-background"],
        palette.text,
        |colors| &mut colors.pointer_background,
    );

    super::section(ui, palette, "SIDEBAR COLORS");
    super::settings_notice(
        ui,
        palette.muted,
        "Unset colors inherit from the active theme.",
    );
    super::sidebar_color_row(
        win,
        ui,
        "Background",
        "Sidebar panel background.",
        &["sidebar", "background"],
        palette.mantle,
        |sidebar| &mut sidebar.background,
    );
    super::sidebar_color_row(
        win,
        ui,
        "Foreground",
        "Sidebar text and icons.",
        &["sidebar", "foreground"],
        palette.text,
        |sidebar| &mut sidebar.foreground,
    );
    super::sidebar_color_row(
        win,
        ui,
        "Selected row",
        "Selected session fill.",
        &["sidebar", "selected"],
        palette.surface,
        |sidebar| &mut sidebar.selected,
    );
    super::sidebar_color_row(
        win,
        ui,
        "Hover row",
        "Hovered session fill.",
        &["sidebar", "hover"],
        palette.hover,
        |sidebar| &mut sidebar.hover,
    );
    super::sidebar_color_row(
        win,
        ui,
        "Border",
        "Separator between sidebar and terminal content.",
        &["sidebar", "border"],
        palette.border,
        |sidebar| &mut sidebar.border,
    );

    super::section(ui, palette, "SPLIT PANES");
    super::chrome_color_row(
        win,
        ui,
        "Divider",
        "Color of the gap between split panes; unset uses the window background.",
        &["chrome", "pane-divider-color"],
        palette.mantle,
        |chrome| &mut chrome.pane_divider_color,
    );
    super::chrome_color_row(
        win,
        ui,
        "Focus border",
        "Border around the focused split pane; unset uses the theme accent.",
        &["chrome", "pane-focus-border-color"],
        palette.primary,
        |chrome| &mut chrome.pane_focus_border_color,
    );

    palette_section(win, ui);
}

fn theme_row(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let config_path = win.config_path.clone();
    let themes = win
        .theme_names
        .get_or_insert_with(|| available_themes(&config_path))
        .clone();
    super::settings_row(
        ui,
        win.palette,
        "Theme",
        "Built-in or config-directory theme.",
        |ui| {
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
        },
    );
}

fn terminal_color_row(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    label: &str,
    help: &str,
    path: &[&str],
    seed: egui::Color32,
    field: fn(&mut crate::config::ColorConfig) -> &mut Option<Color>,
) {
    super::settings_row(ui, win.palette, label, help, |ui| {
        let current = *field(&mut win.config.colors);
        let mut rgb = current.map_or([seed.r(), seed.g(), seed.b()], |color| {
            [color.r, color.g, color.b]
        });
        if super::settings_color_picker(ui, win.palette, &mut rgb).changed() {
            *field(&mut win.config.colors) = Some(Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
            });
            win.set_color(path, rgb);
        }
        if current.is_some() && super::settings_button(ui, win.palette, "Reset").clicked() {
            *field(&mut win.config.colors) = None;
            win.remove(path);
        }
    });
}

fn palette_section(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::section(ui, palette, "ANSI PALETTE");
    super::settings_toggle_row(
        ui,
        palette,
        "Generate 256-color cube",
        "Generate the extended color cube from the ANSI base palette.",
        win.config.colors.palette_generate,
        |enabled| {
            win.config.colors.palette_generate = enabled;
            win.set_bool(&["colors", "palette-generate"], enabled);
        },
    );
    super::settings_toggle_row(
        ui,
        palette,
        "Harmonious blend",
        "Blend generated colors toward the active theme.",
        win.config.colors.palette_harmonious,
        |enabled| {
            win.config.colors.palette_harmonious = enabled;
            win.set_bool(&["colors", "palette-harmonious"], enabled);
        },
    );

    let mut colors = win.config.colors.palette.clone();
    let mut changed = false;
    // Seed sources for the "active" palette buttons: existing overrides win, theme defaults fill the
    // rest, and the cube anchors come from the current bg/fg + harmonious toggle.
    let base_overrides = colors.clone();
    let harmonious = win.config.colors.palette_harmonious;
    let bg_override = win.config.colors.background;
    let fg_override = win.config.colors.foreground;
    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("ANSI colors")
                    .color(palette.text)
                    .strong(),
            );
            ui.label(
                egui::RichText::new(
                    "Override indexed terminal colors. Empty inherits the active theme.",
                )
                .color(palette.muted)
                .size(11.0),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if super::settings_button(ui, palette, "Use standard 16").clicked() {
                    colors = active_base16(&base_overrides);
                    changed = true;
                }
                if super::settings_button(ui, palette, "Use xterm 256").clicked() {
                    colors = active_xterm256(&base_overrides, bg_override, fg_override, harmonious);
                    changed = true;
                }
                if super::settings_button(ui, palette, "Add empty slot").clicked() {
                    colors.push(Color { r: 0, g: 0, b: 0 });
                    changed = true;
                }
                if !colors.is_empty()
                    && super::settings_button(ui, palette, "Remove last slot").clicked()
                {
                    colors.pop();
                    changed = true;
                }
                if !colors.is_empty()
                    && super::settings_button(ui, palette, "Clear overrides").clicked()
                {
                    colors.clear();
                    changed = true;
                }
            });
            ui.add_space(10.0);
            if colors.is_empty() {
                ui.label(egui::RichText::new("No ANSI overrides set.").color(palette.muted));
            } else {
                egui::Grid::new("settings_ansi_palette_grid")
                    .num_columns(8)
                    .spacing([18.0, 10.0])
                    .show(ui, |ui| {
                        for (index, color) in colors.iter_mut().enumerate() {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{index:02}"))
                                        .color(palette.muted)
                                        .size(11.0),
                                );
                                let mut rgb = [color.r, color.g, color.b];
                                if super::settings_color_picker(ui, palette, &mut rgb).changed() {
                                    *color = Color {
                                        r: rgb[0],
                                        g: rgb[1],
                                        b: rgb[2],
                                    };
                                    changed = true;
                                }
                            });
                            if (index + 1) % 8 == 0 {
                                ui.end_row();
                            }
                        }
                    });
            }
        });
    ui.add_space(8.0);

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

/// The active base 16 ANSI colors: existing overrides take priority, theme defaults fill the rest.
fn active_base16(overrides: &[Color]) -> Vec<Color> {
    let defaults = bootty_terminal::terminal_palette::default_base16();
    (0..16)
        .map(|index| {
            overrides
                .get(index)
                .copied()
                .unwrap_or_else(|| rgb_to_color(defaults[index]))
        })
        .collect()
}

/// The full active 256-color palette, generated from the active base 16 the same way the terminal
/// does (Lab-space cube + grayscale ramp), honoring the harmonious-blend toggle.
fn active_xterm256(
    overrides: &[Color],
    bg: Option<Color>,
    fg: Option<Color>,
    harmonious: bool,
) -> Vec<Color> {
    let base16 = active_base16(overrides);
    let base: [RgbColor; 256] = std::array::from_fn(|index| {
        base16
            .get(index)
            .map_or(RgbColor { r: 0, g: 0, b: 0 }, |color| color_to_rgb(*color))
    });
    // Keep the base 16 verbatim; generate everything above them.
    let skip: [bool; 256] = std::array::from_fn(|index| index < 16);
    let defaults = bootty_terminal::terminal_palette::default_base16();
    let bg_rgb = bg.map_or(defaults[0], color_to_rgb);
    let fg_rgb = fg.map_or(defaults[15], color_to_rgb);
    bootty_terminal::terminal_palette::generate_256_palette(
        &base, &skip, bg_rgb, fg_rgb, harmonious,
    )
    .iter()
    .map(|color| rgb_to_color(*color))
    .collect()
}

fn color_to_rgb(color: Color) -> RgbColor {
    RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn rgb_to_color(color: RgbColor) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
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
