use eframe::egui;

use super::SettingsWindow;

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::section(ui, palette, "FONT FAMILY");
    ui.label(
        egui::RichText::new(
            "Fonts are tried in priority order; later entries supply glyphs the earlier ones lack.",
        )
        .color(palette.muted)
        .size(12.0),
    );
    ui.add_space(6.0);

    let installed = win
        .font_families
        .get_or_insert_with(installed_font_families)
        .clone();
    let options: Vec<&str> = installed.iter().map(String::as_str).collect();

    let mut family = win.config.font.family.clone();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    let mut move_up: Option<usize> = None;
    let mut move_down: Option<usize> = None;
    let count = family.len();
    for (index, entry) in family.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{}.", index + 1)).color(palette.muted));
            let label = if entry.is_empty() {
                "(pick a font)".to_owned()
            } else {
                entry.clone()
            };
            let current_index = options.iter().position(|name| *name == entry.as_str());
            if let Some(choice) = super::searchable_combo(
                ui,
                palette,
                &format!("font_family_{index}"),
                &label,
                280.0,
                &options,
                current_index,
            ) {
                *entry = options[choice].to_owned();
                changed = true;
            }
            if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked() {
                move_up = Some(index);
            }
            if ui
                .add_enabled(index + 1 < count, egui::Button::new("↓"))
                .clicked()
            {
                move_down = Some(index);
            }
            if ui.button("✕").clicked() {
                remove = Some(index);
            }
        });
    }
    ui.add_space(4.0);
    if ui.button("+ Add fallback font").clicked() {
        family.push(String::new());
        changed = true;
    }
    if let Some(index) = move_up {
        family.swap(index, index - 1);
        changed = true;
    }
    if let Some(index) = move_down {
        family.swap(index, index + 1);
        changed = true;
    }
    if let Some(index) = remove {
        family.remove(index);
        changed = true;
    }
    if changed {
        win.config.font.family = family.clone();
        win.set_strings(&["font", "family"], &family);
    }

    super::section(ui, palette, "METRICS");
    egui::Grid::new("settings_font_metrics")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            slider(
                ui,
                win,
                "Size",
                &["font", "size"],
                6.0..=48.0,
                " pt",
                |font| &mut font.size,
            );
            optional_slider(
                ui,
                win,
                MetricOverrideRow {
                    label: "Cell width",
                    path: &["font", "cell-width"],
                    range: 1.0..=64.0,
                    suffix: " px",
                    default_value: crate::geometry::DEFAULT_CELL_WIDTH,
                    field: |font| &mut font.cell_width,
                },
            );
            optional_slider(
                ui,
                win,
                MetricOverrideRow {
                    label: "Cell height",
                    path: &["font", "cell-height"],
                    range: 1.0..=128.0,
                    suffix: " px",
                    default_value: crate::geometry::DEFAULT_LINE_HEIGHT,
                    field: |font| &mut font.cell_height,
                },
            );
            ui.label("Fit cell height");
            let mut fit_cell_height = win.config.font.fit_cell_height;
            if ui
                .checkbox(&mut fit_cell_height, "Fill available terminal height")
                .changed()
            {
                win.config.font.fit_cell_height = fit_cell_height;
                win.set_bool(&["font", "fit-cell-height"], fit_cell_height);
            }
            ui.end_row();
        });
}

fn slider(
    ui: &mut egui::Ui,
    win: &mut SettingsWindow,
    label: &str,
    path: &[&str],
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    field: fn(&mut crate::config::FontConfig) -> &mut f32,
) {
    ui.label(label);
    let mut value = *field(&mut win.config.font);
    if ui
        .add(egui::Slider::new(&mut value, range).suffix(suffix))
        .changed()
    {
        *field(&mut win.config.font) = value;
        win.set_f32(path, value);
    }
    ui.end_row();
}

struct MetricOverrideRow<'a> {
    label: &'a str,
    path: &'a [&'a str],
    range: std::ops::RangeInclusive<f32>,
    suffix: &'a str,
    default_value: f32,
    field: fn(&mut crate::config::FontConfig) -> &mut Option<f32>,
}

fn optional_slider(ui: &mut egui::Ui, win: &mut SettingsWindow, row: MetricOverrideRow<'_>) {
    ui.label(row.label);
    let current = *(row.field)(&mut win.config.font);
    ui.horizontal(|ui| {
        let mut value = current.unwrap_or(row.default_value);
        if ui
            .add(egui::Slider::new(&mut value, row.range.clone()).suffix(row.suffix))
            .changed()
        {
            *(row.field)(&mut win.config.font) = Some(value);
            win.set_f32(row.path, value);
        }
        if current.is_some() {
            if ui.small_button("Reset to Auto").clicked() {
                *(row.field)(&mut win.config.font) = None;
                win.remove(row.path);
            }
        } else {
            ui.label(egui::RichText::new("Auto").color(win.palette.muted));
        }
    });
    ui.end_row();
}

fn installed_font_families() -> Vec<String> {
    let database = bootty_render::font_database::system_font_database();
    let mut names: Vec<String> = database
        .faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}
