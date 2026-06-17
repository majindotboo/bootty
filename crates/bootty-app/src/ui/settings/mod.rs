//! Settings window: a real OS window (egui immediate viewport) for editing the user config.
//!
//! Edits are live-applied by writing the changed key straight into `config.toml`; the main
//! window's `ConfigHotReload` watcher then re-reads the file and applies it. The window keeps its
//! own working copy of [`BoottyConfig`] so it can render current values and so the "clear" buttons
//! know whether an override is set.

mod appearance;
mod font;
mod keybinds;
mod window;

use std::path::PathBuf;

use bootty_ui::{Theme, ThemePalette};
use eframe::egui::{self, Color32};

use crate::{
    color::Color,
    config::{
        BoottyConfig, ConfigDocument, ConfigResult, MacosTitlebarStyle,
        load_or_create_config_document,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    Font,
    Appearance,
    Window,
    Keybindings,
}

const VIEWPORT_KEY: &str = "bootty::settings";

/// Stable id for the settings viewport, shared by the renderer and the focus command.
pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of(VIEWPORT_KEY)
}

impl SettingsTab {
    const ALL: [(SettingsTab, &'static str); 4] = [
        (SettingsTab::Font, "Font"),
        (SettingsTab::Appearance, "Appearance"),
        (SettingsTab::Window, "Window"),
        (SettingsTab::Keybindings, "Keybindings"),
    ];
}

pub struct SettingsWindow {
    config: BoottyConfig,
    config_path: PathBuf,
    tab: SettingsTab,
    palette: ThemePalette,
    font_families: Option<Vec<String>>,
    theme_names: Option<Vec<String>>,
    /// Which keybind list is being edited (global, or one of the per-backend lists).
    keybind_scope: keybinds::KeybindScope,
    /// Editable rows for the loaded scope: the user layer that sits on top of the built-in defaults.
    keybind_rows: Option<Vec<keybinds::BindingRow>>,
    /// Whether the loaded scope drops the built-in defaults (the `clear` sentinel).
    keybind_clear: bool,
    /// The scope `keybind_rows`/`keybind_clear` were loaded for; reloaded when the scope changes.
    keybind_loaded_scope: Option<keybinds::KeybindScope>,
    /// In-progress chord capture, if any.
    keybind_capture: Option<keybinds::ChordCapture>,
    last_write_error: Option<String>,
}

impl SettingsWindow {
    pub fn new(config: BoottyConfig) -> Self {
        let config_path = config.config_path.clone();
        Self {
            config,
            config_path,
            tab: SettingsTab::default(),
            palette: ThemePalette::default(),
            font_families: None,
            theme_names: None,
            keybind_scope: keybinds::KeybindScope::Global,
            keybind_rows: None,
            keybind_clear: false,
            keybind_loaded_scope: None,
            keybind_capture: None,
            last_write_error: None,
        }
    }

    /// Render the settings viewport. Returns `false` once the window should close.
    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> bool {
        self.palette = theme.palette;
        let mut keep_open = true;
        // Match the app's dark chrome with a transparent titlebar unless the user explicitly chose
        // the native macOS titlebar.
        let custom_titlebar = self.config.window.macos_titlebar_style != MacosTitlebarStyle::Native;
        let viewport_id = viewport_id();
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Bootty Settings")
            .with_inner_size([780.0, 580.0])
            .with_min_inner_size([620.0, 460.0]);
        if custom_titlebar {
            builder = builder
                .with_title_shown(false)
                .with_titlebar_shown(false)
                .with_fullsize_content_view(true);
        }

        ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
            // Theme both the local ui (panels/widgets) and the context style: combo box and menu
            // popups open as separate areas that read the context style, not the local ui style.
            bootty_ui::configure_style(ui.style_mut(), theme);
            ui.ctx()
                .global_style_mut(|style| bootty_ui::configure_style(style, theme));

            let capturing = self.keybind_capture.is_some();
            let close_requested = ui.input(|input| input.viewport().close_requested());
            let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
            if close_requested || (escape && !capturing) {
                keep_open = false;
            }

            // Inset the top so the traffic-light buttons overlaid by the transparent titlebar do
            // not collide with the "Settings" heading.
            let rail_top = if custom_titlebar { 34 } else { 16 };
            egui::Panel::left("bootty_settings_rail")
                .exact_size(168.0)
                .resizable(false)
                .frame(
                    egui::Frame::NONE
                        .fill(self.palette.mantle)
                        .inner_margin(egui::Margin {
                            left: 10,
                            right: 10,
                            top: rail_top,
                            bottom: 16,
                        }),
                )
                .show_inside(ui, |ui| self.tab_rail(ui));

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(self.palette.base)
                        .inner_margin(egui::Margin::same(18)),
                )
                .show_inside(ui, |ui| {
                    if let Some(error) = self.last_write_error.clone() {
                        ui.colored_label(self.palette.destructive, format!("⚠ {error}"));
                        ui.add_space(6.0);
                    }
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match self.tab {
                            SettingsTab::Font => font::ui(self, ui),
                            SettingsTab::Appearance => appearance::ui(self, ui),
                            SettingsTab::Window => window::ui(self, ui),
                            SettingsTab::Keybindings => keybinds::ui(self, ui),
                        });
                });
        });

        keep_open
    }

    fn tab_rail(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Settings")
                .color(self.palette.text)
                .strong()
                .size(15.0),
        );
        ui.add_space(12.0);
        for (tab, label) in SettingsTab::ALL {
            let selected = self.tab == tab;
            // Selected rows sit on the light `primary` fill, so their text must be dark to read.
            let text = egui::RichText::new(label).color(if selected {
                self.palette.base
            } else {
                self.palette.subtext
            });
            let response = ui.add_sized(
                [ui.available_width(), 30.0],
                egui::Button::selectable(selected, text),
            );
            if response.clicked() {
                self.tab = tab;
                self.keybind_capture = None;
            }
        }
    }

    // --- config writeback -------------------------------------------------------------------

    fn write(&mut self, mutate: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>) {
        let result = (|| {
            let mut document = load_or_create_config_document(&self.config_path)?;
            mutate(&mut document)?;
            document.write_to_disk()
        })();
        self.last_write_error = result.err().map(|error| error.to_string());
    }

    fn set_f32(&mut self, path: &[&str], value: f32) {
        self.write(|document| {
            document.set_item(path, bootty_config::toml_edit::value(f64::from(value)))
        });
    }

    fn set_bool(&mut self, path: &[&str], value: bool) {
        self.write(|document| document.set_item(path, bootty_config::toml_edit::value(value)));
    }

    fn set_str(&mut self, path: &[&str], value: &str) {
        self.write(|document| document.set_item(path, bootty_config::toml_edit::value(value)));
    }

    fn set_color(&mut self, path: &[&str], rgb: [u8; 3]) {
        let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
        self.write(move |document| document.set_item(path, bootty_config::toml_edit::value(hex)));
    }

    fn set_strings(&mut self, path: &[&str], values: &[String]) {
        let mut array = bootty_config::toml_edit::Array::new();
        for value in values {
            array.push(value.as_str());
        }
        self.write(move |document| document.set_item(path, bootty_config::toml_edit::value(array)));
    }

    fn remove(&mut self, path: &[&str]) {
        self.write(|document| document.remove_item(path));
    }
}

/// A combo box whose dropdown has a search filter at the top. Returns the chosen option index.
///
/// Items render with the hover highlight (dark) rather than the light selection fill, so light
/// text stays readable; the current option is tinted with the accent instead of a filled row. The
/// popup uses `CloseOnClickOutside` so clicking into the filter field keeps it open.
fn searchable_combo(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    id_salt: &str,
    selected_text: &str,
    width: f32,
    options: &[&str],
    current: Option<usize>,
) -> Option<usize> {
    let filter_id = ui.make_persistent_id((id_salt, "filter"));
    let mut chosen = None;
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(width)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            let mut filter: String =
                ui.memory(|memory| memory.data.get_temp(filter_id).unwrap_or_default());
            let response = ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .hint_text("Search")
                    .vertical_align(egui::Align::Center)
                    .desired_width(width.max(160.0)),
            );
            // Keep focus on the filter so the user can type immediately on open.
            if !response.has_focus() {
                response.request_focus();
            }
            ui.memory_mut(|memory| memory.data.insert_temp(filter_id, filter.clone()));
            let needle = filter.to_ascii_lowercase();
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    for (index, option) in options.iter().enumerate() {
                        if !needle.is_empty() && !option.to_ascii_lowercase().contains(&needle) {
                            continue;
                        }
                        let color = if current == Some(index) {
                            palette.primary
                        } else {
                            palette.text
                        };
                        if ui
                            .selectable_label(false, egui::RichText::new(*option).color(color))
                            .clicked()
                        {
                            chosen = Some(index);
                            ui.memory_mut(|memory| memory.data.remove_temp::<String>(filter_id));
                            ui.close();
                        }
                    }
                });
        });
    chosen
}

/// Section heading inside a tab.
fn section(ui: &mut egui::Ui, palette: ThemePalette, title: &str) {
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(title)
            .color(palette.subtext)
            .strong()
            .size(12.0),
    );
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);
}

/// One label + color-button + clear row inside a `Grid`. `field` projects the override slot in the
/// working copy so the change is reflected immediately and the clear button knows whether to show.
fn color_row(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    label: &str,
    path: &[&str],
    seed: Color32,
    field: fn(&mut crate::config::ColorConfig) -> &mut Option<Color>,
) {
    ui.label(label);
    let current = *field(&mut win.config.colors);
    ui.horizontal(|ui| {
        let mut rgb = current.map_or([seed.r(), seed.g(), seed.b()], |color| {
            [color.r, color.g, color.b]
        });
        if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
            *field(&mut win.config.colors) = Some(Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
            });
            win.set_color(path, rgb);
        }
        if current.is_some() && ui.small_button("Reset").clicked() {
            *field(&mut win.config.colors) = None;
            win.remove(path);
        }
    });
    ui.end_row();
}
