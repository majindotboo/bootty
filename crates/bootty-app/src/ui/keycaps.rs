use eframe::egui;

/// Lay a trigger out as keycaps. On macOS the modifier symbols come from the icon font (the UI font
/// has no command/option/control glyphs in some themes), elsewhere modifiers fall back to text
/// joined with `+`.
pub fn trigger_galley(
    ui: &egui::Ui,
    palette: bootty_ui::ThemePalette,
    trigger: &str,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    ui.painter()
        .layout_job(trigger_layout_job(palette, trigger, color, max_width))
}

pub fn trigger_galley_from_painter(
    painter: &egui::Painter,
    palette: bootty_ui::ThemePalette,
    trigger: &str,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    painter.layout_job(trigger_layout_job(palette, trigger, color, max_width))
}

fn trigger_layout_job(
    palette: bootty_ui::ThemePalette,
    trigger: &str,
    color: egui::Color32,
    max_width: f32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let mut first_combo = true;
    for combo in trigger.split('>') {
        let combo = combo.trim();
        if combo.is_empty() {
            continue;
        }
        if !first_combo {
            job.append(
                " ▸ ",
                2.0,
                TextFormat {
                    font_id: egui::FontId::proportional(12.0),
                    color: palette.muted,
                    ..Default::default()
                },
            );
        }
        first_combo = false;
        append_combo(&mut job, palette, combo, color);
    }
    job
}

fn append_combo(
    job: &mut egui::text::LayoutJob,
    palette: bootty_ui::ThemePalette,
    combo: &str,
    color: egui::Color32,
) {
    use egui::text::TextFormat;
    let tokens: Vec<&str> = combo
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    for (index, &token) in tokens.iter().enumerate() {
        let leading = if index == 0 { 0.0 } else { 3.0 };
        if cfg!(target_os = "macos") {
            if let Some((glyph, family)) =
                modifier_icon(token).and_then(crate::ui::icons::icon_glyph)
            {
                job.append(
                    &glyph.to_string(),
                    leading,
                    TextFormat {
                        font_id: egui::FontId::new(15.0, egui::FontFamily::Name(family.into())),
                        color,
                        ..Default::default()
                    },
                );
                continue;
            }
            job.append(
                &key_label(token),
                leading,
                TextFormat {
                    font_id: egui::FontId::monospace(13.0),
                    color,
                    ..Default::default()
                },
            );
        } else {
            if index > 0 {
                job.append(
                    "+",
                    2.0,
                    TextFormat {
                        font_id: egui::FontId::proportional(12.0),
                        color: palette.muted,
                        ..Default::default()
                    },
                );
            }
            job.append(
                &key_label(token),
                if index == 0 { 0.0 } else { 2.0 },
                TextFormat {
                    font_id: egui::FontId::monospace(13.0),
                    color,
                    ..Default::default()
                },
            );
        }
    }
}

/// Icon-font slug for a modifier token, so modifier symbols render from the icon font instead of
/// relying on the UI font.
fn modifier_icon(token: &str) -> Option<&'static str> {
    match token {
        "cmd" | "super" => Some("command"),
        "alt" | "option" => Some("option"),
        "shift" => Some("arrow-big-up"),
        "ctrl" | "control" => Some("chevron-up"),
        _ => None,
    }
}

fn key_label(token: &str) -> String {
    match token {
        "cmd" | "super" => "Cmd".to_owned(),
        "ctrl" | "control" => "Ctrl".to_owned(),
        "alt" | "option" => "Alt".to_owned(),
        "shift" => "Shift".to_owned(),
        "space" => "Space".to_owned(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_labels_capitalize_single_character_keys() {
        assert_eq!(key_label("p"), "P");
        assert_eq!(key_label("space"), "Space");
        assert_eq!(key_label("escape"), "escape");
    }
}
