use eframe::egui;

use super::SettingsWindow;
use crate::config::{MacosTitlebarStyle, WindowDecoration, WindowFullscreen};

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "WINDOW");
    super::settings_row(
        ui,
        palette,
        "Title",
        "Shown in native window chrome.",
        |ui| {
            let mut title = win.config.window.title.clone();
            if super::settings_text_edit(ui, palette, &mut title, "Bootty").changed() {
                win.config.window.title = title.clone();
                win.set_str(&["window", "title"], &title);
            }
        },
    );
    super::settings_row(
        ui,
        palette,
        "Titlebar style",
        "macOS window chrome treatment.",
        |ui| {
            let mut style = win.config.window.macos_titlebar_style;
            if enum_combo(
                ui,
                palette,
                "settings_titlebar",
                &mut style,
                &[
                    (MacosTitlebarStyle::Native, "System titlebar"),
                    (MacosTitlebarStyle::Transparent, "Transparent"),
                    (MacosTitlebarStyle::Hidden, "Hidden"),
                ],
            ) {
                win.config.window.macos_titlebar_style = style;
                win.set_str(&["window", "macos-titlebar-style"], titlebar_token(style));
            }
        },
    );
    super::settings_row(
        ui,
        palette,
        "Decoration",
        "Choose who draws the outer window border.",
        |ui| {
            let mut decoration = win.config.window.window_decoration;
            if super::described_combo(
                ui,
                palette,
                "settings_decoration",
                &mut decoration,
                &[
                    (
                        WindowDecoration::Auto,
                        "Automatic",
                        "Let the platform pick based on the titlebar style.",
                    ),
                    (
                        WindowDecoration::None,
                        "Borderless",
                        "No outer border or system window controls.",
                    ),
                    (
                        WindowDecoration::Client,
                        "Drawn by Bootty",
                        "Bootty paints the window border and controls.",
                    ),
                    (
                        WindowDecoration::Server,
                        "Drawn by system",
                        "The OS paints the native window border.",
                    ),
                ],
                super::ComboStyle {
                    width: 260.0,
                    searchable: false,
                    placeholder: "",
                },
            ) {
                win.config.window.window_decoration = decoration;
                win.set_str(
                    &["window", "window-decoration"],
                    decoration_token(decoration),
                );
            }
        },
    );
    super::settings_row(
        ui,
        palette,
        "Fullscreen mode",
        "Controls native fullscreen and notch-aware non-native modes.",
        |ui| {
            let mut fullscreen = win.config.window.fullscreen;
            if super::described_combo(
                ui,
                palette,
                "settings_fullscreen",
                &mut fullscreen,
                &[
                    (
                        WindowFullscreen::Disabled,
                        "Disabled",
                        "Never enter fullscreen.",
                    ),
                    (
                        WindowFullscreen::Native,
                        "Native",
                        "Use macOS native Spaces fullscreen.",
                    ),
                    (
                        WindowFullscreen::NonNative,
                        "Borderless",
                        "Fill the display without native Spaces.",
                    ),
                    (
                        WindowFullscreen::NonNativeVisibleMenu,
                        "Borderless + menu bar",
                        "Keep the menu bar visible in borderless fullscreen.",
                    ),
                    (
                        WindowFullscreen::NonNativePaddedNotch,
                        "Borderless + notch padding",
                        "Reserve space for a notched display.",
                    ),
                ],
                super::ComboStyle {
                    width: 260.0,
                    searchable: false,
                    placeholder: "",
                },
            ) {
                win.config.window.fullscreen = fullscreen;
                win.set_str(&["window", "fullscreen"], fullscreen_token(fullscreen));
            }
        },
    );

    super::section(ui, palette, "DEFAULT SIZE");
    numeric_window_row(
        win,
        ui,
        WindowNumberRow {
            label: "Width",
            help: "Applies to newly created windows.",
            path: ["window", "width"],
            range: 400.0..=6000.0,
            field: |window| &mut window.width,
        },
    );
    numeric_window_row(
        win,
        ui,
        WindowNumberRow {
            label: "Height",
            help: "Applies to newly created windows.",
            path: ["window", "height"],
            range: 300.0..=4000.0,
            field: |window| &mut window.height,
        },
    );

    super::section(ui, palette, "FULLSCREEN NOTCH");
    super::settings_toggle_row(
        ui,
        palette,
        "Tabs in notch band",
        "Allow terminal chrome to occupy the notch/menu-bar band.",
        win.config.window.fullscreen_tabs_in_notch,
        |enabled| {
            win.config.window.fullscreen_tabs_in_notch = enabled;
            win.set_bool(&["window", "fullscreen-tabs-in-notch"], enabled);
        },
    );
    super::settings_row(
        ui,
        palette,
        "Top offset",
        "Leave automatic unless a notched display needs an exact override.",
        |ui| {
            let mut auto = win.config.window.fullscreen_top_offset.is_none();
            if super::settings_toggle(ui, palette, &mut auto) && auto {
                win.config.window.fullscreen_top_offset = None;
                win.remove(&["window", "fullscreen-top-offset"]);
            }
            ui.label(egui::RichText::new("Auto").color(palette.muted));
            let mut offset = win.config.window.fullscreen_top_offset.unwrap_or(0.0);
            if ui
                .add_enabled(
                    !auto,
                    egui::DragValue::new(&mut offset)
                        .speed(1.0)
                        .range(0.0..=160.0)
                        .suffix(" px"),
                )
                .changed()
            {
                win.config.window.fullscreen_top_offset = Some(offset);
                win.set_f32(&["window", "fullscreen-top-offset"], offset);
            }
        },
    );

    super::section(ui, palette, "CHROME");
    chrome_slider(
        win,
        ui,
        ChromeSliderRow {
            label: "Chrome gap",
            help: "Spacing between sidebar, status, and terminal content.",
            path: ["chrome", "gap"],
            range: 0.0..=24.0,
            suffix: " px",
            field: |chrome| &mut chrome.gap,
        },
    );
    chrome_slider(
        win,
        ui,
        ChromeSliderRow {
            label: "Inactive sidebar dim",
            help: "Opacity reduction when the window is not focused.",
            path: ["chrome", "unfocused-sidebar-dim"],
            range: 0.0..=1.0,
            suffix: "",
            field: |chrome| &mut chrome.unfocused_sidebar_dim,
        },
    );
    chrome_slider(
        win,
        ui,
        ChromeSliderRow {
            label: "Inactive terminal dim",
            help: "Opacity reduction when the window is not focused.",
            path: ["chrome", "unfocused-terminal-dim"],
            range: 0.0..=1.0,
            suffix: "",
            field: |chrome| &mut chrome.unfocused_terminal_dim,
        },
    );
}

fn enum_combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    id: &str,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    let labels: Vec<&str> = options.iter().map(|(_, label)| *label).collect();
    let current_index = options.iter().position(|(value, _)| *value == *current);
    let Some(selected) = current_index else {
        return false;
    };
    let next = if labels.len() <= 5 {
        super::settings_segmented(ui, palette, &labels, selected)
    } else {
        super::searchable_combo(
            ui,
            palette,
            id,
            labels[selected],
            220.0,
            &labels,
            Some(selected),
        )
    };
    if let Some(index) = next {
        *current = options[index].0;
        return true;
    }
    false
}

struct WindowNumberRow {
    label: &'static str,
    help: &'static str,
    path: [&'static str; 2],
    range: std::ops::RangeInclusive<f32>,
    field: fn(&mut crate::config::WindowConfig) -> &mut f32,
}

fn numeric_window_row(win: &mut SettingsWindow, ui: &mut egui::Ui, spec: WindowNumberRow) {
    super::settings_row(ui, win.palette, spec.label, spec.help, |ui| {
        let mut value = *(spec.field)(&mut win.config.window);
        if ui
            .add(
                egui::DragValue::new(&mut value)
                    .speed(4.0)
                    .range(spec.range),
            )
            .changed()
        {
            *(spec.field)(&mut win.config.window) = value;
            win.set_f32(&spec.path, value);
        }
    });
}

struct ChromeSliderRow {
    label: &'static str,
    help: &'static str,
    path: [&'static str; 2],
    range: std::ops::RangeInclusive<f32>,
    suffix: &'static str,
    field: fn(&mut crate::config::ChromeConfig) -> &mut f32,
}

fn chrome_slider(win: &mut SettingsWindow, ui: &mut egui::Ui, spec: ChromeSliderRow) {
    super::settings_row(ui, win.palette, spec.label, spec.help, |ui| {
        let mut value = *(spec.field)(&mut win.config.chrome);
        if super::settings_slider(ui, win.palette, &mut value, spec.range) {
            *(spec.field)(&mut win.config.chrome) = value;
            win.set_f32(&spec.path, value);
        }
        super::settings_value_chip(ui, win.palette, &format!("{value:.0}{}", spec.suffix));
    });
}

fn titlebar_token(style: MacosTitlebarStyle) -> &'static str {
    match style {
        MacosTitlebarStyle::Native => "native",
        MacosTitlebarStyle::Transparent => "transparent",
        MacosTitlebarStyle::Hidden => "hidden",
    }
}

fn decoration_token(decoration: WindowDecoration) -> &'static str {
    match decoration {
        WindowDecoration::None => "none",
        WindowDecoration::Auto => "auto",
        WindowDecoration::Client => "client",
        WindowDecoration::Server => "server",
    }
}

fn fullscreen_token(fullscreen: WindowFullscreen) -> &'static str {
    match fullscreen {
        WindowFullscreen::Disabled => "disabled",
        WindowFullscreen::Native => "native",
        WindowFullscreen::NonNative => "non_native",
        WindowFullscreen::NonNativeVisibleMenu => "non_native_visible_menu",
        WindowFullscreen::NonNativePaddedNotch => "non_native_padded_notch",
    }
}
