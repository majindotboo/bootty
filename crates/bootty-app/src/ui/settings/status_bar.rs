use eframe::egui::{self, RichText};

use super::SettingsWindow;
use crate::{
    color::Color,
    config::{SegmentAlign, StatusSegment},
};

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "STATUS BAR");
    egui::Grid::new("settings_statusbar_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Show status bar");
            let mut enabled = win.config.chrome.status_bar;
            if ui.checkbox(&mut enabled, "").changed() {
                win.config.chrome.status_bar = enabled;
                win.set_bool(&["chrome", "status-bar"], enabled);
            }
            ui.end_row();

            ui.label("Height");
            let mut height = win.config.chrome.status_height;
            if ui
                .add(
                    egui::DragValue::new(&mut height)
                        .range(20.0..=48.0)
                        .suffix(" px"),
                )
                .changed()
            {
                win.config.chrome.status_height = height;
                win.set_f32(&["chrome", "status-height"], height);
            }
            ui.end_row();

            ui.label("Hide tmux's own bar");
            let mut hide = win.config.multiplexer.hide_tmux_status;
            if ui.checkbox(&mut hide, "tmux backend only").changed() {
                win.config.multiplexer.hide_tmux_status = hide;
                win.set_bool(&["multiplexer", "hide-tmux-status"], hide);
            }
            ui.end_row();
        });

    super::section(ui, palette, "SEGMENTS");
    ui.label(
        RichText::new(
            "Each segment renders a Luau module, grouped by alignment. Reorder with the arrows; \
             fg/bg/icon fill fields the module leaves unset.",
        )
        .color(palette.muted)
        .size(12.0),
    );
    ui.add_space(8.0);

    let available = win
        .config_path
        .parent()
        .map(|parent| crate::extensions::available_module_names(&parent.join("status")))
        .unwrap_or_default();

    let mut changed = false;
    let mut remove_index = None;
    let mut move_action: Option<(usize, isize)> = None;
    let count = win.config.chrome.status_segments.len();

    for index in 0..count {
        ui.horizontal(|ui| {
            let segment = &mut win.config.chrome.status_segments[index];

            let aligns = [
                SegmentAlign::Left,
                SegmentAlign::Center,
                SegmentAlign::Right,
            ];
            let labels = ["left", "center", "right"];
            let current = aligns.iter().position(|a| *a == segment.align).unwrap_or(0);
            if let Some(selected) = super::searchable_combo(
                ui,
                palette,
                &format!("seg_align_{index}"),
                labels[current],
                88.0,
                &labels,
                Some(current),
            ) && aligns[selected] != segment.align
            {
                segment.align = aligns[selected];
                changed = true;
            }

            let mut module = segment.module.clone();
            if ui
                .add_sized(
                    [140.0, 24.0],
                    egui::TextEdit::singleline(&mut module).hint_text("module"),
                )
                .changed()
            {
                segment.module = module;
                changed = true;
            }

            changed |= optional_color(ui, "fg", &mut segment.fg, palette.subtext);
            changed |= optional_color(ui, "bg", &mut segment.bg, palette.surface);

            let mut icon = segment.icon.clone().unwrap_or_default();
            if ui
                .add_sized(
                    [44.0, 24.0],
                    egui::TextEdit::singleline(&mut icon).hint_text("icon"),
                )
                .changed()
            {
                segment.icon = (!icon.is_empty()).then_some(icon);
                changed = true;
            }

            if ui
                .add_enabled(index > 0, egui::Button::new("↑").small())
                .clicked()
            {
                move_action = Some((index, -1));
            }
            if ui
                .add_enabled(index + 1 < count, egui::Button::new("↓").small())
                .clicked()
            {
                move_action = Some((index, 1));
            }
            if ui.button("×").clicked() {
                remove_index = Some(index);
            }
        });
    }

    if let Some((index, delta)) = move_action {
        let target = index.saturating_add_signed(delta);
        win.config.chrome.status_segments.swap(index, target);
        changed = true;
    }
    if let Some(index) = remove_index {
        win.config.chrome.status_segments.remove(index);
        changed = true;
    }

    ui.add_space(10.0);
    if ui.button("+ Add segment").clicked() {
        let module = available
            .first()
            .cloned()
            .unwrap_or_else(|| "clock".to_owned());
        win.config.chrome.status_segments.push(StatusSegment {
            align: SegmentAlign::Left,
            module,
            ..StatusSegment::default()
        });
        changed = true;
    }

    if changed {
        win.set_status_segments();
    }

    if !available.is_empty() {
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!("Available modules: {}", available.join(", ")))
                .color(palette.muted)
                .size(12.0),
        );
    }
}

/// A small color button with a clear (`×`) affordance, editing an optional override in place.
/// Returns whether the value changed this frame.
fn optional_color(
    ui: &mut egui::Ui,
    label: &str,
    slot: &mut Option<Color>,
    seed: egui::Color32,
) -> bool {
    ui.label(RichText::new(label).size(11.0));
    let mut rgb = slot.map_or([seed.r(), seed.g(), seed.b()], |color| {
        [color.r, color.g, color.b]
    });
    let mut changed = false;
    if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
        *slot = Some(Color {
            r: rgb[0],
            g: rgb[1],
            b: rgb[2],
        });
        changed = true;
    }
    if slot.is_some() && ui.small_button("×").clicked() {
        *slot = None;
        changed = true;
    }
    changed
}
