use std::path::PathBuf;

use eframe::egui;

use super::SettingsWindow;

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "SESSION");
    egui::Grid::new("settings_session_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Shell");
            let mut shell = win.config.session.shell.clone().unwrap_or_default();
            if text_field(ui, &mut shell, "default login shell").changed() {
                if shell.trim().is_empty() {
                    win.config.session.shell = None;
                    win.remove(&["session", "shell"]);
                } else {
                    win.config.session.shell = Some(shell.clone());
                    win.set_str(&["session", "shell"], &shell);
                }
            }
            ui.end_row();

            ui.label("Working directory");
            let mut cwd = win
                .config
                .session
                .working_directory
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            if text_field(ui, &mut cwd, "inherit from launcher").changed() {
                if cwd.trim().is_empty() {
                    win.config.session.working_directory = None;
                    win.remove(&["session", "working-directory"]);
                } else {
                    win.config.session.working_directory = Some(PathBuf::from(cwd.clone()));
                    win.set_str(&["session", "working-directory"], &cwd);
                }
            }
            ui.end_row();

            ui.label("TERM");
            let mut term = win.config.session.term.clone();
            if text_field(ui, &mut term, "xterm-256color").changed() {
                win.config.session.term = term.clone();
                if term.trim().is_empty() {
                    win.remove(&["session", "term"]);
                } else {
                    win.set_str(&["session", "term"], &term);
                }
            }
            ui.end_row();

            ui.label("COLORTERM");
            let mut colorterm = win.config.session.colorterm.clone();
            if text_field(ui, &mut colorterm, "truecolor").changed() {
                win.config.session.colorterm = colorterm.clone();
                if colorterm.trim().is_empty() {
                    win.remove(&["session", "colorterm"]);
                } else {
                    win.set_str(&["session", "colorterm"], &colorterm);
                }
            }
            ui.end_row();

            ui.label("Max scrollback");
            let mut scrollback = win.config.session.max_scrollback as i64;
            if ui
                .add(
                    egui::DragValue::new(&mut scrollback)
                        .speed(100.0)
                        .range(0..=1_000_000),
                )
                .changed()
            {
                let value = scrollback.max(0);
                win.config.session.max_scrollback = value as usize;
                win.set_i64(&["session", "max-scrollback"], value);
            }
            ui.end_row();
        });
    ui.label(
        egui::RichText::new("Lines retained per pane. 0 disables scrollback.")
            .color(palette.muted)
            .size(12.0),
    );

    super::section(ui, palette, "ENVIRONMENT");
    ui.label(
        egui::RichText::new("Extra variables exported to every shell.")
            .color(palette.muted)
            .size(12.0),
    );
    ui.add_space(6.0);

    let mut env = win.config.session.env.clone();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (index, (name, value)) in env.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if ui
                .add_sized(
                    [200.0, 26.0],
                    egui::TextEdit::singleline(name)
                        .hint_text("NAME")
                        .vertical_align(egui::Align::Center),
                )
                .changed()
            {
                changed = true;
            }
            ui.label("=");
            if ui
                .add_sized(
                    [240.0, 26.0],
                    egui::TextEdit::singleline(value)
                        .hint_text("value")
                        .vertical_align(egui::Align::Center),
                )
                .changed()
            {
                changed = true;
            }
            if ui.small_button("✕").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        env.remove(index);
        changed = true;
    }
    ui.add_space(4.0);
    if ui.button("+ Add variable").clicked() {
        env.push((String::new(), String::new()));
        changed = true;
    }
    if changed {
        win.config.session.env = env.clone();
        // Skip rows without a name so a half-typed variable never breaks the reload.
        let valid: Vec<(String, String)> = env
            .into_iter()
            .filter(|(name, _)| !name.trim().is_empty())
            .collect();
        if valid.is_empty() {
            win.remove(&["session", "env"]);
        } else {
            win.set_env(&["session", "env"], &valid);
        }
    }

    super::section(ui, palette, "DIAGNOSTICS");
    egui::Grid::new("settings_diagnostics_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Stability trace");
            let mut trace = win
                .config
                .diagnostics
                .stability_trace
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            if text_field(ui, &mut trace, "path to write trace log").changed() {
                if trace.trim().is_empty() {
                    win.config.diagnostics.stability_trace = None;
                    win.remove(&["diagnostics", "stability-trace"]);
                } else {
                    win.config.diagnostics.stability_trace = Some(PathBuf::from(trace.clone()));
                    win.set_str(&["diagnostics", "stability-trace"], &trace);
                }
            }
            ui.end_row();
        });
    ui.label(
        egui::RichText::new(
            "Writes frame-timing diagnostics to this file. Leave empty to disable.",
        )
        .color(palette.muted)
        .size(12.0),
    );
}

fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str) -> egui::Response {
    ui.add_sized(
        [300.0, 26.0],
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .vertical_align(egui::Align::Center),
    )
}
